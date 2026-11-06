use std::sync::Arc;

use polars::prelude::*;

use super::{CapturedEventPart, EventFrame, EventPart};
use crate::engine::{AdmissionRefusal, AdmittedReadRefusal, AdmittedTable, DecodeLimits};
use crate::event_observation::{BATCH_BYTES_MAX, BATCH_ROWS_MAX};
use crate::resources::ByteBudget;
use crate::{Error, Result};

#[cfg(test)]
mod tests;

/// What normalizing and associating one slice may use, on top of what decoding it holds.
const SLICE_WORKSPACE_BYTES: usize = BATCH_BYTES_MAX * 4;

pub(super) fn limits() -> DecodeLimits {
    DecodeLimits {
        compressed_bytes: collector::u64_from_usize(BATCH_BYTES_MAX * 3),
        expanded_bytes: collector::u64_from_usize(BATCH_BYTES_MAX * 2),
        rows: collector::u64_from_usize(BATCH_ROWS_MAX),
        row_groups: 1,
        columns: model::catalog::EVENTS.column_count(),
    }
}

pub(super) fn slice(
    part: &EventPart,
    offset: u64,
    count: u32,
    budget: &Arc<ByteBudget>,
) -> std::result::Result<EventFrame, AdmittedReadRefusal> {
    if count == 0
        || u64::from(count) > collector::u64_from_usize(BATCH_ROWS_MAX)
        || offset > part.rows
    {
        return Err(Error::Bound("event physical slice").into());
    }
    let file = collector::recording::private::open_private_file(part.path(), false)?;
    part.pin.validate_part_file(part.index, &file)?;
    let identity = file.try_clone()?;
    let table = AdmittedTable::from_file_budgeted_with_limits(file, budget, &limits())
        .map_err(AdmittedReadRefusal::Admission)?;
    let result = captured_slice(part, &table, offset, count, budget)?;
    part.pin.validate_part_file(part.index, &identity)?;
    Ok(result)
}

pub(super) fn capture(
    part: EventPart,
    budget: &Arc<ByteBudget>,
    capture: impl FnOnce(
        std::fs::File,
        &Arc<ByteBudget>,
    ) -> std::result::Result<AdmittedTable, AdmissionRefusal>,
) -> std::result::Result<CapturedEventPart, AdmittedReadRefusal> {
    let file = collector::recording::private::open_private_file(part.path(), false)?;
    part.pin.validate_part_file(part.index, &file)?;
    let limits = limits();
    if file.metadata()?.len() > limits.compressed_bytes {
        return Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Bound(
            "parquet file bytes",
        )));
    }

    let mut input = capture(file, budget).map_err(AdmittedReadRefusal::Admission)?;
    if !input.uses_budget(budget) {
        return Err(Error::Bound("event input budget").into());
    }

    part.pin.validate_part_input(part.index, &input)?;
    input
        .prepare_event_input(&limits)
        .map_err(AdmittedReadRefusal::Admission)?;
    if input.row_count() != part.rows {
        return Err(Error::Bound("event physical row count").into());
    }

    slice_schema(&input, &part)?;

    Ok(CapturedEventPart { input, part })
}

pub(super) fn captured_slice(
    part: &EventPart,
    table: &AdmittedTable,
    offset: u64,
    count: u32,
    budget: &Arc<ByteBudget>,
) -> std::result::Result<EventFrame, AdmittedReadRefusal> {
    if count == 0
        || u64::from(count) > collector::u64_from_usize(BATCH_ROWS_MAX)
        || offset > part.rows
    {
        return Err(Error::Bound("event physical slice").into());
    }
    if !table.uses_budget(budget) {
        return Err(Error::Bound("event input budget").into());
    }
    if table.row_count() != part.rows {
        return Err(Error::Bound("event physical row count").into());
    }

    let reservation = slice_workspace(budget)?;
    let identities = slice_schema(table, part)?;
    let columns = slice_columns(identities);
    crate::work::record(|counts| counts.collections += 1);
    crate::work::ingestion::record_content(crate::work::ingestion::Kind::NativeCollect, 0);
    let rows = table.read_slice_metadata_budgeted(&columns, offset, count)?;
    let native_rows = normalize(rows, identities)?;
    validate_extent(
        &native_rows,
        &part.events,
        offset == 0,
        offset + collector::u64_from_usize(native_rows.height()) == part.rows,
    )?;
    // Copies and buffer leases precede any public borrow: native clones must keep their charge.
    let rows = crate::engine::frame_lease::copy_event_frame(&native_rows, budget)?;
    drop(native_rows);
    Ok(EventFrame { rows, reservation })
}

fn slice_workspace(
    budget: &Arc<ByteBudget>,
) -> std::result::Result<crate::resources::ByteReservation, AdmittedReadRefusal> {
    budget
        .reserve(SLICE_WORKSPACE_BYTES)
        .ok_or(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
}

fn slice_columns(identities: bool) -> Vec<&'static str> {
    let mut columns: Vec<_> = model::catalog::EVENTS
        .columns
        .iter()
        .map(|column| column.name)
        .collect();
    if identities {
        columns.extend(crate::schema::IDENTITY_FIELDS.iter().map(|field| field.0));
    }
    columns.extend(
        crate::schema::EVENT_OBSERVATION_FIELDS
            .iter()
            .map(|field| field.0),
    );
    columns
}

