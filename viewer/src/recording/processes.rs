//! Each observation's frame of a sealed minute, the processes and events the App rule takes, for
//! a minute whose App parts the store does not read. The rows pass the identity admission rule
//! the SQL replay of a minute applies, and the replay's own assembly builds the frames, so the
//! two give the same frames.

use std::sync::Arc;

use persistence::engine::Cells;
use persistence::identity::{validate, validation_columns};
use persistence::schema::{
    IDENTITY_FIELDS, IDENTITY_ROWS_MAX, PROCESS_GPU_FIELD, Table, availability,
    identity_availability,
};
use polars::prelude::{
    DataFrame, DataType, Expr, Null, SortMultipleOptions, UInt32Type, UInt64Type, col, lit,
};

use super::{Refused, SealedMinute, SealedTable, placement};
use crate::apps::Stamp;
use crate::cancellation::Cancellation;
use crate::live_apps::Frame;
use crate::live_apps::committed::{ENDPOINTS_MAX, Endpoint};

/// The columns of a process or event row the frames take besides its identity, each by the
/// name the assembly reads it under.
const USAGE_COLUMNS: [&str; 14] = [
    "awake_ns_at_start",
    "user_ns",
    "system_ns",
    "energy_nj",
    "instructions",
    "super_instructions",
    "cycles",
    "idle_wakeups",
    "interrupt_wakeups",
    "pageins",
    "resident_bytes",
    "footprint_bytes",
    "disk_read_bytes",
    "disk_written_bytes",
];
/// Every column the assembly reads of a row.
const BATCH_COLUMNS: [&str; 28] = [
    "identity_availability",
    "process_unique_id",
    "pid_version",
    "executable_name",
    "app_path",
    "usage_availability",
    "pid",
    "awake_ns_at_start",
    "user_ns",
    "system_ns",
    "energy_nj",
    "instructions",
    "super_instructions",
    "cycles",
    "idle_wakeups",
    "interrupt_wakeups",
    "pageins",
    "resident_bytes",
    "footprint_bytes",
    "disk_read_bytes",
    "disk_written_bytes",
    "publication",
    "app_origin",
    "app_event",
    "app_kind",
    "app_tick",
    "threads",
    "gpu_ns",
];
/// The tick columns of an observation's endpoint.
const TICK_COLUMNS: [&str; 9] = [
    "publication",
    "run_tick",
    "wall_ns",
    "continuous_ns",
    "awake_ns",
    "processes_refused",
    "processes_gone",
    "events_first",
    "events_end",
];
const TICKLESS: &str = "process of no tick";

/// Hands `consume` the frame of each of `minute`'s observations, in publication order.
pub fn read_processes(
    minute: &SealedMinute,
    consume: impl FnMut(Box<Frame>) -> persistence::Result<()>,
) -> Result<(), Refused> {
    let endpoints = read_processes_endpoints(minute)?;

    let processes = minute.table(Table::Processes)?;
    let gpu_ns = if processes.holds(&[PROCESS_GPU_FIELD.0])? {
        col("gpu_ns")
    } else {
        null("gpu_ns", DataType::UInt64)
    };
    let mut rows = read_processes_rows(
        processes,
        Table::Processes,
        [
            lit(0_u8).cast(DataType::UInt8).alias("app_origin"),
            null("app_event", DataType::UInt64),
            null("app_kind", DataType::UInt8),
            null("app_tick", DataType::UInt64),
            col("threads"),
            gpu_ns,
        ],
    )?;

    // The assembly takes each observation's events in the order of their numbers.
    let mut events = minute.table(Table::Events)?;
    events.plan = events.plan.sort(
        ["event"],
        SortMultipleOptions {
            descending: vec![false],
            nulls_last: vec![false],
            multithreaded: false,
            maintain_order: true,
            limit: None,
        },
    );
    let events = read_processes_rows(
        events,
        Table::Events,
        [
            lit(1_u8).cast(DataType::UInt8).alias("app_origin"),
            col("event").alias("app_event"),
            col("kind").alias("app_kind"),
            col("run_tick").alias("app_tick"),
            null("threads", DataType::Int32),
            null("gpu_ns", DataType::UInt64),
        ],
    )?;
    rows.vstack_mut_owned(events)
        .map_err(persistence::Error::from)?;
    rows.rechunk_mut();

    let publications: Vec<u64> = endpoints
        .iter()
        .map(|endpoint| endpoint.stamp.publication)
        .collect();
    let publication = Cells::<UInt64Type>::named(&rows, "publication")?;
    let order = placement(&publications, &publication, rows.height(), TICKLESS)?;
    let operation = Arc::new(Cancellation::default()).operation(0)?;
    Frame::placed_many(&endpoints, &rows, &order, &operation, consume)?;
    Ok(())
}

