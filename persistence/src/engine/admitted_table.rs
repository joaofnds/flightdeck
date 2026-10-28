use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::sync::Arc;

use super::{AdmissionRefusal, AdmittedFile, admission, admitted_file, native_metadata};
use crate::resources::{ByteBudget, ByteReservation};
use crate::{Error, Result};

/// An immutable admitted input and its charged column names, without native schema inference.
pub struct AdmittedTable {
    input: AdmittedFile,
    names: ColumnNames,
    rows: u64,
    metadata_bytes: usize,
    budget: Arc<ByteBudget>,
}

/// Separates a retryable shared-budget refusal from an ordinary native-read failure.
#[derive(Debug)]
pub enum AdmittedReadRefusal {
    Admission(AdmissionRefusal),
    Decode(Error),
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
}

const _: () = {
    assert!(size_of::<ColumnName>() == 129);
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
        self.names.contains(name)
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
        let reservation = self
            .budget
            .reserve(self.metadata_bytes)
            .ok_or(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))?;
        let result = self
            .input
            .read_prefix_with_metadata(columns, row_limit, |footer| {
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
        let footer = input.read_prefix_footer().map_err(admitted_file::refuse)?;
        assert!(footer.len() >= 8);
        let metadata_bytes = native_metadata::requested_bytes(&metadata, footer.len() - 8)
            .map_err(admitted_file::refuse)?;
        let rows = u64::try_from(metadata.num_rows)
            .map_err(|_| AdmissionRefusal::Bound("parquet file rows"))?;
        let names = ColumnNames::capture(&metadata.schema[1..], budget)?;
        drop(metadata);

        Ok(Self {
            input,
            names,
            rows,
            metadata_bytes,
            budget: Arc::clone(budget),
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

impl AdmittedColumns {
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}
