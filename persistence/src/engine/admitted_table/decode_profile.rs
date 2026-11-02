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
}

impl DecodeProfile {
    pub(super) fn capture(
        metadata: &ThriftFileMetadata,
        budget: &Arc<ByteBudget>,
    ) -> Result<Self, AdmissionRefusal> {
        assert!(metadata.row_groups.len() <= super::admission::ROW_GROUPS_MAX);
        let reservation = budget
            .reserve(metadata.row_groups.len() * size_of::<Group>())
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
            for column in &group.columns {
                let column = column
                    .meta_data
                    .as_ref()
                    .ok_or(AdmissionRefusal::Bound("parquet profile column"))?;
                let expanded = u64::try_from(column.total_uncompressed_size)
                    .map_err(|_| AdmissionRefusal::Bound("parquet profile expanded bytes"))?;
                let width = match column.type_ {
                    Type::BOOLEAN => 1,
                    Type::INT32 | Type::FLOAT => 4,
                    Type::INT64 | Type::DOUBLE => 8,
                    Type::BYTE_ARRAY => 16,
                    _ => return Err(AdmissionRefusal::Bound("parquet profile scalar")),
                };
                // Charge compressed input elsewhere, then overlapping expanded pages and
                // decoded backing here. BinaryView can retain the entire group's payload.
                let decoded = count
                    .checked_mul(width)
                    .and_then(|fixed| fixed.checked_add(count.div_ceil(8)))
                    .and_then(|fixed| {
                        fixed.checked_add(if column.type_ == Type::BYTE_ARRAY {
                            expanded
                        } else {
                            0
                        })
                    })
                    .ok_or(AdmissionRefusal::Bound("parquet profile decoded bytes"))?;
                bytes = bytes
                    .checked_add(expanded)
                    .and_then(|bytes| bytes.checked_add(decoded))
                    .ok_or(AdmissionRefusal::Bound("parquet profile work bytes"))?;
            }
            groups.push(Group {
                first,
                end,
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