/// The endpoint of each of `minute`'s ticks, in the order the table holds them.
fn read_processes_endpoints(minute: &SealedMinute) -> Result<Vec<Endpoint>, Refused> {
    let limit =
        u32::try_from(ENDPOINTS_MAX + 1).map_err(|_| persistence::Error::Bound("process ticks"))?;
    let mut ticks = minute.table(Table::Ticks)?;
    ticks.plan = ticks.plan.limit(limit);
    let frame = ticks.collect(&TICK_COLUMNS)?;

    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let run_tick = Cells::<UInt64Type>::named(&frame, "run_tick")?;
    let wall_ns = Cells::<UInt64Type>::named(&frame, "wall_ns")?;
    let continuous_ns = Cells::<UInt64Type>::named(&frame, "continuous_ns")?;
    let awake_ns = Cells::<UInt64Type>::named(&frame, "awake_ns")?;
    let refused = Cells::<UInt32Type>::named(&frame, "processes_refused")?;
    let gone = Cells::<UInt32Type>::named(&frame, "processes_gone")?;
    let events_first = Cells::<UInt64Type>::named(&frame, "events_first")?;
    let events_end = Cells::<UInt64Type>::named(&frame, "events_end")?;
    let identity = minute.manifest.identity.run;
    let mut endpoints = Vec::with_capacity(frame.height());
    for row in 0..frame.height() {
        endpoints.push(Endpoint {
            identity,
            stamp: Stamp {
                boot: identity.boot.0,
                run: identity.run.0,
                publication: publication.get(row)?,
                continuous_ns: continuous_ns.optional(row)?,
                awake_ns: awake_ns.optional(row)?,
            },
            run_tick: run_tick.get(row)?,
            wall_ns: wall_ns.optional(row)?,
            usage_refused: refused.get(row)?,
            usage_gone: gone.get(row)?,
            events_first: events_first.get(row)?,
            events_end: events_end.get(row)?,
        });
    }
    Ok(endpoints)
}

/// The rows of `table`, the minute's table `kind` of processes or events, in the columns the
/// assembly reads, with `origin` the columns where the two kinds' rows differ. Rows of a table
/// that holds the identity fields pass the identity admission rule, and a table sealed before
/// them holds processes of unsupported identity, as the SQL replay reads it.
fn read_processes_rows(
    table: SealedTable,
    kind: Table,
    origin: [Expr; 6],
) -> Result<DataFrame, Refused> {
    let held = table.holds(&IDENTITY_FIELDS.map(|(name, _)| name))?;
    let mut columns = if held {
        validation_columns(kind)
    } else {
        read_processes_rows_legacy(kind)
    };
    columns.push(col("pid"));
    columns.extend(USAGE_COLUMNS.map(col));
    columns.push(col("publication"));
    columns.extend(origin);

    let limit = u32::try_from(IDENTITY_ROWS_MAX + 1)
        .map_err(|_| persistence::Error::Bound("process identity rows"))?;
    let frame = table
        .plan
        .limit(limit)
        .select(columns)
        .collect()
        .map_err(persistence::Error::from)?;
    if collector::u64_from_usize(frame.height()) > IDENTITY_ROWS_MAX {
        return Err(persistence::Error::Bound("process identity rows").into());
    }

    if held {
        validate(kind, &frame, 0..frame.height())?;
    }
    Ok(frame
        .select(BATCH_COLUMNS)
        .map_err(persistence::Error::from)?)
}

/// The identity columns of a process or event table sealed before processes carried their
/// identity, as the SQL replay normalizes them.
fn read_processes_rows_legacy(kind: Table) -> Vec<Expr> {
    let usage = match kind {
        Table::Events => col("availability"),
        _ => lit(availability::MEASURED),
    };
    vec![
        lit(identity_availability::UNSUPPORTED)
            .cast(DataType::UInt8)
            .alias("identity_availability"),
        null("process_unique_id", DataType::UInt64),
        null("pid_version", DataType::Int32),
        null("executable_name", DataType::Binary),
        null("app_path", DataType::Binary),
        usage.cast(DataType::UInt8).alias("usage_availability"),
    ]
}

/// A column named `name` of type `data_type` holding no values.
fn null(name: &str, data_type: DataType) -> Expr {
    lit(Null {}).cast(data_type).alias(name)
}
