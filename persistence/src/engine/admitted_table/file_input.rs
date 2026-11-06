//! Validated descriptor-backed raw table, with no whole-file compressed capture.
use super::{
    AdmittedTable, ColumnNames, RawTableWork, admission, admitted_file, decode_profile,
    native_metadata,
};
use crate::engine::{AdmissionRefusal, AdmittedFile};
use crate::resources::{ByteBudget, ByteReservation};
use crate::{Error, Result};
use polars::prelude::{DataFrame, ParallelStrategy, ParquetReader, SerReader};
use polars_buffer::Buffer;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};

pub(super) struct FileInput {
    file: Mutex<File>,
    fingerprint: crate::engine::sources::Fingerprint,
    footer: Vec<u8>,
    bytes: u64,
    _reservation: ByteReservation,
}
pub(super) fn admit(
    mut file: File,
    budget: &Arc<ByteBudget>,
    work: Option<&dyn RawTableWork>,
    limits: Option<&super::DecodeLimits>,
) -> std::result::Result<AdmittedTable, AdmissionRefusal> {
    let scratch = budget
        .reserve(admission::cursor_scratch_bytes().map_err(admitted_file::refuse)? + 8192)
        .ok_or(AdmissionRefusal::Budget)?;
    let (table, fingerprint) = admitted_file::capture_verified(&mut file, |file| {
        let maximum = limits.map_or(admission::FILE_BYTES_MAX, |value| value.compressed_bytes);
        let size = admitted_file::file_size(file, maximum).map_err(admitted_file::refuse)?;
        let mut counted = std::io::BufReader::with_capacity(8192, CountedFile { file, work });
        counted
            .seek(SeekFrom::Start(0))
            .map_err(Error::from)
            .map_err(admitted_file::refuse)?;
        let metadata = admission::validate_reader_grouped_metadata(&mut counted, size)
            .map_err(admitted_file::refuse)?;
        if let Some(limits) = limits {
            admission::validate_decode_limits(&metadata, limits).map_err(admitted_file::refuse)?;
        }
        let (footer, reservation) = admit_footer(&mut counted, budget)?;
        let length = footer.len();
        let rows = u64::try_from(metadata.num_rows)
            .map_err(|_| AdmissionRefusal::Bound("parquet rows"))?;
        let types = admission::storage_types(&metadata).map_err(admitted_file::refuse)?;
        let names = ColumnNames::capture(&metadata.schema[1..], &types, budget)?;
        let profile = decode_profile::DecodeProfile::capture(&metadata, budget)?;
        let metadata_bytes =
            native_metadata::requested_bytes(&metadata, length).map_err(admitted_file::refuse)?;
        Ok((
            footer,
            reservation,
            rows,
            names,
            profile,
            metadata_bytes,
            size,
        ))
    })?;
    let (footer, reservation, rows, names, profile, metadata_bytes, bytes) = table;
    drop(scratch);
    Ok(AdmittedTable {
        input: AdmittedFile {
            bytes: Vec::new(),
            reservation: None,
        },
        file_input: Some(FileInput {
            file: Mutex::new(file),
            fingerprint,
            footer,
            bytes,
            _reservation: reservation,
        }),
        names,
        rows,
        metadata_bytes,
        budget: Arc::clone(budget),
        decode_profile: Some(profile),
        captured_file: Some(fingerprint),
    })
}
fn admit_footer(
    file: &mut (impl Read + Seek),
    budget: &Arc<ByteBudget>,
) -> std::result::Result<(Vec<u8>, ByteReservation), AdmissionRefusal> {
    file.seek(SeekFrom::End(-8))
        .map_err(Error::from)
        .map_err(admitted_file::refuse)?;
    let mut trailer = [0; 8];
    file.read_exact(&mut trailer)
        .map_err(Error::from)
        .map_err(admitted_file::refuse)?;
    let length = usize::try_from(u32::from_le_bytes(
        trailer[..4]
            .try_into()
            .map_err(|_| AdmissionRefusal::Bound("parquet footer"))?,
    ))
    .map_err(|_| AdmissionRefusal::Bound("parquet footer"))?;
    if length > 4 * 1024 * 1024 {
        return Err(AdmissionRefusal::Bound("parquet footer"));
    }

    let reservation = budget.reserve(length).ok_or(AdmissionRefusal::Budget)?;
    let mut footer = vec![0; length];
    file.seek(SeekFrom::End(
        -i64::try_from(length + 8).map_err(|_| AdmissionRefusal::Bound("parquet footer"))?,
    ))
    .map_err(Error::from)
    .map_err(admitted_file::refuse)?;
    file.read_exact(&mut footer)
        .map_err(Error::from)
        .map_err(admitted_file::refuse)?;
    Ok((footer, reservation))
}

