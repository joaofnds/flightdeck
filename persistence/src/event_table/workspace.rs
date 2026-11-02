//! Fixed-schema event transformation storage, separate from native decoded payload.
use polars::prelude::*;
use polars_arrow::array::{BinaryViewArray, BooleanArray, PrimitiveArray, View};
use polars_buffer::Buffer;

use crate::{Error, Result};

const COLUMNS: usize = model::catalog::EVENTS.column_count();
const ARRAY_BYTES: usize = size_of::<BinaryViewArray>();
const _: () = {
    assert!(size_of::<PrimitiveArray<u64>>() <= ARRAY_BYTES);
    assert!(size_of::<BooleanArray>() <= ARRAY_BYTES);
    assert!(size_of::<View>() == 16);
    assert!(COLUMNS <= 32);
};

pub(super) fn normalization(rows: usize, chunks: usize) -> Result<usize> {
    shape(rows, chunks)?;
    // Legacy synthesis: two provenance bytes, identity availability/version, u64/i32,
    // two BinaryView null vectors, source availability cast, and eight validity/mask maps.
    let values = rows * (5 + size_of::<u64>() + size_of::<i32>() + 2 * size_of::<View>())
        + 8 * rows.div_ceil(8);
    Ok(structures(chunks) + values)
}

pub(super) fn association(rows: usize) -> Result<usize> {
    shape(rows, 1)?;
    // EventFrame buffers have one chunk. Filtering copies numeric/view values and validity,
    // while binary payload remains borrowed until the separately charged leased copy.
    Ok(structures(1) + COLUMNS * (rows * size_of::<View>() + rows.div_ceil(8)))
}

fn shape(rows: usize, chunks: usize) -> Result<()> {
    if rows > crate::event_observation::BATCH_ROWS_MAX || chunks > 2048 {
        return Err(Error::Bound("event transformation shape"));
    }

    Ok(())
}

fn structures(chunks: usize) -> usize {
    // Pinned Polars 22a147d: flat scalar readers produce one array per selected row group;
    // rechunking is disabled. A rename may clone its ChunkedArray and boxed array wrappers.
    // Four complete column sets cover the input's projection, 14 validation columns,
    // nine synthetic columns, final catalog projection and their temporary wrappers.
    // Four times each vector's max(n,4) covers Rust RawVec growth including old/new overlap.
    let column = size_of::<ChunkedArray<UInt64Type>>()
        + 2 * size_of::<usize>()
        + size_of::<Field>()
        + 2 * size_of::<usize>()
        + 128
        + 4 * chunks.max(4) * (size_of::<Box<dyn polars_arrow::array::Array>>() + ARRAY_BYTES)
        + size_of::<Buffer<u8>>();
    let columns = 4 * COLUMNS * column;
    let vectors = 4 * COLUMNS * size_of::<Column>();
    // DataFrame/schema construction uses a bounded name lookup with <=4n buckets,
    // including control bytes and the trailing SIMD group. Names fit128 admitted bytes.
    let lookup = 4 * COLUMNS * (size_of::<PlSmallStr>() + size_of::<DataType>() + 1) + 16;
    columns + vectors + 4 * lookup + 4 * size_of::<DataFrame>()
}
