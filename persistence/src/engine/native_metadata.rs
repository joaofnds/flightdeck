//! Requested heap layouts for borrowed-footer native Parquet metadata construction.

use std::alloc::Layout;

use polars_parquet::parquet::metadata::{
    ColumnChunkMetadata, ColumnDescriptor, FileMetadata, RowGroupMetadata, ThriftFileMetadata,
};
use polars_parquet::parquet::schema::types::ParquetType;

use crate::{Error, Result};

/// Only `Buffer::with_slice` and `parquet::read::deserialize_metadata` are covered.
/// Admission must have accepted these exact immutable footer bytes. Acquire the returned
/// charge before constructing the Buffer, and retain it until metadata and errors drop.
/// This excludes Arrow inference, projection, decoding, Polars errors and allocator overhead.
pub(super) fn requested_bytes(metadata: &ThriftFileMetadata, footer_bytes: usize) -> Result<usize> {
    let columns = metadata.schema.len().checked_sub(1).ok_or_else(bound)?;
    if columns > super::admission::COLUMNS_MAX {
        return Err(bound());
    }

    let root = metadata.schema.first().ok_or_else(bound)?;
    let names = metadata.schema[1..]
        .iter()
        .try_fold(0usize, |total, field| {
            total
                .checked_add(requested_bytes_name(field.name.len()))
                .ok_or_else(bound)
        })?;
    let descriptor = requested_bytes_sum(&[
        requested_bytes_product(640, requested_bytes_vector(columns)?)?,
        requested_bytes_product(384, columns)?,
        64 + 512 + 40,
        requested_bytes_name(root.name.len()),
        requested_bytes_product(4, names)?,
    ])?;
    let groups = metadata.row_groups.len();
    let group = requested_bytes_sum(&[
        requested_bytes_product(256, requested_bytes_vector(columns)?)?,
        40,
        requested_bytes_lookup(columns)?,
        requested_bytes_product(128, columns)?,
        names,
    ])?;
    let bytes = requested_bytes_sum(&[
        // Every native copied string, including replaced values, fits the footer ledger.
        super::admission::VECTOR_BYTES_MAX,
        footer_bytes,
        // Raw ColumnOrder is zero-sized; each native one-byte tag consumes a footer byte.
        // Exact-capacity lists, including replaced lists, therefore request at most F.
        footer_bytes,
        descriptor,
        requested_bytes_product(128, requested_bytes_vector(groups)?)?,
        requested_bytes_product(groups, group)?,
        requested_bytes_product(8, columns)?,
        // Optional file Arc, borrowed Buffer owner, ordinary errors, cold foldhash entropy.
        256 + 48 + 4096 + 1,
    ])?;
    Layout::from_size_align(bytes, 16).map_err(|_| bound())?;

    Ok(bytes)
}

// Polars 22a147d: schema/io_thrift/from_thrift.rs grows the fields and root vectors;
// metadata/schema_descriptor.rs grows leaves, with four leaf-name copies and one root.
// Each flat leaf needs at most 384 path bytes; one traversal scratch needs 64 bytes.
// metadata/row_metadata.rs builds each columns Vec, Arc<Vec<_>>, and pre-sized lookup.
// The source-confirmed public element ceilings are held below and by the CI certificates.
// Native errors in handwritten_thrift, schema/io_thrift, schema/types and parquet_bridge
// contain only fixed text, primitive enums and fixed-width numbers.
// Even allowing 256 literal bytes, six 20-digit numbers and two 128-byte enum renderings,
// each message fits 1024 bytes. Rust formatting's old/new requests fit 4*1024 bytes.
// No IO reader, ParquetError display, Polars-error conversion or backtrace runs here.
const _: () = {
    assert!(size_of::<ParquetType>() <= 128);
    assert!(align_of::<ParquetType>() <= 16);
    assert!(size_of::<ColumnDescriptor>() <= 512);
    assert!(align_of::<ColumnDescriptor>() <= 16);
    assert!(size_of::<ColumnChunkMetadata>() <= 256);
    assert!(align_of::<ColumnChunkMetadata>() <= 16);
    assert!(size_of::<RowGroupMetadata>() <= 128);
    assert!(align_of::<RowGroupMetadata>() <= 16);
    assert!(size_of::<FileMetadata>() + 16 <= 256);
    assert!(align_of::<FileMetadata>() <= 16);
};

fn requested_bytes_name(length: usize) -> usize {
    // compact_str 0.9.1 repr/{mod,heap,capacity}.rs: 24 inline bytes, 32-byte heap minimum.
    if length <= 24 { 0 } else { length.max(32) }
}

fn requested_bytes_vector(count: usize) -> Result<usize> {
    // Rust 1.98 RawVec doubles, with minimum capacity four for these non-byte elements.
    // Four times max(n,4) bounds old/new overlap, as well as an exact reservation.
    if count == 0 {
        Ok(0)
    } else {
        requested_bytes_product(4, count.max(4))
    }
}

fn requested_bytes_lookup(count: usize) -> Result<usize> {
    // hashbrown d76a6f8 raw.rs uses <=4*max(n,4) buckets, one control byte per bucket,
    // and <=16 trailing control bytes. PlSmallStr + UnitVec<usize> occupies <=48 bytes.
    // UnitVec's minimum eight/doubling growth needs <=128 bytes per input column,
    // summed across all duplicate-name lists, including simultaneous old/new storage.
    if count == 0 {
        Ok(0)
    } else {
        requested_bytes_sum(&[
            requested_bytes_product(49, requested_bytes_vector(count)?)?,
            16,
        ])
    }
}

fn requested_bytes_sum(terms: &[usize]) -> Result<usize> {
    terms.iter().try_fold(0usize, |total, term| {
        total.checked_add(*term).ok_or_else(bound)
    })
}

fn requested_bytes_product(left: usize, right: usize) -> Result<usize> {
    left.checked_mul(right).ok_or_else(bound)
}

fn bound() -> Error {
    Error::Bound("parquet native metadata bytes")
}

#[cfg(test)]
mod tests;
