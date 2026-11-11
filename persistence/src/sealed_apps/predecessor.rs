//! The last tick saved before a window, read back from its minute for a derivation that did not
//! read it itself, which is every derivation's first window of a run after a restart. Reading the
//! saved rows and not the ring means one route whichever of the two still holds that tick. The
//! whole minute passes what a replay of it checks, so a minute a replay refuses lends no rates,
//! and this costs what the replay of one minute costs, once a restart.
use model::columns::TickColumns;
use model::programs::Programs;
use polars::prelude::{DataFrame, Expr, IntoLazy, LazyFrame, UInt64Type, col, lit};

use crate::canonical::ReadWork;
use crate::canonical::catalog::Catalog;
use crate::canonical::rows::{admission_error, admit_tick_file, read_error};
use crate::engine::{AdmittedTable, Cells};
use crate::event_observation::{BATCH_ROWS_MAX, ROWS_MAX};
use crate::event_table::EventTable;
use crate::event_table::query::ordinary_error;
use crate::identity::validate;
use crate::schema::{IDENTITY_FIELDS, IDENTITY_ROWS_MAX, PROCESS_GPU_FIELD, Table};
use crate::tick_rows::{COLUMNS, TickPlace, TickRows, event_columns, process_columns, read_tick};
use crate::window_ticks::WindowTicks;
use crate::worker::SavedRecording;
use crate::{Error, Manifest, Observation, Result, Window};

/// Puts in `ticks` the last tick saved of the window's run before the window. A run's first window
/// has none. The App rule takes rates only from the tick just before, so a missed tick gives none.
pub(super) fn seed(
    ticks: &mut WindowTicks,
    programs: &mut Programs,
    window: &Window,
    saved: &SavedRecording<'_>,
) -> Result<()> {
    let Some(first) = window.ticks.first() else {
        return Ok(());
    };
    let Some(before) = first.observation.publication.checked_sub(1) else {
        return Ok(());
    };
    let catalog = Catalog::open(saved.root, saved.budget)?;
    let manifest =
        catalog.window_at_or_before(&window.identity.run, before, &mut ReadWork::default())?;
    let Some(manifest) = manifest else {
        return Ok(());
    };
    let minute = seed_minute(saved, &manifest)?;
    let mut frame = seed_rows(saved, &manifest, &minute)?;
    frame.vstack_mut_owned(seed_events(saved, &manifest, &minute.last)?)?;
    let rows = TickRows::new(&frame)?;
    let order: Vec<usize> = (0..frame.height()).collect();

    ticks.seed(
        window,
        &minute.last_observation,
        minute.last.run_tick,
        |columns: &mut TickColumns| {
            let placed = read_tick(&rows, &order, &minute.last, programs, columns)?;
            assert_eq!(placed, order.len());

            Ok(())
        },
    )
}

/// What a saved minute's table of ticks says of it.
struct SeedMinute {
    /// The publication of each of its ticks, each after the one before.
    publications: Vec<u64>,
    /// Its last tick, which the window's first follows.
    last: TickPlace,
    last_observation: Observation,
}

fn seed_table(
    saved: &SavedRecording<'_>,
    manifest: &Manifest,
    table: Table,
) -> Result<AdmittedTable> {
    let file = seed_file(saved, manifest, table)?;

    AdmittedTable::from_file_streaming(file, saved.budget)
        .map_err(|error| admission_error(error).into())
}

fn seed_file(
    saved: &SavedRecording<'_>,
    manifest: &Manifest,
    table: Table,
) -> Result<std::fs::File> {
    let path = saved
        .root
        .join(&manifest.data_directory)
        .join(format!("{}.parquet", table.name()));

    Ok(collector::recording::private::open_private_file(
        &path, false,
    )?)
}

