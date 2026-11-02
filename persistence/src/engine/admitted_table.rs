use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::sync::Arc;

use super::sources::Fingerprint;
use super::{AdmissionRefusal, AdmittedFile, admission, admitted_file, native_metadata};
use crate::resources::{ByteBudget, ByteReservation};
use crate::{Error, Result};

mod decode_profile;

/// An immutable admitted input and its charged column names, without native schema inference.
pub struct AdmittedTable {
    input: AdmittedFile,
    names: ColumnNames,
    rows: u64,
    metadata_bytes: usize,
    budget: Arc<ByteBudget>,
    decode_profile: Option<decode_profile::DecodeProfile>,
    captured_file: Option<Fingerprint>,
}

/// Separates a retryable shared-budget refusal from an ordinary native-read failure.
#[derive(Debug)]
pub enum AdmittedReadRefusal {
    Admission(AdmissionRefusal),
    Decode(Error),
}

impl From<Error> for AdmittedReadRefusal {
    fn from(error: Error) -> Self {
        Self::Decode(error)
    }
}

impl From<std::io::Error> for AdmittedReadRefusal {
    fn from(error: std::io::Error) -> Self {
        Self::Decode(Error::Io(error))
    }
}

/// Admission-derived names whose backing cannot escape its reservation.
pub struct AdmittedColumns {
    names: ColumnNames,
}

struct ColumnNames {
    values: Vec<ColumnName>,
    reservation: ByteReservation,
}

struct ColumnName {
    bytes: [u8; admission::COLUMN_NAME_BYTES_MAX],
    length: u8,
    storage: admission::Storage,
}

/// Additional caller limits checked on admitted immutable bytes before native metadata/decode.
/// Existing format bounds and their allocation certificates always apply first.
#[derive(Clone, Copy)]
pub struct DecodeLimits {
    pub compressed_bytes: u64,
    pub expanded_bytes: u64,
    pub rows: u64,
    pub row_groups: usize,
    pub columns: usize,
}

