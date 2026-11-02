//! Direct projection of the fixed event identity constraints, without a query plan.
use polars::prelude::*;
use polars_arrow::array::BooleanArray;
use polars_arrow::bitmap::Bitmap;

use crate::schema::Table;
use crate::{Error, Result};

pub(crate) fn event_validation_frame(frame: &DataFrame) -> Result<DataFrame> {
    if frame.height() > crate::event_observation::BATCH_ROWS_MAX {
        return Err(Error::Bound("event validation batch rows"));
    }

    let mut columns = Vec::with_capacity(14);
    for name in [
        "identity_version",
        "identity_availability",
        "process_unique_id",
        "pid_version",
        "executable_name",
        "app_path",
        "usage_availability",
    ] {
        columns.push(frame.column(name)?.clone());
    }
    let payload = super::validation_payload(Table::Events);
    columns.push(event_validation_mask(
        frame,
        &payload,
        true,
        "usage_present",
    )?);
    columns.push(event_validation_mask(
        frame,
        &payload,
        false,
        "usage_absent",
    )?);
    columns.push(
        frame
            .column("availability")?
            .cast(&DataType::UInt8)?
            .with_name("source_usage_availability".into()),
    );
    columns.push(event_validation_mask(
        frame,
        &["pid"],
        true,
        "observation_present",
    )?);
    columns.push(
        frame
            .column("publication")?
            .clone()
            .with_name("event_publication".into()),
    );
    for (name, _) in crate::event_observation::EVENT_OBSERVATION_FIELDS {
        columns.push(frame.column(name)?.clone());
    }
    Ok(DataFrame::new(frame.height(), columns)?)
}

fn event_validation_mask(
    frame: &DataFrame,
    fields: &[&str],
    present: bool,
    name: &'static str,
) -> Result<Column> {
    let mut values = vec![u8::MAX; frame.height().div_ceil(8)];
    for field in fields {
        let column = frame.column(field)?;
        let mut offset = 0;
        for array in column.as_materialized_series().chunks() {
            for index in 0..array.len() {
                let valid = array
                    .validity()
                    .is_none_or(|validity| validity.get_bit(index));
                if valid != present {
                    let row = offset + index;
                    values[row / 8] &= !(1 << (row % 8));
                }
            }
            offset += array.len();
        }
        assert_eq!(offset, frame.height());
    }
    let values = Bitmap::from_u8_vec(values, frame.height());
    let array = BooleanArray::new(
        DataType::Boolean.to_arrow(CompatLevel::newest()),
        values,
        None,
    );
    Ok(BooleanChunked::from_chunk_iter(name.into(), [array]).into_column())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_recovery::batch::{self, Row};
    use crate::owned_identity::OwnedIdentity;
    use crate::window::EventRecord;

    #[test]
    fn direct_event_masks_match_the_existing_expression_projection() {
        // Goal: removing query plans retains every identity/provenance projection value.
        // Method: compare direct and original expression frames across all payload-null cases.
        let mut frame = frame();
        compare(&frame);
        for field in super::super::validation_payload(Table::Events) {
            frame
                .with_column(Column::new(field.into(), [Some(1_u64), None]))
                .unwrap();
            compare(&frame);
        }
        frame
            .with_column(Column::new("pid".into(), [None, Some(41_i32)]))
            .unwrap();
        compare(&frame);
    }

    fn compare(frame: &DataFrame) {
        let expected = frame
            .clone()
            .lazy()
            .select(super::super::validation_columns_with_event_observation(
                Table::Events,
                true,
            ))
            .collect()
            .unwrap();
        let actual = event_validation_frame(frame).unwrap();
        assert!(actual.equals_missing(&expected));
    }

    fn frame() -> DataFrame {
        let rows = [Some(1), None]
            .into_iter()
            .enumerate()
            .map(|(index, publication)| Row {
                publication,
                event: EventRecord {
                    number: collector::u64_from_usize(index) + 1,
                    tick: 1,
                    pid: 41,
                    kind: crate::schema::event_kind::EXEC,
                    coalition: None,
                    usage: None,
                    availability: crate::schema::availability::NOT_APPLICABLE,
                    noticed: None,
                },
                identity: Some(OwnedIdentity {
                    pid: 41,
                    creation: None,
                    availability: crate::schema::identity_availability::UNSUPPORTED,
                    usage: crate::schema::availability::NOT_APPLICABLE,
                    program: None,
                }),
                name: None,
                app: None,
            })
            .collect::<Vec<_>>();
        batch::frame(&rows, true).unwrap()
    }
}
