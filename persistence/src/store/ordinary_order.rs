//! Physical catalog ordering across bounded decode chunks, independent of source membership.
use super::{CHUNK_ROWS, PublicationLedger, PublicationWork, invalid, rows, value_error};
use crate::engine::AdmittedTable;
use crate::resources::ByteReservation;
use polars::prelude::*;
use std::io;

pub(super) type FileKey = [i128; KEY_COLUMNS_MAX];
const KEY_COLUMNS_MAX: usize = 3;
const _: () = {
    let mut table = 0;
    while table < model::catalog::TABLES.len() {
        assert!(model::catalog::TABLES[table].key.len() <= KEY_COLUMNS_MAX);
        table += 1;
    }
    assert!(CHUNK_ROWS <= 64);
};

pub(super) fn validate_chunk(
    input: &AdmittedTable,
    table: crate::schema::Table,
    offset: u64,
    previous: Option<&FileKey>,
    ledger: &PublicationLedger,
    work: &mut PublicationWork,
) -> io::Result<(u32, FileKey)> {
    let requested =
        u32::try_from((input.row_count() - offset).min(collector::u64_from_usize(CHUNK_ROWS)))
            .map_err(|_| invalid())?;
    work.proof_operations += 1;
    let count = input
        .bounded_slice_count(offset, requested)
        .map_err(rows::admission_error)?;
    let names = table.catalog().key;
    let mut reservations: [Option<ByteReservation>; KEY_COLUMNS_MAX] =
        std::array::from_fn(|_| None);
    for (position, name) in names.iter().enumerate() {
        reservations[position] = Some(
            input
                .reserve_column_work(offset, count, name)
                .map_err(rows::admission_error)?,
        );
    }
    let frame = input
        .read_slice_counted(names, offset, count, ledger)
        .map_err(rows::read_error)?;
    if frame.height() != usize::try_from(count).map_err(|_| invalid())? {
        return Err(invalid());
    }

    let key = validate_chunk_keys(&frame, names, previous, work)?;
    Ok((count, key))
}

fn validate_chunk_keys(
    frame: &DataFrame,
    names: &[&str],
    previous: Option<&FileKey>,
    work: &mut PublicationWork,
) -> io::Result<FileKey> {
    assert!(frame.height() <= CHUNK_ROWS);
    assert!(frame.width() <= KEY_COLUMNS_MAX);
    let mut last = previous.copied();
    for row in 0..frame.height() {
        let mut key = [0; KEY_COLUMNS_MAX];
        for (position, name) in names.iter().enumerate() {
            let column = frame.column(name).map_err(value_error)?;
            work.proof_operations += 1;
            key[position] = match column.get(row).map_err(value_error)? {
                AnyValue::UInt8(value) => i128::from(value),
                AnyValue::UInt16(value) => i128::from(value),
                AnyValue::UInt32(value) => i128::from(value),
                AnyValue::UInt64(value) => i128::from(value),
                AnyValue::Int32(value) => i128::from(value),
                AnyValue::Int64(value) => i128::from(value),
                // The wider sentinel puts NULL after every persisted integer key.
                AnyValue::Null => i128::MAX,
                _ => return Err(invalid()),
            };
        }
        work.proof_operations += 1;
        if last.as_ref().is_some_and(|previous| previous > &key) {
            return Err(invalid());
        }

        last = Some(key);
    }
    last.ok_or_else(invalid)
}
