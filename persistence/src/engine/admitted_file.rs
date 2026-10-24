use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

use polars::prelude::{DataFrame, ParallelStrategy, ParquetReader, SerReader};

use super::admission;
use crate::{Error, Result};

/// Immutable compressed bytes shared by admission and projected decoding.
pub struct AdmittedFile {
    bytes: Vec<u8>,
}

impl AdmittedFile {
    /// Reads the file once, within the caller's compressed-byte allowance and admission's limits.
    /// The caller must separately bound concurrent readers and their decoded results.
    pub fn open(path: &Path, compressed_bytes_max: u64) -> Result<Self> {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        if size > compressed_bytes_max.min(admission::FILE_BYTES_MAX) {
            return Err(Error::Bound("parquet file bytes"));
        }
        if size < 12 {
            return Err(Error::Bound("parquet file framing"));
        }

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
        Ok(Self { bytes })
    }

    /// Decodes only the named columns from the bytes admission checked, with no path reopen.
    pub fn read(self, columns: &[&str]) -> Result<DataFrame> {
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
        Ok(ParquetReader::new(Cursor::new(self.bytes))
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
