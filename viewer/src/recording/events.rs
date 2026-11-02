//! Bounded physical event reads preserve raw validation before observation placement.

use std::sync::Arc;

use persistence::engine::{AdmittedReadRefusal, AdmittedTable};
use persistence::event_observation::{BATCH_ROWS_MAX, ROWS_MAX};
use persistence::event_table::EventTable;
use persistence::resources::ByteBudget;
use polars::prelude::DataFrame;

use super::{Refused, ScopedSealedMinute};

/// Independent readers use one bounded working allowance; shared loaders supply their ledger.
const WORKING_BYTES_MAX: usize = 256 * 1024 * 1024;

pub(super) fn associated(minute: &ScopedSealedMinute<'_>) -> Result<DataFrame, Refused> {
    let result = associated_read(minute);
    if result.is_err() {
        // Preserve a commitment failure when the event reader discovers changed privacy early.
        minute.revalidate_full()?;
    }

    result
}

fn associated_read(minute: &ScopedSealedMinute<'_>) -> Result<DataFrame, Refused> {
    let budget = minute.inputs.as_ref().map_or_else(
        || Arc::new(ByteBudget::new(WORKING_BYTES_MAX)),
        |inputs| Arc::clone(inputs.budget),
    );
    let _observation =
        persistence::work::ingestion::minute_source(&minute.manifest, Some("events"));
    let expected = minute
        .rows("events")
        .ok_or(persistence::Error::Bound("events table"))?;
    if expected > collector::u64_from_usize(ROWS_MAX) {
        return Err(persistence::Error::Bound("event rows").into());
    }

    associated_check(minute)?;
    let table = EventTable::open_budgeted(&minute.root, &minute.manifest, &budget)
        .map_err(associated_refused)?;
    let mut associated = EventTable::empty_frame()?;
    let mut read = 0_u64;
    let count =
        u32::try_from(BATCH_ROWS_MAX).map_err(|_| persistence::Error::Bound("event batch rows"))?;
    for index in 0..table.len() {
        associated_check(minute)?;
        let part = table.part(index).map_err(associated_refused)?;
        let part = part
            .capture_with(&budget, |file, budget| {
                if let Some(inputs) = &minute.inputs {
                    (inputs.capture)(file, budget)
                } else {
                    AdmittedTable::from_file_budgeted(file, budget)
                }
            })
            .map_err(associated_refused)?;
        associated_check(minute)?;
        let slices = part.rows().div_ceil(u64::from(count)).max(1);
        for slice in 0..slices {
            associated_check(minute)?;
            let frame = part
                .read_slice(slice * u64::from(count), count, &budget)
                .map_err(associated_refused)?;
            read += collector::u64_from_usize(frame.rows().height());
            let frame = frame.associated(&budget).map_err(associated_refused)?;
            let kept = associated.height() + frame.height();
            if collector::u64_from_usize(kept) > persistence::schema::IDENTITY_ROWS_MAX {
                return Err(persistence::Error::Bound("process identity rows").into());
            }
            associated
                .vstack_mut_owned(frame)
                .map_err(persistence::Error::from)?;
        }
    }
    if read != expected {
        return Err(persistence::Error::Bound("event row count").into());
    }
    associated_check(minute)?;

    Ok(associated)
}

fn associated_check(minute: &ScopedSealedMinute<'_>) -> Result<(), Refused> {
    if let Some(inputs) = &minute.inputs {
        inputs.operation.check()?;
    }
    Ok(())
}

fn associated_refused(error: AdmittedReadRefusal) -> Refused {
    match error {
        AdmittedReadRefusal::Admission(error) => Refused::Input(error),
        AdmittedReadRefusal::Decode(error) => Refused::Decode(error),
    }
}
