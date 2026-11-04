//! Bounded metric comparison computations; the consuming owner commits their results.
use super::{CHUNK_ROWS, PublicationLedger, PublicationWork, invalid, rows, value_error};
use crate::engine::AdmittedTable;
use polars::prelude::*;
use std::io;

pub(super) struct Comparison<'input> {
    pub input: &'input AdmittedTable,
    pub expected: &'input DataFrame,
    pub observations: &'input [crate::Observation],
    pub publication: u64,
    pub offset: u64,
    pub matched: usize,
    pub expected_first: usize,
    pub ledger: &'input PublicationLedger,
}
impl Comparison<'_> {
    pub(super) fn count(&self) -> io::Result<u32> {
        let remaining = (self.expected_first + self.expected.height()).saturating_sub(self.matched);
        let chunk = if remaining == 0 {
            CHUNK_ROWS
        } else {
            remaining.min(CHUNK_ROWS)
        };
        let requested = u32::try_from(
            (self.input.row_count() - self.offset).min(collector::u64_from_usize(chunk)),
        )
        .map_err(|_| invalid())?;
        self.input
            .bounded_slice_count(self.offset, requested)
            .map_err(rows::admission_error)
    }
    fn columns(&self) -> io::Result<Vec<&str>> {
        validate_schema(self.input, self.expected)?;
        let columns: Vec<_> = self
            .expected
            .get_column_names()
            .into_iter()
            .map(PlSmallStr::as_str)
            .collect();
        Ok(columns)
    }
    pub(super) fn full(&self, count: u32, work: &mut PublicationWork) -> io::Result<usize> {
        work.proof_operations += 1;
        let columns = self.columns()?;
        let _workspace = self
            .input
            .reserve_slice_work(self.offset, count)
            .map_err(rows::admission_error)?;
        let actual = self
            .input
            .read_slice_counted(&columns, self.offset, count, self.ledger)
            .map_err(rows::read_error)?;
        if actual.height() != usize::try_from(count).map_err(|_| invalid())? {
            return Err(invalid());
        }

        let matches = self.positions(&actual, work)?;
        work.proof_operations += collector::u64_from_usize(matches.len() * columns.len());
        for &(actual_row, expected_row) in &matches {
            for (actual, expected) in actual.columns().iter().zip(self.expected.columns()) {
                compare_value(actual, actual_row, expected, expected_row)?;
            }
        }
        Ok(self.matched + matches.len())
    }
    pub(super) fn select(
        &self,
        count: u32,
        work: &mut PublicationWork,
    ) -> io::Result<Vec<(usize, usize)>> {
        work.proof_operations += 1;
        self.columns()?;
        let _workspace = self
            .input
            .reserve_column_work(self.offset, count, "publication")
            .map_err(rows::admission_error)?;
        let actual = self
            .input
            .read_slice_counted(&["publication"], self.offset, count, self.ledger)
            .map_err(rows::read_error)?;
        if actual.height() != usize::try_from(count).map_err(|_| invalid())? {
            return Err(invalid());
        }

        self.positions(&actual, work)
    }
    fn positions(
        &self,
        actual: &DataFrame,
        work: &mut PublicationWork,
    ) -> io::Result<Vec<(usize, usize)>> {
        let publications = actual
            .column("publication")
            .map_err(value_error)?
            .u64()
            .map_err(value_error)?;
        let mut matches = Vec::with_capacity(CHUNK_ROWS);
        for row in 0..actual.height() {
            work.proof_operations += 1;
            let number = publications.get(row).ok_or_else(invalid)?;
            let member = self.observations.binary_search_by(|observation| {
                work.proof_operations += 1;
                observation.publication.cmp(&number)
            });
            if member.is_err() {
                return Err(invalid());
            }

            if number == self.publication {
                let expected_row = self.matched + matches.len();
                if expected_row >= self.expected_first + self.expected.height() {
                    return Err(invalid());
                }

                matches.push((row, expected_row - self.expected_first));
            }
        }
        Ok(matches)
    }
    pub(super) fn column(
        &self,
        column: usize,
        count: u32,
        matches: &[(usize, usize)],
        work: &mut PublicationWork,
    ) -> io::Result<()> {
        work.proof_operations += 1;
        let columns = self.columns()?;
        let name = columns[column];
        let _workspace = self
            .input
            .reserve_column_work(self.offset, count, name)
            .map_err(rows::admission_error)?;
        let actual = self
            .input
            .read_slice_counted(&[name], self.offset, count, self.ledger)
            .map_err(rows::read_error)?;
        let actual = actual.column(name).map_err(value_error)?;
        let expected = self.expected.column(name).map_err(value_error)?;
        work.proof_operations += collector::u64_from_usize(matches.len());
        for &(actual_row, expected_row) in matches {
            compare_value(actual, actual_row, expected, expected_row)?;
        }
        Ok(())
    }
}
pub(super) fn validate_schema(input: &AdmittedTable, expected: &DataFrame) -> io::Result<()> {
    if input.fields().count() != expected.width() {
        return Err(invalid());
    }

    for (name, storage) in input.fields() {
        let column = expected.column(name).map_err(value_error)?;
        if column.dtype() != &storage {
            return Err(invalid());
        }
    }
    Ok(())
}
fn compare_value(
    actual: &Column,
    actual_row: usize,
    expected: &Column,
    expected_row: usize,
) -> io::Result<()> {
    if actual.dtype() != expected.dtype() {
        return Err(invalid());
    }

    let actual = actual.get(actual_row).map_err(value_error)?;
    let expected = expected.get(expected_row).map_err(value_error)?;
    let equal = match (&actual, &expected) {
        (AnyValue::Float32(a), AnyValue::Float32(b)) => a.to_bits() == b.to_bits(),
        (AnyValue::Float64(a), AnyValue::Float64(b)) => a.to_bits() == b.to_bits(),
        _ => actual == expected,
    };
    if equal { Ok(()) } else { Err(invalid()) }
}