const _: () = {
    assert!(size_of::<ColumnName>() == 130);
    assert!(align_of::<ColumnName>() == 1);
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod buffer_tests;

#[cfg(test)]
mod metadata_tests;

impl AdmittedTable {
    /// Moves compressed storage into a shared native source without copying it.
    /// Native bridge wrappers, plans and decoding need separate accounting.
    pub fn into_scan(self) -> Result<(polars::prelude::LazyFrame, AdmittedColumns)> {
        let Self { input, names, .. } = self;
        let plan = super::scan::scan_sources(super::scan::buffer_sources(input))?;

        Ok((plan, AdmittedColumns { names }))
    }

    /// Reserves before seeking or capturing. Native reads/results need separate accounting.
    pub fn from_file_budgeted(
        file: File,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        Self::from_file_limited(file, budget, None, false)
    }

    pub fn from_file_budgeted_with_limits(
        file: File,
        budget: &Arc<ByteBudget>,
        limits: &DecodeLimits,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        Self::from_file_limited(file, budget, Some(limits), false)
    }

    /// Captures validated row-group work bounds for an explicitly budgeted legacy decode.
    pub(crate) fn from_file_budgeted_profiled(
        file: File,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        Self::from_file_limited(file, budget, None, true)
    }

    fn from_file_limited(
        mut file: File,
        budget: &Arc<ByteBudget>,
        limits: Option<&DecodeLimits>,
        profile: bool,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        let (mut input, captured_file) = admitted_file::capture_verified(&mut file, |file| {
            let maximum =
                limits.map_or(admission::FILE_BYTES_MAX, |limits| limits.compressed_bytes);
            let size = admitted_file::file_size(file, maximum).map_err(admitted_file::refuse)?;
            Self::from_reader_limited(file, size, budget, limits, profile)
        })?;
        input.captured_file = Some(captured_file);

        Ok(input)
    }

    pub(super) fn captured_file(&self) -> Option<&Fingerprint> {
        self.captured_file.as_ref()
    }

    pub(crate) fn uses_budget(&self, budget: &Arc<ByteBudget>) -> bool {
        Arc::ptr_eq(&self.budget, budget)
    }

    pub(crate) fn prepare_event_input(
        &mut self,
        limits: Option<&DecodeLimits>,
    ) -> std::result::Result<(), AdmissionRefusal> {
        let _scratch = self
            .budget
            .reserve(admission::cursor_scratch_bytes().map_err(admitted_file::refuse)?)
            .ok_or(AdmissionRefusal::Budget)?;
        let size = collector::u64_from_usize(self.input.bytes.len());
        if limits.is_some_and(|limits| size > limits.compressed_bytes) {
            return Err(AdmissionRefusal::Bound("parquet file bytes"));
        }

        let metadata =
            admission::validate_reader_metadata(&mut Cursor::new(&self.input.bytes), size)
                .map_err(admitted_file::refuse)?;
        if let Some(limits) = limits {
            admission::validate_decode_limits(&metadata, limits).map_err(admitted_file::refuse)?;
        } else if self.decode_profile.is_none() {
            self.decode_profile = Some(decode_profile::DecodeProfile::capture(
                &metadata,
                &self.budget,
            )?);
        }
        Ok(())
    }

    pub(crate) fn reserve_slice_work(
        &self,
        offset: u64,
        count: u32,
    ) -> std::result::Result<ByteReservation, AdmissionRefusal> {
        self.decode_profile
            .as_ref()
            .ok_or(AdmissionRefusal::Bound("missing parquet decode profile"))?
            .reserve(offset, count, &self.budget)
    }

    pub(crate) fn slice_work_shape(
        &self,
        offset: u64,
        count: u32,
    ) -> std::result::Result<(usize, usize), AdmissionRefusal> {
        self.decode_profile
            .as_ref()
            .ok_or(AdmissionRefusal::Bound("missing parquet decode profile"))?
            .work(offset, count)
    }

    pub(crate) fn reserve_native_metadata(
        &self,
    ) -> std::result::Result<ByteReservation, AdmissionRefusal> {
        self.budget
            .reserve(self.metadata_bytes)
            .ok_or(AdmissionRefusal::Budget)
    }

    #[cfg(test)]
    fn from_reader_budgeted(
        file: impl Read + Seek,
        size: u64,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        Self::from_reader_limited(file, size, budget, None, false)
    }

    fn from_reader_limited(
        file: impl Read + Seek,
        size: u64,
        budget: &Arc<ByteBudget>,
        limits: Option<&DecodeLimits>,
        profile: bool,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        let scratch = budget
            .reserve(admission::cursor_scratch_bytes().map_err(admitted_file::refuse)?)
            .ok_or(AdmissionRefusal::Budget)?;
        let length =
            usize::try_from(size).map_err(|_| AdmissionRefusal::Bound("parquet file bytes"))?;
        let input = budget.reserve(length).ok_or(AdmissionRefusal::Budget)?;

        let result = Self::from_reader_capture(file, size, input, budget, limits, profile);
        drop(scratch);

        result
    }

    #[must_use]
    pub fn contains_column(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub(crate) fn fields(&self) -> impl Iterator<Item = (&str, polars::prelude::DataType)> + Clone {
        self.names.values.iter().map(|column| {
            let name = match std::str::from_utf8(&column.bytes[..usize::from(column.length)]) {
                Ok(name) => name,
                Err(error) => unreachable!("captured Rust string column name: {error}"),
            };
            (name, column.storage.data_type())
        })
    }

    /// The captured footer's row count, validated against the sum of all row groups.
    #[must_use]
    pub fn row_count(&self) -> u64 {
        self.rows
    }

    /// Applies a prefix before native decoding; zero offset cannot overflow its slice extent.
    pub fn read(
        &self,
        columns: &[&str],
        row_limit: Option<u32>,
    ) -> Result<polars::prelude::DataFrame> {
        self.input.read_prefix(columns, row_limit)
    }

    /// Reserves native footer-metadata construction before entering its borrowed buffer scope.
    /// The lease covers metadata through destruction, including inline metadata refusals.
    /// Arrow inference, projection, value decoding and returned results need separate bounds.
    pub fn read_metadata_budgeted(
        &self,
        columns: &[&str],
        row_limit: Option<u32>,
    ) -> std::result::Result<polars::prelude::DataFrame, AdmittedReadRefusal> {
        let slice = row_limit
            .map(usize::try_from)
            .transpose()
            .map_err(|_| AdmittedReadRefusal::Decode(Error::Bound("parquet prefix rows")))?
            .map(|length| (0, length));
        self.read_metadata_budgeted_slice(columns, slice)
    }

    /// A bounded physical slice of a legacy file; native metadata is charged as for prefixes.
    pub fn read_slice_metadata_budgeted(
        &self,
        columns: &[&str],
        offset: u64,
        count: u32,
    ) -> std::result::Result<polars::prelude::DataFrame, AdmittedReadRefusal> {
        let offset = usize::try_from(offset)
            .map_err(|_| AdmittedReadRefusal::Decode(Error::Bound("parquet row offset")))?;
        let count = usize::try_from(count)
            .map_err(|_| AdmittedReadRefusal::Decode(Error::Bound("parquet slice rows")))?;
        self.read_metadata_budgeted_slice(columns, Some((offset, count)))
    }

    fn read_metadata_budgeted_slice(
        &self,
        columns: &[&str],
        slice: Option<(usize, usize)>,
    ) -> std::result::Result<polars::prelude::DataFrame, AdmittedReadRefusal> {
        let reservation = self
            .reserve_native_metadata()
            .map_err(AdmittedReadRefusal::Admission)?;
        let result = self
            .input
            .read_slice_with_metadata(columns, slice, |footer| {
                polars_parquet::parquet::read::deserialize_metadata(footer).map_err(|error| {
                    drop(error);
                    Error::Bound("native parquet footer metadata")
                })
            });
        drop(reservation);
        result.map_err(AdmittedReadRefusal::Decode)
    }

    fn from_reader_capture(
        mut file: impl Read + Seek,
        size: u64,
        input: ByteReservation,
        budget: &Arc<ByteBudget>,
        limits: Option<&DecodeLimits>,
        profile: bool,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        file.rewind()
            .map_err(Error::from)
            .map_err(admitted_file::refuse)?;
        let bytes = admitted_file::capture_bytes(&mut file, size).map_err(admitted_file::refuse)?;
        let input = AdmittedFile {
            bytes,
            reservation: Some(input),
        };
        let metadata = admission::validate_reader_metadata(&mut Cursor::new(&input.bytes), size)
            .map_err(admitted_file::refuse)?;
        if let Some(limits) = limits {
            admission::validate_decode_limits(&metadata, limits).map_err(admitted_file::refuse)?;
        }
        let footer = input.read_prefix_footer().map_err(admitted_file::refuse)?;
        assert!(footer.len() >= 8);
        let metadata_bytes = native_metadata::requested_bytes(&metadata, footer.len() - 8)
            .map_err(admitted_file::refuse)?;
        let rows = u64::try_from(metadata.num_rows)
            .map_err(|_| AdmissionRefusal::Bound("parquet file rows"))?;
        let types = admission::storage_types(&metadata).map_err(admitted_file::refuse)?;
        let names = ColumnNames::capture(&metadata.schema[1..], &types, budget)?;
        let decode_profile = if profile {
            Some(decode_profile::DecodeProfile::capture(&metadata, budget)?)
        } else {
            None
        };
        drop(metadata);

        Ok(Self {
            input,
            names,
            rows,
            metadata_bytes,
            budget: Arc::clone(budget),
            decode_profile,
            captured_file: None,
        })
    }
}

impl ColumnNames {
    fn contains(&self, name: &str) -> bool {
        assert_eq!(
            self.reservation.bytes(),
            self.values.capacity() * size_of::<ColumnName>()
        );

        self.values
            .iter()
            .any(|column| column.bytes[..usize::from(column.length)] == *name.as_bytes())
    }

    fn capture(
        schema: &[polars_parquet_format::SchemaElement],
        types: &[admission::Storage],
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        assert!(schema.len() <= admission::COLUMNS_MAX);
        let length = schema
            .len()
            .checked_mul(size_of::<ColumnName>())
            .ok_or(AdmissionRefusal::Bound("parquet column names"))?;
        let reservation = budget.reserve(length).ok_or(AdmissionRefusal::Budget)?;

        let values = Self::capture_values(schema, types).map_err(admitted_file::refuse)?;

        Ok(Self {
            values,
            reservation,
        })
    }

    fn capture_values(
        schema: &[polars_parquet_format::SchemaElement],
        types: &[admission::Storage],
    ) -> Result<Vec<ColumnName>> {
        assert!(types.len() >= schema.len());
        let mut names = Vec::new();
        names
            .try_reserve_exact(schema.len())
            .map_err(std::io::Error::other)?;
        for (index, field) in schema.iter().enumerate() {
            assert!(field.name.len() <= admission::COLUMN_NAME_BYTES_MAX);
            let mut name = ColumnName {
                bytes: [0; admission::COLUMN_NAME_BYTES_MAX],
                length: u8::try_from(field.name.len())
                    .map_err(|_| Error::Bound("parquet column name"))?,
                storage: types[index],
            };
            name.bytes[..field.name.len()].copy_from_slice(field.name.as_bytes());
            names.push(name);
        }
        assert_eq!(names.len(), names.capacity());

        Ok(names)
    }
}

impl AdmittedColumns {
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}
