//! The last tick saved before a window, read back from its minute for a derivation that did not
//! read it itself, which is every derivation's first window of a run after a restart. Reading the
//! saved rows and not the ring means one route whichever of the two still holds that tick.
use model::columns::TickColumns;
use model::programs::Programs;
use polars::prelude::{DataFrame, DataType, Expr, LazyFrame, Null, UInt64Type, col, lit};

use crate::canonical::ReadWork;
use crate::canonical::catalog::Catalog;
use crate::canonical::rows::{admission_error, read_error};
use crate::engine::{AdmittedTable, Cells};
use crate::identity::{validate, validation_columns};
use crate::schema::{PROCESS_GPU_FIELD, Table};
use crate::tick_rows::{COLUMNS, TickPlace, TickRows, USAGE_COLUMNS, read_tick};
use crate::window_ticks::{SeedTick, WindowTicks};
use crate::worker::SavedRecording;
use crate::{Error, Manifest, Observation, Result, Window};

/// Puts in `ticks` the last tick saved of the window's run before the window. A run's first window
/// has none. The App rule takes rates only from the tick just before, so a missed tick gives none.
pub(super) fn seed(
    ticks: &mut WindowTicks,
    programs: &mut Programs,
    window: &Window,
    saved: SavedRecording<'_>,
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
    let (seed_tick, place) = seed_last_tick(saved, &manifest)?;
    let frame = seed_rows(saved, &manifest, &place)?;
    let rows = TickRows::new(&frame)?;
    let order: Vec<usize> = (0..frame.height()).collect();

    ticks.seed(window, &seed_tick, |columns: &mut TickColumns| {
        let placed = read_tick(&rows, &order, &place, programs, columns)?;
        assert_eq!(placed, order.len());

        Ok(())
    })
}

fn seed_table(
    saved: SavedRecording<'_>,
    manifest: &Manifest,
    table: Table,
) -> Result<AdmittedTable> {
    let path = saved
        .root
        .join(&manifest.data_directory)
        .join(format!("{}.parquet", table.name()));
    let file = collector::recording::private::open_private_file(&path, false)?;

    AdmittedTable::from_file_budgeted(file, saved.budget)
        .map_err(|error| admission_error(error).into())
}

/// The newest tick of `manifest`'s window. A window's ticks lie in publication order.
fn seed_last_tick(saved: SavedRecording<'_>, manifest: &Manifest) -> Result<(SeedTick, TickPlace)> {
    let table = seed_table(saved, manifest, Table::Ticks)?;
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
    let last = frame
        .height()
        .checked_sub(1)
        .ok_or(Error::Bound("saved window without ticks"))?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?.get(last)?;
    let run_tick = Cells::<UInt64Type>::named(&frame, "run_tick")?.get(last)?;

    Ok((
        SeedTick {
            observation: Observation {
                publication,
                wall_ns: Cells::<UInt64Type>::named(&frame, "wall_ns")?.optional(last)?,
                continuous_ns: Cells::<UInt64Type>::named(&frame, "continuous_ns")?
                    .optional(last)?,
                awake_ns: Cells::<UInt64Type>::named(&frame, "awake_ns")?.optional(last)?,
            },
            run_tick,
        },
        TickPlace {
            publication,
            run_tick,
            events_first: Cells::<UInt64Type>::named(&frame, "events_first")?.get(last)?,
            events_end: Cells::<UInt64Type>::named(&frame, "events_end")?.get(last)?,
        },
    ))
}

/// The tick's process rows, which are all the App rule reads of a tick before the one it derives.
fn seed_rows(
    saved: SavedRecording<'_>,
    manifest: &Manifest,
    place: &TickPlace,
) -> Result<DataFrame> {
    let table = seed_table(saved, manifest, Table::Processes)?;
    let gpu_ns = if table.contains_column(PROCESS_GPU_FIELD.0) {
        col("gpu_ns")
    } else {
        seed_null("gpu_ns", DataType::UInt64)
    };
    let (plan, _columns) = table.into_scan()?;
    let mut columns = validation_columns(Table::Processes);
    columns.push(col("pid"));
    columns.extend(USAGE_COLUMNS.map(col));
    columns.extend([
        col("publication"),
        lit(0_u8).cast(DataType::UInt8).alias("app_origin"),
        seed_null("app_event", DataType::UInt64),
        seed_null("app_kind", DataType::UInt8),
        seed_null("app_tick", DataType::UInt64),
        col("threads"),
        gpu_ns,
    ]);
    let frame = seed_rows_of_tick(plan, place).select(columns).collect()?;
    validate(Table::Processes, &frame, 0..frame.height())?;

    Ok(frame.select(COLUMNS)?)
}

fn seed_rows_of_tick(plan: LazyFrame, place: &TickPlace) -> LazyFrame {
    plan.filter(col("publication").eq(lit(place.publication)))
}

/// A column named `name` of type `data_type` holding no values.
fn seed_null(name: &str, data_type: DataType) -> Expr {
    lit(Null {}).cast(data_type).alias(name)
}
