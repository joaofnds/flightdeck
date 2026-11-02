//! Owned event-schema buffers whose ledger charge follows every native buffer clone.
use std::sync::Arc;

use polars::prelude::*;
use polars_arrow::array::{BinaryViewArray, PrimitiveArray, View};
use polars_arrow::bitmap::Bitmap;
use polars_buffer::Buffer;

use super::{AdmissionRefusal, AdmittedReadRefusal};
use crate::Error;
use crate::resources::{ByteBudget, ByteReservation};

struct BufferOwner<T> {
    values: Vec<T>,
    _reservation: ByteReservation,
    _structure: Arc<ByteReservation>,
}

const BUFFER_OVERHEAD: usize = 256;
const FRAME_OVERHEAD: usize = 4096;
const COLUMN_OVERHEAD: usize = 1024;

impl<T> AsRef<[T]> for BufferOwner<T> {
    fn as_ref(&self) -> &[T] {
        &self.values
    }
}

pub(crate) fn copy_event_frame(
    frame: &DataFrame,
    budget: &Arc<ByteBudget>,
) -> Result<DataFrame, AdmittedReadRefusal> {
    if frame.height() > crate::event_observation::BATCH_ROWS_MAX
        || frame.width() > model::catalog::EVENTS.column_count()
    {
        return Err(Error::Bound("event output copy shape").into());
    }
    let structure = Arc::new(reserve(
        budget,
        FRAME_OVERHEAD + frame.width() * COLUMN_OVERHEAD,
    )?);
    let mut columns = Vec::with_capacity(frame.width());
    for column in frame.columns() {
        if column.name().len() > super::admission::COLUMN_NAME_BYTES_MAX {
            return Err(Error::Bound("event output column name bytes").into());
        }
        let owned = match column.dtype() {
            DataType::UInt8 => copy_numeric(column.u8().map_err(Error::from)?, budget, &structure)?,
            DataType::UInt64 => {
                copy_numeric(column.u64().map_err(Error::from)?, budget, &structure)?
            }
            DataType::Int32 => {
                copy_numeric(column.i32().map_err(Error::from)?, budget, &structure)?
            }
            DataType::Binary => {
                copy_binary(column.binary().map_err(Error::from)?, budget, &structure)?
            }
            _ => return Err(Error::Bound("event output copy type").into()),
        };
        columns.push(owned.into_column());
    }
    Ok(DataFrame::new(frame.height(), columns).map_err(Error::from)?)
}

fn copy_numeric<T: PolarsNumericType>(
    column: &ChunkedArray<T>,
    budget: &Arc<ByteBudget>,
    structure: &Arc<ByteReservation>,
) -> Result<Series, AdmittedReadRefusal> {
    let reservation = reserve(
        budget,
        column.len() * size_of::<T::Native>() + BUFFER_OVERHEAD,
    )?;
    let mut values = Vec::with_capacity(column.len());
    for value in column.iter() {
        values.push(value.unwrap_or_default());
    }
    let validity = copy_validity(
        column.len(),
        column.null_count(),
        column.iter().map(|value| value.is_some()),
        budget,
        structure,
    )?;
    let values = owner(values, reservation, structure);
    let array = PrimitiveArray::new(
        column.dtype().to_arrow(CompatLevel::newest()),
        values,
        validity,
    );
    Ok(ChunkedArray::<T>::from_chunk_iter(column.name().clone(), [array]).into_series())
}