fn slice_schema(table: &AdmittedTable, part: &EventPart) -> Result<bool> {
    let identities =
        crate::identity::validate_fields(crate::schema::Table::Events, || table.fields())?;
    let provenance = crate::event_observation::extended_fields(table.fields())?;
    if part.identities != identities || !provenance {
        return Err(Error::Bound("event physical schema"));
    }
    Ok(identities)
}

fn normalize(mut rows: DataFrame, identities: bool) -> Result<DataFrame> {
    crate::event_recovery::batch::validate_frame(&rows, identities)?;
    if identities {
        return Ok(rows);
    }

    for (name, kind) in [
        ("identity_version", DataType::UInt8),
        ("process_unique_id", DataType::UInt64),
        ("pid_version", DataType::Int32),
        ("executable_name", DataType::Binary),
        ("app_path", DataType::Binary),
    ] {
        rows.with_column(normalize_null(name, rows.height(), &kind)?)?;
    }
    rows.with_column(Column::new(
        "identity_availability".into(),
        vec![crate::schema::identity_availability::UNSUPPORTED; rows.height()],
    ))?;
    rows.with_column(
        rows.column("availability")?
            .clone()
            .with_name("usage_availability".into()),
    )?;
    let columns = model::catalog::EVENTS
        .columns
        .iter()
        .chain(
            model::catalog::EVENTS
                .groups
                .iter()
                .flat_map(|group| group.iter()),
        )
        .map(|column| rows.column(column.name).cloned())
        .collect::<PolarsResult<Vec<_>>>()?;
    Ok(DataFrame::new(rows.height(), columns)?)
}

fn normalize_null(name: &str, rows: usize, kind: &DataType) -> Result<Column> {
    use polars_arrow::array::{BinaryViewArray, PrimitiveArray, View};
    use polars_arrow::bitmap::Bitmap;
    use polars_buffer::Buffer;

    // Arrow's new_null uses a process-global8MiB zero arena even for zero rows.
    // These bounded owned buffers keep synthesis inside this caller's workspace.
    let validity = Some(Bitmap::from_u8_vec(vec![0; rows.div_ceil(8)], rows));
    Ok(match kind {
        DataType::UInt8 => UInt8Chunked::from_chunk_iter(
            name.into(),
            [PrimitiveArray::new(
                kind.to_arrow(CompatLevel::newest()),
                vec![0_u8; rows].into(),
                validity,
            )],
        )
        .into_column(),
        DataType::UInt64 => UInt64Chunked::from_chunk_iter(
            name.into(),
            [PrimitiveArray::new(
                kind.to_arrow(CompatLevel::newest()),
                vec![0_u64; rows].into(),
                validity,
            )],
        )
        .into_column(),
        DataType::Int32 => Int32Chunked::from_chunk_iter(
            name.into(),
            [PrimitiveArray::new(
                kind.to_arrow(CompatLevel::newest()),
                vec![0_i32; rows].into(),
                validity,
            )],
        )
        .into_column(),
        DataType::Binary => BinaryChunked::from_chunk_iter(
            name.into(),
            [BinaryViewArray::try_new(
                kind.to_arrow(CompatLevel::newest()),
                vec![View::default(); rows].into(),
                Vec::<Buffer<u8>>::new().into(),
                validity,
            )?],
        )
        .into_column(),
        _ => return Err(Error::Bound("event synthesized identity type")),
    })
}

fn validate_extent(
    rows: &DataFrame,
    expected: &std::ops::Range<u64>,
    first: bool,
    last: bool,
) -> Result<()> {
    let column = rows.column("event")?.u64()?;
    if column.null_count() != 0 {
        return Err(Error::Bound("event physical number"));
    }
    let mut previous = None;
    for event in column.into_no_null_iter() {
        if !expected.contains(&event) || previous.is_some_and(|previous| previous >= event) {
            return Err(Error::Bound("event physical order"));
        }
        previous = Some(event);
    }
    if first && column.get(0) != Some(expected.start) {
        return Err(Error::Bound("event physical first"));
    }
    if last && previous != Some(expected.end - 1) {
        return Err(Error::Bound("event physical end"));
    }
    Ok(())
}

pub(super) fn ordinary_error(error: AdmittedReadRefusal) -> Error {
    match error {
        AdmittedReadRefusal::Decode(error) => error,
        AdmittedReadRefusal::Admission(AdmissionRefusal::Budget) => {
            Error::Bound("event table budget")
        }
        AdmittedReadRefusal::Admission(AdmissionRefusal::Bound(message)) => Error::Bound(message),
        AdmittedReadRefusal::Admission(AdmissionRefusal::Io { kind, raw_os_error }) => {
            Error::Io(raw_os_error.map_or_else(
                || std::io::Error::from(kind),
                std::io::Error::from_raw_os_error,
            ))
        }
    }
}
