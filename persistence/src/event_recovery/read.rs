use std::path::Path;
use std::sync::Arc;

use collector::ring::RunIdentity;
use polars::prelude::*;

use super::{EventPage, Manifest};
use crate::engine::{AdmissionRefusal, AdmittedReadRefusal, AdmittedTable, DecodeLimits};
use crate::event_observation::{BATCH_BYTES_MAX, BATCH_ROWS_MAX};
use crate::resources::ByteBudget;
use crate::{Error, Result};

// Plain scalar values, binary offsets, validity/definition levels and page framing fit
// twice the owned payload. Compressed bytes additionally allow bounded footer/framing.
const EXPANDED_BYTES_MAX: usize = BATCH_BYTES_MAX * 2;
const COMPRESSED_BYTES_MAX: usize = EXPANDED_BYTES_MAX + 8 * 1024 * 1024;
const READ_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// Reads at most one physical batch, validates all its rows, then selects ascending events.
/// `next` resumes after returned rows or at the next batch, including gaps in event numbers.
pub fn read_page(
    root: &Path,
    run: &RunIdentity,
    first_event: u64,
    limit: usize,
) -> Result<EventPage> {
    if first_event == 0 || limit == 0 || limit > BATCH_ROWS_MAX {
        return Err(Error::Bound("terminal event page"));
    }
    super::validate_run(root, run)?;
    let budget = Arc::new(ByteBudget::new(READ_BUDGET_BYTES));
    let _reservation = budget
        .reserve(super::batch::WORKING_BYTES)
        .ok_or(Error::Bound("terminal decode bytes"))?;
    let manifest = crate::store::events::read_committed(root, run)?
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    let selected = manifest
        .batches
        .iter()
        .position(|batch| batch.end > first_event);
    let mut rows = super::batch::empty(manifest.identities)?;
    let mut next = None;
    if let Some(index) = selected {
        rows = read_batch(root, &manifest, index, &budget)?;
        let event = rows.column("event")?.u64()?;
        let offset = event
            .into_no_null_iter()
            .take_while(|number| *number < first_event)
            .count();
        let length = (rows.height() - offset).min(limit);
        next = if offset + length < rows.height() {
            event.get(offset + length)
        } else {
            manifest.batches.get(index + 1).map(|batch| batch.first)
        };
        rows = rows.slice(
            i64::try_from(offset).map_err(|_| Error::Bound("terminal page offset"))?,
            length,
        );
    }
    let rows =
        crate::engine::frame_lease::copy_event_frame(&rows, &budget).map_err(
            |error| match error {
                AdmittedReadRefusal::Admission(error) => admission_error(error),
                AdmittedReadRefusal::Decode(error) => error,
            },
        )?;
    Ok(EventPage {
        rows,
        next,
        coverage: manifest.recovery.events_first..manifest.recovery.events_end,
        losses: manifest.recovery.losses,
    })
}

fn read_batch(
    root: &Path,
    manifest: &Manifest,
    index: usize,
    budget: &Arc<ByteBudget>,
) -> Result<DataFrame> {
    let batch = &manifest.batches[index];
    let path = super::directory(root, &manifest.recovery.run)
        .join("data")
        .join(super::batch_name(index));
    let file = collector::recording::private::open_private_file(&path, false)?;
    let limits = DecodeLimits {
        compressed_bytes: collector::u64_from_usize(COMPRESSED_BYTES_MAX),
        expanded_bytes: collector::u64_from_usize(EXPANDED_BYTES_MAX),
        rows: collector::u64_from_usize(BATCH_ROWS_MAX),
        row_groups: 1,
        columns: model::catalog::EVENTS.column_count(),
    };
    let admitted = AdmittedTable::from_file_budgeted_with_limits(file, budget, &limits)
        .map_err(admission_error)?;
    if admitted.row_count() != batch.rows {
        return Err(Error::Bound("terminal batch row count"));
    }
    let mut columns: Vec<_> = model::catalog::EVENTS
        .columns
        .iter()
        .map(|column| column.name)
        .collect();
    if manifest.identities {
        columns.extend(model::catalog::IDENTITY.iter().map(|column| column.name));
    }
    columns.extend(
        model::catalog::EVENT_OBSERVATION
            .iter()
            .map(|column| column.name),
    );
    let frame = admitted
        .read_metadata_budgeted(
            &columns,
            Some(u32::try_from(BATCH_ROWS_MAX).map_err(|_| Error::Bound("terminal batch rows"))?),
        )
        .map_err(|error| match error {
            AdmittedReadRefusal::Admission(error) => admission_error(error),
            AdmittedReadRefusal::Decode(error) => error,
        })?;
    read_batch_validate(&frame, batch, manifest.identities)?;
    Ok(frame)
}

fn read_batch_validate(frame: &DataFrame, batch: &super::Batch, identities: bool) -> Result<()> {
    if collector::u64_from_usize(frame.height()) != batch.rows
        || frame.estimated_size(true) > BATCH_BYTES_MAX * 2
    {
        return Err(Error::Bound("terminal batch payload"));
    }
    super::batch::validate_frame(frame, identities)?;
    if frame.column("publication")?.null_count() != frame.height() {
        return Err(Error::Bound("terminal associated observation"));
    }
    let events = frame.column("event")?.u64()?;
    let mut previous = None;
    if events.null_count() != 0 {
        return Err(Error::Bound("terminal event number"));
    }
    for event in events.into_no_null_iter() {
        if event < batch.first
            || event >= batch.end
            || previous.is_some_and(|previous| event <= previous)
        {
            return Err(Error::Bound("terminal event order"));
        }
        previous = Some(event);
    }
    if events.get(0) != Some(batch.first) || previous != Some(batch.end - 1) {
        return Err(Error::Bound("terminal batch extent"));
    }
    Ok(())
}

fn admission_error(error: AdmissionRefusal) -> Error {
    match error {
        AdmissionRefusal::Budget => Error::Bound("terminal read budget"),
        AdmissionRefusal::Bound(message) => Error::Bound(message),
        AdmissionRefusal::Io { kind, raw_os_error } => Error::Io(raw_os_error.map_or_else(
            || std::io::Error::from(kind),
            std::io::Error::from_raw_os_error,
        )),
    }
}