fn copy_binary(
    column: &BinaryChunked,
    budget: &Arc<ByteBudget>,
    structure: &Arc<ByteReservation>,
) -> Result<Series, AdmittedReadRefusal> {
    let mut bytes = 0;
    for value in column.iter().flatten() {
        if value.len() > crate::schema::IDENTITY_PATH_BYTES_MAX {
            return Err(Error::Bound("event output program bytes").into());
        }
        if value.len() > 12 {
            bytes += value.len();
        }
    }
    let views_reservation = reserve(budget, column.len() * size_of::<View>() + BUFFER_OVERHEAD)?;
    let bytes_reservation = reserve(budget, bytes + BUFFER_OVERHEAD)?;
    let buffers_reservation = reserve(budget, size_of::<Buffer<u8>>() + BUFFER_OVERHEAD)?;
    let mut views = Vec::with_capacity(column.len());
    let mut values = Vec::with_capacity(bytes);
    for value in column.iter() {
        let value = value.unwrap_or_default();
        let offset =
            u32::try_from(values.len()).map_err(|_| Error::Bound("event output byte offset"))?;
        views.push(View::new_from_bytes(value, 0, offset));
        if value.len() > 12 {
            values.extend_from_slice(value);
        }
    }
    let validity = copy_validity(
        column.len(),
        column.null_count(),
        column.iter().map(|value| value.is_some()),
        budget,
        structure,
    )?;
    let buffers = owner(
        vec![owner(values, bytes_reservation, structure)],
        buffers_reservation,
        structure,
    );
    let array = BinaryViewArray::try_new(
        ArrowDataType::BinaryView,
        owner(views, views_reservation, structure),
        buffers,
        validity,
    )
    .map_err(Error::from)?;
    Ok(BinaryChunked::from_chunk_iter(column.name().clone(), [array]).into_series())
}

fn copy_validity(
    length: usize,
    unset: usize,
    values: impl Iterator<Item = bool>,
    budget: &Arc<ByteBudget>,
    structure: &Arc<ByteReservation>,
) -> Result<Option<Bitmap>, AdmittedReadRefusal> {
    if unset == 0 {
        return Ok(None);
    }
    let reservation = reserve(budget, length.div_ceil(8) + BUFFER_OVERHEAD)?;
    let mut bytes = vec![0_u8; length.div_ceil(8)];
    for (index, valid) in values.enumerate().take(length) {
        if valid {
            bytes[index / 8] |= 1 << (index % 8);
        }
    }
    bitmap(owner(bytes, reservation, structure), 0, length, unset).map(Some)
}

fn bitmap(
    buffer: Buffer<u8>,
    offset: usize,
    length: usize,
    unset: usize,
) -> Result<Bitmap, AdmittedReadRefusal> {
    let end = offset
        .checked_add(length)
        .ok_or(Error::Bound("event output validity extent"))?;
    if end.div_ceil(8) > buffer.len() || unset > length {
        return Err(Error::Bound("event output validity extent").into());
    }
    let actual = (offset..end)
        .filter(|index| buffer[index / 8] & (1 << (index % 8)) == 0)
        .count();
    if actual != unset {
        return Err(Error::Bound("event output validity count").into());
    }
    assert!(end.div_ceil(8) <= buffer.len());
    assert!(unset <= length);
    let offset = buffer
        .offset()
        .checked_mul(8)
        .and_then(|start| start.checked_add(offset))
        .ok_or(Error::Bound("event output validity storage offset"))?;
    let storage = buffer.into_storage();
    let end = offset
        .checked_add(length)
        .ok_or(Error::Bound("event output validity storage extent"))?;
    if end.div_ceil(8) > storage.len() {
        return Err(Error::Bound("event output validity storage extent").into());
    }
    // The checked bit extent is within the owned storage and its cached unset count is exact.
    Ok(unsafe { Bitmap::from_inner_unchecked(storage, offset, length, Some(unset)) })
}

fn owner<T: Send + 'static>(
    values: Vec<T>,
    reservation: ByteReservation,
    structure: &Arc<ByteReservation>,
) -> Buffer<T> {
    assert!(values.capacity() * size_of::<T>() <= reservation.bytes());
    Buffer::from_owner(BufferOwner {
        values,
        _reservation: reservation,
        _structure: Arc::clone(structure),
    })
}

fn reserve(budget: &Arc<ByteBudget>, bytes: usize) -> Result<ByteReservation, AdmittedReadRefusal> {
    budget
        .reserve(bytes)
        .ok_or(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
}

#[cfg(test)]
mod tests;
