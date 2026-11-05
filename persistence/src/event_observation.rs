//! Versioned event provenance permits an unavailable observation without fabricating one.
use std::ops::Range;

use polars::prelude::{
    BinaryChunked, DataFrame, DataType, Expr, IntoColumn, NewChunkedArray, Schema, UInt8Type,
    UInt64Type, col, lit,
};

use crate::engine::Cells;
use crate::{Error, Result};

pub use crate::schema::EVENT_OBSERVATION_FIELDS;
pub const VERSION: u8 = 1;
pub const ASSOCIATED: u8 = 0;
pub const UNAVAILABLE: u8 = 1;
/// The largest event retention capacity admitted from a source ring.
pub const ROWS_MAX: usize = 1_966_080;
/// One independently decoded and validated event batch.
pub const BATCH_ROWS_MAX: usize = 16_384;
/// Owned numeric, identity and raw program payload before encoding overhead.
pub const BATCH_BYTES_MAX: usize = 16 * 1024 * 1024;

/// Projects the actual complete group or a strict legacy associated group of the same types.
#[must_use]
pub fn columns(extended: bool) -> Vec<Expr> {
    if extended {
        EVENT_OBSERVATION_FIELDS
            .iter()
            .map(|(name, _)| col(*name))
            .collect()
    } else {
        vec![
            lit(VERSION)
                .cast(DataType::UInt8)
                .alias(EVENT_OBSERVATION_FIELDS[0].0),
            lit(ASSOCIATED)
                .cast(DataType::UInt8)
                .alias(EVENT_OBSERVATION_FIELDS[1].0),
        ]
    }
}

/// Identity normalization has already validated the bounded source's rows.
/// Append strict legacy provenance and give every source the same catalog column order.
pub(crate) fn normalize(connection: &crate::engine::Engine, source: &str) -> Result<String> {
    let schema = connection.schema(source)?;
    let extended = extended(schema.as_ref())?;
    if schema.len() > model::catalog::EVENTS.column_count() {
        return Err(Error::Bound("event normalized columns"));
    }
    let mut fields: Vec<_> = model::catalog::EVENTS
        .columns
        .iter()
        .chain(model::catalog::IDENTITY.iter())
        .map(|column| column.name.to_owned())
        .collect();
    if extended {
        fields.extend(
            EVENT_OBSERVATION_FIELDS
                .iter()
                .map(|field| field.0.to_owned()),
        );
    } else {
        fields.push(format!(
            "{VERSION}::UTINYINT {}",
            EVENT_OBSERVATION_FIELDS[0].0
        ));
        fields.push(format!(
            "{ASSOCIATED}::UTINYINT {}",
            EVENT_OBSERVATION_FIELDS[1].0
        ));
    }
    Ok(format!("(SELECT {} FROM {source})", fields.join(",")))
}

/// Validates every selected row before a caller can omit detached events from observations.
pub fn validate(frame: &DataFrame, rows: Range<usize>) -> Result<()> {
    validate_named(frame, rows, "publication")
}

/// Retains only associated rows after validating the complete input, including detached rows.
pub fn associated(frame: &DataFrame) -> Result<DataFrame> {
    let mut retained = associated_rows(frame)?;
    for name in ["executable_name", "app_path"] {
        if retained.get_column_index(name).is_some() {
            let bytes = retained.column(name)?.binary()?;
            let owned = BinaryChunked::from_iter_options(name.into(), bytes.iter());
            retained.with_column(owned.into_column())?;
        }
    }
    Ok(retained)
}

/// Binary views borrow input buffers until the caller copies them into its charged owner.
pub(crate) fn associated_rows(frame: &DataFrame) -> Result<DataFrame> {
    validate(frame, 0..frame.height())?;
    let mask = associated_mask(frame)?;
    let length = mask.iter().filter(|value| *value == Some(true)).count();
    let mut columns = Vec::with_capacity(frame.width());
    for column in frame.columns() {
        columns.push(associated_column(column, &mask, length)?);
    }
    Ok(DataFrame::new(length, columns)?)
}

