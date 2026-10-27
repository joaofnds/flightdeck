//! The Apps of a sealed minute whose App parts the store does not read, by the App rule over each
//! observation's frame of the minute's processes and events. The rows pass the identity
//! admission rule the SQL replay of a minute applies, and the replay's own assembly builds the
//! frames, so the two give the same frames.

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

use super::apps::AppMinute;
use super::{Refused, ScopedSealedMinute as SealedMinute, SealedTable, ordered, placement};
use crate::apps::Stamp;
use crate::cancellation::{Cancellation, Operation};
use crate::live_app_response::Sample;
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

/// The last observation of a minute and its frame, which the first sample of the minute after it
/// may take its rates from.
pub struct Before {
    endpoint: Endpoint,
    frame: Box<Frame>,
}

impl Before {
    pub(crate) fn bytes(&self) -> usize {
        self.frame.bytes()
    }

    pub(crate) fn into_parts(self) -> (Endpoint, Box<Frame>) {
        (self.endpoint, self.frame)
    }
}

/// Every App of each of `minute`'s observations, which `into`, empty, takes, by the App rule over
/// the minute's processes and events, as the seal derives a minute's App parts. Each sample takes
/// its rates from the observation before it where the rule allows, the first from `before`, the
/// last observation of the minute read before this one, which `before` then holds of this one. A
/// refused minute leaves both empty, since neither holds a whole minute.
pub fn derive_apps(
    minute: &SealedMinute,
    before: &mut Option<Before>,
    into: &mut AppMinute,
) -> Result<(), Refused> {
    assert!(into.samples.is_empty());
    assert!(into.apps.is_empty());

    let derived = derive_apps_samples(minute, before, into);
    if derived.is_err() {
        *before = None;
        into.clear();
    }
    derived
}

fn derive_apps_samples(
    minute: &SealedMinute,
    before: &mut Option<Before>,
    into: &mut AppMinute,
) -> Result<(), Refused> {
    minute.visit_samples(before, |_, captured| {
        let captured = captured?;
        into.push_sample(captured);
        // A minute's Apps fit the rows its App parts would keep of them.
        if into.samples.len() + into.apps.len()
            > collector::usize_from_u32(persistence::app_tables::PARTS_ROWS_MAX)
        {
            return Err(persistence::Error::Bound("derived App rows"));
        }

        Ok(())
    })
}

impl SealedMinute<'_> {
    /// Retains valid source input even when joint derivation or presentation reaches its bound.
    /// Integrity failures still break the predecessor, by the same rule as native ingestion.
    pub(crate) fn terminal_processes(
        &self,
        operation: &Operation,
    ) -> Result<Option<Before>, Refused> {
        let mut before: Option<Before> = None;
        read_processes_with_operation(self, operation, |endpoint, frame| {
            let previous = before
                .as_ref()
                .filter(|before| {
                    frame
                        .observation()
                        .can_use_predecessor(&before.endpoint.observation())
                })
                .map(|before| &*before.frame);
            let valid_input = match Sample::validate_capture_typed(&frame, previous) {
                Ok(()) => true,
                Err(error) => error.retains_input(),
            };
            if valid_input {
                before = Some(Before {
                    endpoint: *endpoint,
                    frame,
                });
            } else {
                before = None;
            }
            Ok(())
        })?;

        Ok(before)
    }

    /// Visits each observation's typed sample or refusal, including unattributed processes.
    /// A refused presentation preserves valid input without suppressing later observations.
    pub(crate) fn visit_samples(
        &self,
        before: &mut Option<Before>,
        consume: impl FnMut(&Endpoint, persistence::Result<&Sample>) -> persistence::Result<()>,
    ) -> Result<(), Refused> {
        let operation = Arc::new(Cancellation::default()).operation(0)?;
        self.visit_samples_with_operation(before, &operation, consume)
    }

    pub(crate) fn visit_samples_with_operation(
        &self,
        before: &mut Option<Before>,
        operation: &Operation,
        mut consume: impl FnMut(&Endpoint, persistence::Result<&Sample>) -> persistence::Result<()>,
    ) -> Result<(), Refused> {
        let mut sample = None;
        let result = read_processes_with_operation(self, operation, |endpoint, frame| {
            let previous = before
                .as_ref()
                .filter(|before| {
                    frame
                        .observation()
                        .can_use_predecessor(&before.endpoint.observation())
                })
                .map(|before| &*before.frame);
            match Sample::capture_typed(&frame, previous, &mut sample) {
                Ok(()) => {
                    let captured = sample
                        .as_deref()
                        .ok_or(persistence::Error::Bound("derived App sample"))?;
                    consume(endpoint, Ok(captured))?;
                }
                Err(error) => {
                    let valid_input = error.retains_input();
                    consume(endpoint, Err(error.legacy()))?;
                    if !valid_input {
                        *before = None;
                        return Ok(());
                    }
                }
            }
            *before = Some(Before {
                endpoint: *endpoint,
                frame,
            });
            Ok(())
        });
        if result.is_err() {
            *before = None;
        }
        result
    }
}

/// A minute's processes and events as rows of the columns the assembly reads, with each
/// observation's endpoint and the order that places each row with its observation.
pub(crate) struct ProcessBatch {
    pub(crate) endpoints: Vec<Endpoint>,
    pub(crate) rows: DataFrame,
    pub(crate) order: Vec<usize>,
}

/// Hands `consume` the endpoint and the frame of each of `minute`'s observations, in publication
/// order.
#[cfg(test)]
pub(crate) fn read_processes(
    minute: &SealedMinute,
    consume: impl FnMut(&Endpoint, Box<Frame>) -> persistence::Result<()>,
) -> Result<(), Refused> {
    let operation = Arc::new(Cancellation::default()).operation(0)?;
    read_processes_with_operation(minute, &operation, consume)
}

fn read_processes_with_operation(
    minute: &SealedMinute,
    operation: &Operation,
    consume: impl FnMut(&Endpoint, Box<Frame>) -> persistence::Result<()>,
) -> Result<(), Refused> {
    operation.check()?;
    let batch = read_processes_batch(minute)?;
    operation.check()?;
    Frame::placed_many(
        &batch.endpoints,
        &batch.rows,
        &batch.order,
        operation,
        consume,
    )?;

    Ok(())
}

/// The rows of `minute`'s processes and events, each placed with its observation.
pub(crate) fn read_processes_batch(minute: &SealedMinute) -> Result<ProcessBatch, Refused> {
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
    Ok(ProcessBatch {
        endpoints,
        rows,
        order,
    })
}

/// The endpoint of each of `minute`'s ticks, in the order the table holds them.
pub(super) fn read_processes_endpoints(minute: &SealedMinute) -> Result<Vec<Endpoint>, Refused> {
    let limit =
        u32::try_from(ENDPOINTS_MAX + 1).map_err(|_| persistence::Error::Bound("process ticks"))?;
    let ticks = minute.ticks()?;
    let frame = ticks.collect(&TICK_COLUMNS, Some(limit))?;

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
    ordered(&endpoints, "process tick order", |endpoint| {
        endpoint.stamp.publication
    })?;
    Ok(endpoints)
}

/// The rows of `table`, the minute's table `kind` of processes or events, in the columns the
/// assembly reads, with `origin` the columns where the two kinds' rows differ. Rows of a table
/// that holds the identity fields pass the identity admission rule, and a table sealed before
/// them holds processes of unsupported identity, as the SQL replay reads it.
fn read_processes_rows(
    mut table: SealedTable,
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
    table.plan = table.plan.limit(limit).select(columns);
    let frame = table.collect_plan()?;
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