impl FileInput {
    pub(super) fn read(
        &self,
        columns: &[&str],
        slice: Option<(usize, usize)>,
        work: Option<&dyn RawTableWork>,
    ) -> Result<DataFrame> {
        if let Some(work) = work {
            work.decode_attempt(self.bytes);
        }
        let file = self
            .file
            .lock()
            .map_err(|_| Error::Bound("poisoned raw table decoder"))?;
        self.check(&file)?;
        let mapping = ReadMapping::new(&file, self.bytes)?;
        let result = Buffer::with_slice(&self.footer, |footer| {
            let mut metadata = polars_parquet::parquet::read::deserialize_metadata(footer)
                .map_err(|error| {
                    drop(error);
                    Error::Bound("native parquet footer metadata")
                })?;
            if let Some(entries) = &mut metadata.key_value_metadata {
                entries.retain(|entry| entry.key == "ARROW:schema");
            }
            let slice = select_groups(&mut metadata, slice)?;
            let mut reader = ParquetReader::new(Cursor::new(mapping.bytes()))
                .set_rechunk(false)
                .with_slice(slice)
                .with_columns(Some(
                    columns.iter().map(|name| (*name).to_owned()).collect(),
                ))
                .read_parallel(ParallelStrategy::None)
                .with_row_index(None)
                .set_low_memory(true)
                .with_hive_partition_columns(None)
                .with_include_file_path(None);
            reader.set_metadata(Arc::new(metadata));
            reader.finish().map_err(Error::from)
        });
        let retirement = mapping.retire();
        self.check(&file)?;
        retirement?;
        result
    }
    fn check(&self, file: &File) -> Result<()> {
        if crate::engine::sources::Fingerprint::from_file(file)? != self.fingerprint {
            return Err(Error::Bound("changed parquet streaming source"));
        }

        Ok(())
    }
}

pub(super) struct CountedFile<'file> {
    pub(super) file: &'file mut File,
    pub(super) work: Option<&'file dyn RawTableWork>,
}
impl Read for CountedFile<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if let Some(work) = self.work {
            work.read_attempt(bytes.len());
        }
        self.file.read(bytes)
    }
}
impl Seek for CountedFile<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        if let Some(work) = self.work {
            work.seek_attempt();
        }
        self.file.seek(position)
    }
}

fn select_groups(
    metadata: &mut polars_parquet::parquet::metadata::FileMetadata,
    slice: Option<(usize, usize)>,
) -> Result<Option<(usize, usize)>> {
    let Some((offset, count)) = slice else {
        return Ok(None);
    };
    let end = offset
        .checked_add(count)
        .ok_or(Error::Bound("table slice"))?;
    let mut first = 0;
    let mut selected_first = None;
    metadata.row_groups.retain(|group| {
        let start = first;
        first += group.num_rows();
        let selected = start < end && first > offset && count > 0;
        if selected && selected_first.is_none() {
            selected_first = Some(start);
        }
        selected
    });
    metadata.num_rows = metadata
        .row_groups
        .iter()
        .map(polars_parquet::parquet::metadata::RowGroupMetadata::num_rows)
        .sum();
    metadata.max_row_group_height = metadata
        .row_groups
        .iter()
        .map(polars_parquet::parquet::metadata::RowGroupMetadata::num_rows)
        .max()
        .unwrap_or(0);
    Ok(Some((
        offset.saturating_sub(selected_first.unwrap_or(offset)),
        count,
    )))
}

const PROT_READ: i32 = 1;
const MAP_PRIVATE: i32 = 2;

