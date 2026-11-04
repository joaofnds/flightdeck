//! Conservative whole-row-group work reservations for the legacy event slice reader.
use std::sync::Arc;

use polars_parquet::parquet::metadata::ThriftFileMetadata;
use polars_parquet_format::Type;

use crate::engine::AdmissionRefusal;
use crate::resources::{ByteBudget, ByteReservation};

pub(super) struct DecodeProfile {
    groups: Vec<Group>,
    _reservation: ByteReservation,
}

struct Group {
    first: u64,
    end: u64,
    bytes: usize,
    columns: Vec<usize>,
}

impl DecodeProfile {
    pub(super) fn capture(
        metadata: &ThriftFileMetadata,
        budget: &Arc<ByteBudget>,
        file_backed: bool,
    ) -> Result<Self, AdmissionRefusal> {
        assert!(metadata.row_groups.len() <= super::admission::ROW_GROUPS_MAX);
        let reservation = budget
            .reserve(
                metadata.row_groups.len()
                    * (size_of::<Group>() + metadata.schema.len() * size_of::<usize>()),
            )
            .ok_or(AdmissionRefusal::Budget)?;
        let mut groups = Vec::with_capacity(metadata.row_groups.len());
        let mut first = 0_u64;
        for group in &metadata.row_groups {
            let count = u64::try_from(group.num_rows)
                .map_err(|_| AdmissionRefusal::Bound("parquet profile rows"))?;
            let end = first
                .checked_add(count)
                .ok_or(AdmissionRefusal::Bound("parquet profile rows"))?;
            let mut bytes = 0_u64;
            let mut columns = Vec::with_capacity(group.columns.len());
            for column in &group.columns {
                let column = column
                    .meta_data
                    .as_ref()
                    .ok_or(AdmissionRefusal::Bound("parquet profile column"))?;
                let work = capture_column_work_bytes(column, count, file_backed)?;
                columns.push(
                    usize::try_from(work)
                        .map_err(|_| AdmissionRefusal::Bound("parquet profile column bytes"))?,
                );
                bytes = bytes
                    .checked_add(work)
                    .ok_or(AdmissionRefusal::Bound("parquet profile work bytes"))?;
            }
            groups.push(Group {
                first,
                end,
                columns,
                bytes: usize::try_from(bytes)
                    .map_err(|_| AdmissionRefusal::Bound("parquet profile work bytes"))?,
            });
            first = end;
        }
        assert_eq!(groups.capacity(), metadata.row_groups.len());
        Ok(Self {
            groups,
            _reservation: reservation,
        })
    }

    pub(super) fn bounded_count(
        &self,
        offset: u64,
        requested: u32,
    ) -> Result<u32, AdmissionRefusal> {
        for group in &self.groups {
            if group.first <= offset && offset < group.end {
                return u32::try_from((group.end - offset).min(u64::from(requested)))
                    .map_err(|_| AdmissionRefusal::Bound("parquet profile slice"));
            }
        }
        Err(AdmissionRefusal::Bound("parquet profile slice"))
    }
    pub(super) fn column_work(
        &self,
        offset: u64,
        count: u32,
        column: usize,
    ) -> Result<usize, AdmissionRefusal> {
        let end = offset
            .checked_add(u64::from(count))
            .ok_or(AdmissionRefusal::Bound("parquet profile slice"))?;
        let mut bytes = 0usize;
        for group in &self.groups {
            if group.first < end && offset < group.end {
                bytes = bytes
                    .checked_add(
                        *group
                            .columns
                            .get(column)
                            .ok_or(AdmissionRefusal::Bound("parquet profile column"))?,
                    )
                    .ok_or(AdmissionRefusal::Bound("parquet profile work bytes"))?;
            }
        }
        Ok(bytes)
    }
    pub(super) fn reserve(
        &self,
        offset: u64,
        count: u32,
        budget: &Arc<ByteBudget>,
    ) -> Result<ByteReservation, AdmissionRefusal> {
        budget
            .reserve(self.work(offset, count)?.0)
            .ok_or(AdmissionRefusal::Budget)
    }

    pub(super) fn work(&self, offset: u64, count: u32) -> Result<(usize, usize), AdmissionRefusal> {
        let end = offset
            .checked_add(u64::from(count))
            .ok_or(AdmissionRefusal::Bound("parquet profile slice"))?;
        let mut bytes = 0_usize;
        let mut groups = 0;
        for group in &self.groups {
            if group.first < end && offset < group.end {
                groups += 1;
                bytes = bytes
                    .checked_add(group.bytes)
                    .ok_or(AdmissionRefusal::Bound("parquet profile work bytes"))?;
            }
        }
        Ok((bytes, groups))
    }
}

fn capture_column_work_bytes(
    column: &polars_parquet_format::ColumnMetaData,
    count: u64,
    file_backed: bool,
) -> Result<u64, AdmissionRefusal> {
    let expanded = u64::try_from(column.total_uncompressed_size)
        .map_err(|_| AdmissionRefusal::Bound("parquet profile expanded bytes"))?;
    let width = match column.type_ {
        Type::BOOLEAN => 1,
        Type::INT32 | Type::FLOAT => 4,
        Type::INT64 | Type::DOUBLE => 8,
        Type::BYTE_ARRAY => 16,
        _ => return Err(AdmissionRefusal::Bound("parquet profile scalar")),
    };
    // BinaryView can retain the entire group's payload alongside expanded pages.
    let compressed = if file_backed {
        u64::try_from(column.total_compressed_size)
            .map_err(|_| AdmissionRefusal::Bound("parquet profile compressed bytes"))?
    } else {
        0
    };
    count
        .checked_mul(width)
        .and_then(|fixed| fixed.checked_add(count.div_ceil(8)))
        .and_then(|fixed| {
            fixed.checked_add(if column.type_ == Type::BYTE_ARRAY {
                expanded
            } else {
                0
            })
        })
        .and_then(|decoded| expanded.checked_add(decoded))
        .and_then(|bytes| bytes.checked_add(compressed))
        .ok_or(AdmissionRefusal::Bound("parquet profile decoded bytes"))
}
