use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;

use polars::prelude::{DataFrame, ParallelStrategy, ParquetReader, SerReader};

use super::admission;
use crate::resources::{ByteBudget, ByteReservation};
use crate::{Error, Result};

#[cfg(test)]
mod tests;

/// Immutable compressed bytes shared by admission and projected decoding.
pub struct AdmittedFile {
    bytes: Vec<u8>,
    // Fields drop in declaration order, so captured storage dies before its charge.
    reservation: Option<ByteReservation>,
}

/// Inline failure data; the budgeted path destroys rich errors before releasing scratch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionRefusal {
    Budget,
    Bound(&'static str),
    Io {
        kind: std::io::ErrorKind,
        raw_os_error: Option<i32>,
    },
}

impl AdmittedFile {
    /// Reads the file once, within the caller's compressed-byte allowance and admission's limits.
    /// The caller must separately bound concurrent readers and their decoded results.
    pub fn open(path: &Path, compressed_bytes_max: u64) -> Result<Self> {
        let (mut file, size) = open_file(path, compressed_bytes_max)?;
        Ok(Self {
            bytes: capture(&mut file, size)?,
            reservation: None,
        })
    }

    /// Reserves shared input and cursor-admission storage, excluding native decoding.
    /// The scratch envelope requires the supported compiler/target/dependency certificate.
    /// Success retains only the compressed charge; read's decoded output needs its own budget.
    pub fn open_budgeted(
        path: &Path,
        compressed_bytes_max: u64,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        let (mut file, size) = open_file(path, compressed_bytes_max).map_err(refuse)?;
        capture_budgeted(&mut file, size, budget)
    }

    /// Decodes named columns while retaining the admitted bytes for subsequent projections.
    /// Native decoding and projection allocations are not charged by `open_budgeted`.
    pub fn read(&self, columns: &[&str]) -> Result<DataFrame> {
        if let Some(reservation) = &self.reservation {
            assert_eq!(reservation.bytes(), self.bytes.capacity());
        }
        if columns.len() > admission::COLUMNS_MAX {
            return Err(Error::Bound("parquet projection columns"));
        }
        for (index, name) in columns.iter().enumerate() {
            if name.len() > admission::COLUMN_NAME_BYTES_MAX {
                return Err(Error::Bound("parquet projection name bytes"));
            }
            if columns[..index].contains(name) {
                return Err(Error::Bound("duplicate parquet projection column"));
            }
        }

        let columns = columns.iter().map(|name| (*name).to_owned()).collect();
        Ok(ParquetReader::new(Cursor::new(self.bytes.as_slice()))
            .set_rechunk(false)
            .with_slice(None)
            .with_columns(Some(columns))
            .with_projection(None)
            .read_parallel(ParallelStrategy::None)
            .with_row_index(None)
            .set_low_memory(true)
            .with_hive_partition_columns(None)
            .with_include_file_path(None)
            .finish()?)
    }
}

fn open_file(path: &Path, compressed_bytes_max: u64) -> Result<(File, u64)> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    if size > compressed_bytes_max.min(admission::FILE_BYTES_MAX) {
        return Err(Error::Bound("parquet file bytes"));
    }
    if size < 12 {
        return Err(Error::Bound("parquet file framing"));
    }

    Ok((file, size))
}

fn capture(file: &mut impl Read, size: u64) -> Result<Vec<u8>> {
    let length = usize::try_from(size).map_err(|_| Error::Bound("parquet file bytes"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(std::io::Error::other)?;
    bytes.resize(length, 0);
    file.read_exact(&mut bytes)?;
    if file.read(&mut [0])? != 0 {
        return Err(Error::Bound("changed parquet file size"));
    }

    admission::validate_reader(&mut Cursor::new(&bytes), size)?;
    Ok(bytes)
}

fn capture_budgeted(
    file: &mut impl Read,
    size: u64,
    budget: &Arc<ByteBudget>,
) -> std::result::Result<AdmittedFile, AdmissionRefusal> {
    let scratch = budget
        .reserve(admission::cursor_scratch_bytes().map_err(refuse)?)
        .ok_or(AdmissionRefusal::Budget)?;
    let length =
        usize::try_from(size).map_err(|_| AdmissionRefusal::Bound("parquet file bytes"))?;
    let input = budget.reserve(length).ok_or(AdmissionRefusal::Budget)?;

    let result = capture(file, size)
        .map_err(refuse)
        .map(|bytes| AdmittedFile {
            bytes,
            reservation: Some(input),
        });

    drop(scratch);

    result
}

fn refuse(error: Error) -> AdmissionRefusal {
    match error {
        Error::Bound(context) => AdmissionRefusal::Bound(context),
        Error::Io(error) => AdmissionRefusal::Io {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        },
        Error::Engine(_) => unreachable!("admission does not decode values"),
    }
}