struct ReadMapping {
    address: *mut std::ffi::c_void,
    length: usize,
}
impl ReadMapping {
    fn new(file: &File, bytes: u64) -> Result<Self> {
        #[cfg(test)]
        let bytes = MAP_TEST_EXTENT.with(std::cell::Cell::take).unwrap_or(bytes);
        let length = usize::try_from(bytes).map_err(|_| Error::Bound("table mapping"))?;
        let address = unsafe {
            mmap(
                std::ptr::null_mut(),
                length,
                PROT_READ,
                MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if address.addr() == usize::MAX {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { address, length })
    }
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.address.cast(), self.length) }
    }
    fn retire(mut self) -> Result<()> {
        let result = unsafe { munmap(self.address, self.length) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        self.length = 0;
        Ok(())
    }
}
impl Drop for ReadMapping {
    fn drop(&mut self) {
        if self.length > 0 {
            // Retire also on a decoder error before the explicit retirement point.
            let result = unsafe { munmap(self.address, self.length) };
            if result != 0 {
                collector::diagnostics::error(format_args!(
                    "table unmap: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
}

unsafe extern "C" {
    fn mmap(
        address: *mut std::ffi::c_void,
        length: usize,
        protection: i32,
        flags: i32,
        descriptor: i32,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    fn munmap(address: *mut std::ffi::c_void, length: usize) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_refusal_is_returned() {
        // Goal: FileInput returns a native mapping error and supports same-owner retry.
        // Method: fault only the mmap extent on one call; the admitted file remains unchanged.
        let fixture = crate::engine::source_tests::Fixture::new();
        let cleanup_path = fixture.path.clone();
        eprintln!("DECODER owned fixture={}", cleanup_path.display());
        let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
        let table = AdmittedTable::from_file_streaming(File::open(&fixture.path).unwrap(), &budget)
            .unwrap();
        MAP_TEST_EXTENT.with(|extent| extent.set(Some(u64::MAX)));
        let refused = table.read_slice_metadata_budgeted(&["counter"], 0, 1);
        assert!(matches!(
            refused,
            Err(super::super::AdmittedReadRefusal::Decode(Error::Io(_)))
        ));
        assert_eq!(
            table
                .read_slice_metadata_budgeted(&["counter"], 0, 1)
                .unwrap()
                .height(),
            1
        );
        drop((table, fixture));
        assert!(!cleanup_path.exists());
    }

    #[test]
    fn mapping_retirement_is_synchronous() {
        // Goal: mappings larger than the native async-unmap threshold retire before return.
        // Method: probe the exact released address after each of 32 real native mappings, in a
        // process of its own, since another test's mapping can land on a released address.
        if std::env::var_os("FLIGHTDECK_MAPPING_RETIREMENT_CHILD").is_none() {
            let test_name = concat!(module_path!(), "::mapping_retirement_is_synchronous")
                .strip_prefix("persistence::")
                .unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test_name, "--test-threads=1"])
                .env("FLIGHTDECK_MAPPING_RETIREMENT_CHILD", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        let fixture = crate::engine::source_tests::Fixture::new();
        let cleanup_path = fixture.path.clone();
        eprintln!("DECODER owned fixture={}", cleanup_path.display());
        let file = File::open(&fixture.path).unwrap();
        for _ in 0..32 {
            let mapping = ReadMapping::new(&file, 2 * 1024 * 1024).unwrap();
            let address = mapping.address;
            assert_eq!(&mapping.bytes()[..4], b"PAR1");
            mapping.retire().unwrap();
            let mut next = u64::try_from(address.addr()).unwrap();
            let mut length = 0_u64;
            let mut info = [0_i32; 32];
            let mut count = 32_u32;
            let mut object = 0_u32;
            let result = unsafe {
                mach_vm_region(
                    mach_task_self_,
                    &raw mut next,
                    &raw mut length,
                    9,
                    info.as_mut_ptr(),
                    &raw mut count,
                    &raw mut object,
                )
            };
            assert_eq!(result, 0);
            assert_ne!(next, u64::try_from(address.addr()).unwrap());
            if object != 0 {
                assert_eq!(unsafe { mach_port_deallocate(mach_task_self_, object) }, 0);
            }
        }
        drop((file, fixture));
        assert!(!cleanup_path.exists());
    }
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn mach_vm_region(
            task: u32,
            address: *mut u64,
            length: *mut u64,
            flavor: i32,
            info: *mut i32,
            count: *mut u32,
            object: *mut u32,
        ) -> i32;
        fn mach_port_deallocate(task: u32, name: u32) -> i32;
    }
}

#[cfg(test)]
thread_local! {
    static MAP_TEST_EXTENT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}