fn associated_column(
    column: &polars::prelude::Column,
    mask: &polars::prelude::BooleanChunked,
    length: usize,
) -> Result<polars::prelude::Column> {
    use polars::prelude::Series;
    use polars_arrow::bitmap::Bitmap;

    let source = column.as_materialized_series();
    let mut bits = vec![0_u8; length.div_ceil(8)];
    let mut read = 0;
    let mut written = 0;
    for array in source.chunks() {
        for index in 0..array.len() {
            if mask.get(read) == Some(true) {
                if array
                    .validity()
                    .is_none_or(|validity| validity.get_bit(index))
                {
                    bits[written / 8] |= 1 << (written % 8);
                }
                written += 1;
            }
            read += 1;
        }
    }
    assert_eq!(written, length);
    assert_eq!(read, source.len());
    // Native filtering's all-null bitmap shortcut uses a permanent global zero arena.
    // Filter values without validity, then attach this operation's owned validity bits.
    let arrays = source
        .chunks()
        .iter()
        .map(|array| array.with_validity(None))
        .collect();
    let values = Series::from_arrow_chunks(column.name().clone(), arrays)?.filter(mask)?;
    let validity = Bitmap::from_u8_vec(bits, length);
    let mut offset = 0;
    let arrays = values
        .chunks()
        .iter()
        .map(|array| {
            let bitmap = validity.clone().sliced(offset, array.len());
            offset += array.len();
            array.with_validity(Some(bitmap))
        })
        .collect();
    assert_eq!(offset, length);
    Ok(Series::from_arrow_chunks(column.name().clone(), arrays)?.into_column())
}

fn associated_mask(frame: &DataFrame) -> Result<polars::prelude::BooleanChunked> {
    use polars::prelude::{BooleanType, ChunkedArray, CompatLevel};
    use polars_arrow::array::BooleanArray;
    use polars_arrow::bitmap::Bitmap;

    let mut bits = vec![0_u8; frame.height().div_ceil(8)];
    for (row, publication) in frame.column("publication")?.u64()?.iter().enumerate() {
        if publication.is_some() {
            bits[row / 8] |= 1 << (row % 8);
        }
    }
    let array = BooleanArray::new(
        DataType::Boolean.to_arrow(CompatLevel::newest()),
        Bitmap::from_u8_vec(bits, frame.height()),
        None,
    );
    Ok(ChunkedArray::<BooleanType>::from_chunk_iter(
        "associated".into(),
        [array],
    ))
}

pub(crate) fn extended(schema: &Schema) -> Result<bool> {
    extended_fields(
        schema
            .iter()
            .map(|(name, kind)| (name.as_str(), kind.clone())),
    )
}

pub(crate) fn extended_fields<'a>(
    columns: impl Iterator<Item = (&'a str, DataType)>,
) -> Result<bool> {
    let mut present = 0;
    for (name, data_type) in columns {
        for (field, _) in EVENT_OBSERVATION_FIELDS {
            if name.eq_ignore_ascii_case(field) {
                if name != field || data_type != DataType::UInt8 {
                    return Err(Error::Bound("event observation field"));
                }
                present += 1;
            }
        }
    }
    match present {
        0 => Ok(false),
        2 => Ok(true),
        _ => Err(Error::Bound("incomplete event observation group")),
    }
}