/// The ticks of `manifest`'s window, refused as a replay refuses them unless they stand in
/// publication order, each with an event range.
fn seed_minute(saved: &SavedRecording<'_>, manifest: &Manifest) -> Result<SeedMinute> {
    let file = seed_file(saved, manifest, Table::Ticks)?;
    let table = admit_tick_file(file, saved.budget, None)?;
    let frame = table
        .read_metadata_budgeted(
            &[
                "publication",
                "run_tick",
                "wall_ns",
                "continuous_ns",
                "awake_ns",
                "events_first",
                "events_end",
            ],
            None,
        )
        .map_err(read_error)?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let events_first = Cells::<UInt64Type>::named(&frame, "events_first")?;
    let events_end = Cells::<UInt64Type>::named(&frame, "events_end")?;
    let mut publications = Vec::with_capacity(frame.height());
    for row in 0..frame.height() {
        if events_first.get(row)? > events_end.get(row)? {
            return Err(Error::Bound("sealed tick event range"));
        }
        publications.push(publication.get(row)?);
    }
    if !publications.is_sorted_by(|before, after| before < after) {
        return Err(Error::Bound("process tick order"));
    }
    let last = frame
        .height()
        .checked_sub(1)
        .ok_or(Error::Bound("saved window without ticks"))?;

    Ok(SeedMinute {
        last_observation: Observation {
            publication: publication.get(last)?,
            wall_ns: Cells::<UInt64Type>::named(&frame, "wall_ns")?.optional(last)?,
            continuous_ns: Cells::<UInt64Type>::named(&frame, "continuous_ns")?.optional(last)?,
            awake_ns: Cells::<UInt64Type>::named(&frame, "awake_ns")?.optional(last)?,
        },
        last: TickPlace {
            publication: publication.get(last)?,
            run_tick: Cells::<UInt64Type>::named(&frame, "run_tick")?.get(last)?,
            events_first: events_first.get(last)?,
            events_end: events_end.get(last)?,
        },
        publications,
    })
}

/// The last tick's process rows, in the columns `read_tick` reads, once every row of the minute
/// has passed what a replay checks. The minute is read from its file a row group at a time, since
/// all of its rows at once are more than a seal holds where a Mac runs thousands of processes.
fn seed_rows(
    saved: &SavedRecording<'_>,
    manifest: &Manifest,
    minute: &SeedMinute,
) -> Result<DataFrame> {
    let table = seed_table(saved, manifest, Table::Processes)?;
    let identity_held = seed_rows_identity_held(&table)?;
    let gpu_held = table.contains_column(PROCESS_GPU_FIELD.0);
    let rows_saved = table.row_count();
    if rows_saved > IDENTITY_ROWS_MAX {
        return Err(Error::Bound("process identity rows"));
    }
    let saved_rows = SeedTable {
        names: table.fields().map(|(name, _)| name).collect(),
        table: &table,
        columns: process_columns(identity_held, gpu_held),
        identity_held,
    };
    let slice_rows = collector::u64_from_usize(SLICE_ROWS);
    let mut rows = seed_rows_slice(&saved_rows, minute, 0)?;
    for slice in 1..rows_saved.div_ceil(slice_rows) {
        let read = seed_rows_slice(&saved_rows, minute, slice * slice_rows)?;
        rows.of_tick.vstack_mut_owned(read.of_tick)?;
        rows.read += read.read;
    }
    if rows.read != rows_saved {
        return Err(Error::Bound("process row count"));
    }

    Ok(rows.of_tick)
}

/// The rows of a saved process table a seed holds at once, a row group as a seal writes one.
const SLICE_ROWS: usize = crate::encoder::ROW_GROUP_ROWS;

/// A saved process table and how a seed reads it.
struct SeedTable<'table> {
    table: &'table AdmittedTable,
    /// The names of the columns the table holds.
    names: Vec<&'table str>,
    /// What a seed selects from them.
    columns: Vec<Expr>,
    identity_held: bool,
}

/// What a seed read of one slice of a saved process table.
struct SeedSlice {
    /// The rows of the minute's last tick among those read.
    of_tick: DataFrame,
    /// How many rows it read.
    read: u64,
}

