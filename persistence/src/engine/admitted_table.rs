use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::sync::Arc;

use super::{AdmissionRefusal, AdmittedFile, admission, admitted_file};
use crate::resources::{ByteBudget, ByteReservation};
use crate::{Error, Result};

/// An immutable admitted input and its charged column names, without native schema inference.
pub struct AdmittedTable {
    input: AdmittedFile,
    names: ColumnNames,
}

struct ColumnNames {
    values: Vec<ColumnName>,
    reservation: ByteReservation,
}

struct ColumnName {
    bytes: [u8; admission::COLUMN_NAME_BYTES_MAX],
    length: u8,
}

const _: () = {
    assert!(size_of::<ColumnName>() == 129);
    assert!(align_of::<ColumnName>() == 1);
};

#[cfg(test)]
mod tests;

impl AdmittedTable {
    /// Reserves before seeking or capturing. Native reads/results need separate accounting.
    pub fn from_file_budgeted(
        file: File,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        let size = admitted_file::file_size(&file, admission::FILE_BYTES_MAX)
            .map_err(admitted_file::refuse)?;
        Self::from_reader_budgeted(file, size, budget)
    }

    fn from_reader_budgeted(
        file: impl Read + Seek,
        size: u64,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        let scratch = budget
            .reserve(admission::cursor_scratch_bytes().map_err(admitted_file::refuse)?)
            .ok_or(AdmissionRefusal::Budget)?;
        let length =
            usize::try_from(size).map_err(|_| AdmissionRefusal::Bound("parquet file bytes"))?;
        let input = budget.reserve(length).ok_or(AdmissionRefusal::Budget)?;

        let result = Self::from_reader_capture(file, size, input, budget);
        drop(scratch);

        result
    }

    #[must_use]
    pub fn contains_column(&self, name: &str) -> bool {
        assert_eq!(
            self.names.reservation.bytes(),
            self.names.values.capacity() * size_of::<ColumnName>()
        );
        self.names
            .values
            .iter()
            .any(|column| column.bytes[..usize::from(column.length)] == *name.as_bytes())
    }

    /// Applies a prefix before native decoding; zero offset cannot overflow its slice extent.
    pub fn read(
        &self,
        columns: &[&str],
        row_limit: Option<u32>,
    ) -> Result<polars::prelude::DataFrame> {
        self.input.read_prefix(columns, row_limit)
    }

    fn from_reader_capture(
        mut file: impl Read + Seek,
        size: u64,
        input: ByteReservation,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        file.rewind()
            .map_err(Error::from)
            .map_err(admitted_file::refuse)?;
        let bytes = admitted_file::capture_bytes(&mut file, size).map_err(admitted_file::refuse)?;
        let metadata = admission::validate_reader_metadata(&mut Cursor::new(&bytes), size)
            .map_err(admitted_file::refuse)?;
        let names = ColumnNames::capture(&metadata.schema[1..], budget)?;
        drop(metadata);

        Ok(Self {
            input: AdmittedFile {
                bytes,
                reservation: Some(input),
            },
            names,
        })
    }
}

impl ColumnNames {
    fn capture(
        schema: &[polars_parquet_format::SchemaElement],
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmissionRefusal> {
        assert!(schema.len() <= admission::COLUMNS_MAX);
        let length = schema
            .len()
            .checked_mul(size_of::<ColumnName>())
            .ok_or(AdmissionRefusal::Bound("parquet column names"))?;
        let reservation = budget.reserve(length).ok_or(AdmissionRefusal::Budget)?;

        let values = Self::capture_values(schema).map_err(admitted_file::refuse)?;

        Ok(Self {
            values,
            reservation,
        })
    }

    fn capture_values(schema: &[polars_parquet_format::SchemaElement]) -> Result<Vec<ColumnName>> {
        let mut names = Vec::new();
        names
            .try_reserve_exact(schema.len())
            .map_err(std::io::Error::other)?;
        for field in schema {
            assert!(field.name.len() <= admission::COLUMN_NAME_BYTES_MAX);
            let mut name = ColumnName {
                bytes: [0; admission::COLUMN_NAME_BYTES_MAX],
                length: u8::try_from(field.name.len())
                    .map_err(|_| Error::Bound("parquet column name"))?,
            };
            name.bytes[..field.name.len()].copy_from_slice(field.name.as_bytes());
            names.push(name);
        }
        assert_eq!(names.len(), names.capacity());

        Ok(names)
    }
}