pub(crate) fn validate_named(
    frame: &DataFrame,
    rows: Range<usize>,
    publication: &str,
) -> Result<()> {
    if rows.start > rows.end || rows.end > frame.height() {
        return Err(Error::Bound("event observation row range"));
    }
    let publication = Cells::<UInt64Type>::named(frame, publication)?;
    let provenance = if extended(frame.schema())? {
        Some((
            Cells::<UInt8Type>::named(frame, EVENT_OBSERVATION_FIELDS[0].0)?,
            Cells::<UInt8Type>::named(frame, EVENT_OBSERVATION_FIELDS[1].0)?,
        ))
    } else {
        None
    };
    for row in rows {
        let publication = publication.optional(row)?;
        if publication == Some(0) {
            return Err(Error::Bound("event publication zero"));
        }
        let availability = if let Some((version, availability)) = &provenance {
            if version.optional(row)? != Some(VERSION) {
                return Err(Error::Bound("event observation version"));
            }
            availability.get(row)?
        } else {
            ASSOCIATED
        };
        if !matches!(
            (availability, publication),
            (ASSOCIATED, Some(_)) | (UNAVAILABLE, None)
        ) {
            return Err(Error::Bound("event observation association"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use polars::prelude::{IntoLazy, NamedFrom, Series};

    use super::*;

    #[test]
    fn event_provenance_does_not_widen_process_column_admission() {
        // Goal: adding two known event fields cannot admit unrelated process columns.
        // Method: normalize a complete empty process schema with one unknown extra field.
        let schema: Schema = model::catalog::PROCESSES
            .columns
            .iter()
            .chain(
                model::catalog::PROCESSES
                    .groups
                    .iter()
                    .flat_map(|group| group.iter()),
            )
            .map(|column| {
                (
                    column.name.into(),
                    match column.kind {
                        model::catalog::Kind::U64 => DataType::UInt64,
                        model::catalog::Kind::I32 => DataType::Int32,
                        model::catalog::Kind::U8 => DataType::UInt8,
                        model::catalog::Kind::Bytes => DataType::Binary,
                        _ => panic!("process schema primitive"),
                    },
                )
            })
            .collect();
        let mut frame = DataFrame::empty_with_schema(&schema);
        frame
            .with_column(Series::new("unrelated".into(), Vec::<u8>::new()).into())
            .unwrap();
        let engine = crate::engine::Engine::new();
        engine.register("processes", frame).unwrap();
        let result = crate::query::table_source(
            &engine,
            crate::schema::Table::Processes,
            crate::query::TableSource::Staged,
        );
        assert!(matches!(result, Err(Error::Bound("table column count"))));
    }

    #[test]
    fn logical_legacy_and_extended_sources_share_complete_catalog_schema() {
        // Goal: live and old saved SQL expose the same typed provenance as physical parts.
        // Method: normalize a real event frame with and without the additive group.
        for extended in [false, true] {
            let mut frame = crate::event_recovery::batch::frame(&[], false).unwrap();
            if !extended {
                for (name, _) in EVENT_OBSERVATION_FIELDS {
                    frame.drop_in_place(name).unwrap();
                }
            }
            let engine = crate::engine::Engine::new();
            engine.register("events", frame).unwrap();
            let source = crate::query::table_source(
                &engine,
                crate::schema::Table::Events,
                crate::query::TableSource::Staged,
            )
            .unwrap();
            let normalized = engine.query(&format!("SELECT * FROM {source}")).unwrap();
            assert_eq!(normalized.height(), 0);
            let expected: Vec<_> = model::catalog::EVENTS
                .columns
                .iter()
                .chain(
                    model::catalog::EVENTS
                        .groups
                        .iter()
                        .flat_map(|group| group.iter()),
                )
                .map(|column| column.name)
                .collect();
            let actual: Vec<_> = normalized
                .get_column_names()
                .into_iter()
                .map(polars::prelude::PlSmallStr::as_str)
                .collect();
            assert_eq!(actual, expected);
            for (name, _) in EVENT_OBSERVATION_FIELDS {
                assert_eq!(normalized.column(name).unwrap().dtype(), &DataType::UInt8);
            }
        }
    }

    #[test]
    fn legacy_projection_preserves_the_provenance_storage_types() {
        // Goal: a saved legacy event remains readable through the production projection.
        // Method: apply the same lazy normalization used by the native and SQL consumers.
        let legacy = DataFrame::new(
            2,
            vec![Series::new("publication".into(), [7_u64, 8]).into()],
        )
        .unwrap();
        let projected = legacy
            .lazy()
            .with_columns(columns(false))
            .collect()
            .unwrap();
        assert_eq!(associated(&projected).unwrap().height(), 2);
        for (name, _) in EVENT_OBSERVATION_FIELDS {
            assert_eq!(projected.column(name).unwrap().dtype(), &DataType::UInt8);
        }
    }

    #[test]
    fn associated_rows_release_program_arenas_owned_only_by_detached_rows() {
        // Goal: retaining a small associated subset cannot pin every detached program buffer.
        // Method: filter one long detached value and inspect the retained native binary arenas.
        let kept = [b'a'; 32];
        let detached = [b'b'; 4095];
        let frame = DataFrame::new(
            2,
            vec![
                Series::new("publication".into(), [Some(7_u64), None]).into(),
                Series::new(EVENT_OBSERVATION_FIELDS[0].0.into(), [VERSION; 2]).into(),
                Series::new(
                    EVENT_OBSERVATION_FIELDS[1].0.into(),
                    [ASSOCIATED, UNAVAILABLE],
                )
                .into(),
                Series::new(
                    "executable_name".into(),
                    [Some(kept.as_slice()), Some(detached.as_slice())],
                )
                .into(),
                Series::new(
                    "app_path".into(),
                    [Some(kept.as_slice()), Some(detached.as_slice())],
                )
                .into(),
            ],
        )
        .unwrap();

        let result = associated(&frame).unwrap();
        drop(frame);

        for name in ["executable_name", "app_path"] {
            let column = result.column(name).unwrap().binary().unwrap();
            let mut retained = 0;
            for array in column.downcast_iter() {
                retained += array.total_buffer_len();
            }
            assert_eq!(column.get(0), Some(kept.as_slice()));
            assert_eq!(retained, kept.len(), "{name}");
        }
    }

    #[test]
    fn missing_observation_is_explicit_and_malformed_rows_cannot_be_filtered_away() {
        // Goal: filtering cannot hide bad provenance or silently assign a detached event.
        // Method: validate the actual typed columns with every association and version case.
        let detached = frame(None, VERSION, UNAVAILABLE);
        assert_eq!(associated(&detached).unwrap().height(), 0);
        let associated_row = frame(Some(7), VERSION, ASSOCIATED);
        assert_eq!(associated(&associated_row).unwrap().height(), 1);
        for invalid in [
            frame(None, VERSION + 1, UNAVAILABLE),
            frame(None, VERSION, ASSOCIATED),
            frame(Some(7), VERSION, UNAVAILABLE),
            frame(None, VERSION, 2),
            frame(Some(0), VERSION, ASSOCIATED),
        ] {
            assert!(associated(&invalid).is_err());
        }
        let mut partial = detached.clone();
        partial
            .drop_in_place(EVENT_OBSERVATION_FIELDS[1].0)
            .unwrap();
        assert!(associated(&partial).is_err());
        let mut wrong_type = detached;
        wrong_type
            .with_column(
                Series::new(EVENT_OBSERVATION_FIELDS[0].0.into(), [u32::from(VERSION)]).into(),
            )
            .unwrap();
        assert!(associated(&wrong_type).is_err());
    }

    #[test]
    fn legacy_events_require_their_original_publication() {
        // Goal: absence of the new group preserves the old strict association contract.
        // Method: remove the entire group from associated and detached typed rows.
        for publication in [Some(7), None] {
            let mut legacy = frame(publication, VERSION, ASSOCIATED);
            for (field, _) in EVENT_OBSERVATION_FIELDS {
                legacy.drop_in_place(field).unwrap();
            }
            assert_eq!(associated(&legacy).is_ok(), publication.is_some());
        }
    }

    fn frame(publication: Option<u64>, version: u8, availability: u8) -> DataFrame {
        DataFrame::new(
            1,
            vec![
                Series::new("publication".into(), [publication]).into(),
                Series::new(EVENT_OBSERVATION_FIELDS[0].0.into(), [version]).into(),
                Series::new(EVENT_OBSERVATION_FIELDS[1].0.into(), [availability]).into(),
            ],
        )
        .unwrap()
    }
}