/// The slice of the minute's process rows that starts at row `offset`, refused unless each row
/// passes what a replay checks.
fn seed_rows_slice(saved: &SeedTable<'_>, minute: &SeedMinute, offset: u64) -> Result<SeedSlice> {
    let count = u32::try_from(SLICE_ROWS).map_err(|_| Error::Bound("process identity rows"))?;
    let frame = saved
        .table
        .read_slice_metadata_budgeted(&saved.names, offset, count)
        .map_err(read_error)?
        .lazy()
        .select(saved.columns.clone())
        .collect()?;
    if saved.identity_held {
        validate(Table::Processes, &frame, 0..frame.height())?;
    }
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    for row in 0..frame.height() {
        if minute
            .publications
            .binary_search(&publication.get(row)?)
            .is_err()
        {
            return Err(Error::Bound("process of no tick"));
        }
    }

    Ok(SeedSlice {
        read: collector::u64_from_usize(frame.height()),
        of_tick: seed_rows_of_tick(frame.lazy(), &minute.last)
            .select(COLUMNS.map(col))
            .collect()?,
    })
}

/// Whether `table` has the identity fields. A table with only some of them is refused.
fn seed_rows_identity_held(table: &AdmittedTable) -> Result<bool> {
    let held = IDENTITY_FIELDS
        .iter()
        .filter(|(name, _)| table.contains_column(name))
        .count();
    if held != 0 && held != IDENTITY_FIELDS.len() {
        return Err(Error::Bound("incomplete column group"));
    }

    Ok(held != 0)
}

fn seed_rows_of_tick(plan: LazyFrame, place: &TickPlace) -> LazyFrame {
    plan.filter(col("publication").eq(lit(place.publication)))
}

/// The last tick's events, in event order and the columns `read_tick` reads, once all the minute's
/// events have read. The App rule names a process it never read from its start or exec in the tick
/// before its exit, so a seed without them gives that exit no App where a replay gives it one.
fn seed_events(
    saved: &SavedRecording<'_>,
    manifest: &Manifest,
    place: &TickPlace,
) -> Result<DataFrame> {
    let expected = manifest
        .tables
        .iter()
        .find(|table| table.name == "events")
        .ok_or(Error::Bound("events table"))?
        .rows;
    if expected > collector::u64_from_usize(ROWS_MAX) {
        return Err(Error::Bound("event rows"));
    }
    let table =
        EventTable::open_budgeted(saved.root, manifest, saved.budget).map_err(ordinary_error)?;
    let mut rows = seed_events_of_tick(EventTable::empty_frame()?, place)?;
    let mut read = 0_u64;
    let mut associated = 0_u64;
    let count = u32::try_from(BATCH_ROWS_MAX).map_err(|_| Error::Bound("event batch rows"))?;
    for index in 0..table.len() {
        let part = table
            .part(index)
            .map_err(ordinary_error)?
            .capture_with(saved.budget, AdmittedTable::from_file_budgeted)
            .map_err(ordinary_error)?;
        for slice in 0..part.rows().div_ceil(u64::from(count)).max(1) {
            let frame = part
                .read_slice(slice * u64::from(count), count, saved.budget)
                .map_err(ordinary_error)?;
            read += collector::u64_from_usize(frame.rows().height());
            let frame = frame.associated(saved.budget).map_err(ordinary_error)?;
            associated += collector::u64_from_usize(frame.height());
            if associated > IDENTITY_ROWS_MAX {
                return Err(Error::Bound("process identity rows"));
            }
            rows.vstack_mut_owned(seed_events_of_tick(frame, place)?)?;
        }
    }
    if read != expected {
        return Err(Error::Bound("event row count"));
    }

    Ok(rows)
}

/// The rows of `frame`, which holds associated events, that are events of the tick.
fn seed_events_of_tick(frame: DataFrame, place: &TickPlace) -> Result<DataFrame> {
    Ok(seed_rows_of_tick(frame.lazy(), place)
        .with_columns(event_columns())
        .select(COLUMNS.map(col))
        .collect()?)
}
