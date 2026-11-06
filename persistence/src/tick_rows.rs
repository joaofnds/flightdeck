//! A saved tick's process and event rows as the columns the model's App rule reads, for whichever
//! reader holds those rows: a store replaying a minute, or a persister that restarted and takes a
//! window's first rates from the minute before it.

use model::apps::{Creation, Measurement, ProgramUnavailable, Usage};
use model::columns::{EventRow, ExitRow, Full, ProcessRow, Refusal, TickColumns};
use model::programs::{Program, Programs};
use polars::prelude::{
    BinaryType, DataFrame, DataType, Expr, Int32Type, Null, PolarsPhysicalType, UInt8Type,
    UInt64Type, col, lit,
};

use crate::engine::Cells;
use crate::identity::validation_columns;
use crate::schema::{Table, availability, event_kind, identity_availability};
use crate::{Error, Result};

/// The usage columns of a process or event row, each by the name the row is read under.
pub const USAGE_COLUMNS: [&str; 14] = [
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

/// Every column `TickRows` reads.
pub const COLUMNS: [&str; 24] = [
    "publication",
    "app_origin",
    "app_event",
    "app_kind",
    "app_tick",
    "pid",
    "process_unique_id",
    "pid_version",
    "identity_availability",
    "executable_name",
    "app_path",
    "usage_availability",
    "awake_ns_at_start",
    "user_ns",
    "system_ns",
    "energy_nj",
    "disk_read_bytes",
    "disk_written_bytes",
    "idle_wakeups",
    "pageins",
    "resident_bytes",
    "footprint_bytes",
    "threads",
    "gpu_ns",
];

/// What `app_origin` says of a row of a process table.
const ORIGIN_PROCESS: u8 = 0;
/// What `app_origin` says of an event row.
const ORIGIN_EVENT: u8 = 1;

/// The columns a reader selects from a saved process table, among them what `validate` checks
/// where `identity_held` says the table has the identity fields. One sealed before them holds
/// processes of unsupported identity, as the SQL replay reads it. `gpu_held` says the same of the
/// process's GPU time.
#[must_use]
pub fn process_columns(identity_held: bool, gpu_held: bool) -> Vec<Expr> {
    let mut columns = if identity_held {
        validation_columns(Table::Processes)
    } else {
        process_columns_unidentified()
    };
    columns.push(col("pid"));
    columns.extend(USAGE_COLUMNS.map(col));
    columns.extend([
        col("publication"),
        lit(ORIGIN_PROCESS)
            .cast(DataType::UInt8)
            .alias("app_origin"),
        column_of_nulls("app_event", DataType::UInt64),
        column_of_nulls("app_kind", DataType::UInt8),
        column_of_nulls("app_tick", DataType::UInt64),
        col("threads"),
        if gpu_held {
            col("gpu_ns")
        } else {
            column_of_nulls("gpu_ns", DataType::UInt64)
        },
    ]);

    columns
}

/// The identity columns of a process table sealed before processes carried their identity, as the
/// SQL replay normalizes them.
fn process_columns_unidentified() -> Vec<Expr> {
    vec![
        lit(identity_availability::UNSUPPORTED)
            .cast(DataType::UInt8)
            .alias("identity_availability"),
        column_of_nulls("process_unique_id", DataType::UInt64),
        column_of_nulls("pid_version", DataType::Int32),
        column_of_nulls("executable_name", DataType::Binary),
        column_of_nulls("app_path", DataType::Binary),
        lit(availability::MEASURED)
            .cast(DataType::UInt8)
            .alias("usage_availability"),
    ]
}

/// The columns where a saved event's associated row differs from a process row.
pub fn event_columns() -> [Expr; 6] {
    [
        lit(ORIGIN_EVENT).cast(DataType::UInt8).alias("app_origin"),
        col("event").alias("app_event"),
        col("kind").alias("app_kind"),
        col("run_tick").alias("app_tick"),
        column_of_nulls("threads", DataType::Int32),
        column_of_nulls("gpu_ns", DataType::UInt64),
    ]
}

/// A column named `name` of type `data_type` holding no values.
fn column_of_nulls(name: &str, data_type: DataType) -> Expr {
    lit(Null {}).cast(data_type).alias(name)
}

/// Where a tick's rows lie among the rows given to `read_tick`.
pub struct TickPlace {
    pub publication: u64,
    pub run_tick: u64,
    pub events_first: u64,
    pub events_end: u64,
}

/// The columns of a minute's process and event rows the rule reads, by the names in `COLUMNS`.
pub struct TickRows<'frame> {
    publication: Cells<'frame, UInt64Type>,
    app_origin: Cells<'frame, UInt8Type>,
    app_event: Cells<'frame, UInt64Type>,
    app_kind: Cells<'frame, UInt8Type>,
    app_tick: Cells<'frame, UInt64Type>,
    pid: Cells<'frame, Int32Type>,
    process_unique_id: Cells<'frame, UInt64Type>,
    pid_version: Cells<'frame, Int32Type>,
    identity_availability: Cells<'frame, UInt8Type>,
    executable_name: Cells<'frame, BinaryType>,
    app_path: Cells<'frame, BinaryType>,
    usage_availability: Cells<'frame, UInt8Type>,
    usage: UsageRows<'frame>,
    threads: Cells<'frame, Int32Type>,
    gpu_ns: Cells<'frame, UInt64Type>,
}

/// The columns of a measured usage the rule reads.
struct UsageRows<'frame> {
    awake_ns_at_start: Cells<'frame, UInt64Type>,
    user_ns: Cells<'frame, UInt64Type>,
    system_ns: Cells<'frame, UInt64Type>,
    energy_nj: Cells<'frame, UInt64Type>,
    disk_read_bytes: Cells<'frame, UInt64Type>,
    disk_written_bytes: Cells<'frame, UInt64Type>,
    idle_wakeups: Cells<'frame, UInt64Type>,
    pageins: Cells<'frame, UInt64Type>,
    resident_bytes: Cells<'frame, UInt64Type>,
    footprint_bytes: Cells<'frame, UInt64Type>,
}

impl<'frame> TickRows<'frame> {
    pub fn new(frame: &'frame DataFrame) -> Result<Self> {
        Ok(Self {
            publication: Cells::named(frame, "publication")?,
            app_origin: Cells::named(frame, "app_origin")?,
            app_event: Cells::named(frame, "app_event")?,
            app_kind: Cells::named(frame, "app_kind")?,
            app_tick: Cells::named(frame, "app_tick")?,
            pid: Cells::named(frame, "pid")?,
            process_unique_id: Cells::named(frame, "process_unique_id")?,
            pid_version: Cells::named(frame, "pid_version")?,
            identity_availability: Cells::named(frame, "identity_availability")?,
            executable_name: Cells::named(frame, "executable_name")?,
            app_path: Cells::named(frame, "app_path")?,
            usage_availability: Cells::named(frame, "usage_availability")?,
            usage: UsageRows {
                awake_ns_at_start: Cells::named(frame, "awake_ns_at_start")?,
                user_ns: Cells::named(frame, "user_ns")?,
                system_ns: Cells::named(frame, "system_ns")?,
                energy_nj: Cells::named(frame, "energy_nj")?,
                disk_read_bytes: Cells::named(frame, "disk_read_bytes")?,
                disk_written_bytes: Cells::named(frame, "disk_written_bytes")?,
                idle_wakeups: Cells::named(frame, "idle_wakeups")?,
                pageins: Cells::named(frame, "pageins")?,
                resident_bytes: Cells::named(frame, "resident_bytes")?,
                footprint_bytes: Cells::named(frame, "footprint_bytes")?,
            },
            threads: Cells::named(frame, "threads")?,
            gpu_ns: Cells::named(frame, "gpu_ns")?,
        })
    }
}

/// Reads into `columns` the rows at the start of `order` that belong to `place`'s tick, and
/// returns how many there are.
pub fn read_tick(
    rows: &TickRows<'_>,
    order: &[usize],
    place: &TickPlace,
    programs: &mut Programs,
    columns: &mut TickColumns,
) -> Result<usize> {
    let mut next = place.events_first;
    let mut count = 0;
    for &row in order {
        if rows.publication.get(row)? != place.publication {
            break;
        }
        count += 1;

        match required(&rows.app_origin, row)? {
            ORIGIN_PROCESS => {
                let process = read_tick_process(rows, row, programs)?;
                columns.push_process(&process).map_err(process_refused)?;
            }
            ORIGIN_EVENT => read_tick_event(rows, row, place, &mut next, programs, columns)?,
            _ => return Err(Error::Bound("sealed tick row origin")),
        }
    }
    Ok(count)
}

fn read_tick_process(
    rows: &TickRows<'_>,
    row: usize,
    programs: &mut Programs,
) -> Result<ProcessRow> {
    Ok(ProcessRow {
        pid: required(&rows.pid, row)?,
        creation: creation(rows, row)?,
        program: program(rows, row, programs)?,
        usage: usage(rows, row)?,
        gpu_ns: rows.gpu_ns.optional(row)?,
        // The collector writes a negative count where it read none.
        threads: rows
            .threads
            .optional(row)?
            .and_then(|threads| u64::try_from(threads).ok()),
    })
}

/// Reads the event of `row` into `columns`, its number at least `next` and in `place`'s
/// range, and moves `next` past it.
fn read_tick_event(
    rows: &TickRows<'_>,
    row: usize,
    place: &TickPlace,
    next: &mut u64,
    programs: &mut Programs,
    columns: &mut TickColumns,
) -> Result<()> {
    let number = required(&rows.app_event, row)?;
    if number < *next {
        return Err(Error::Bound("sealed tick event order"));
    }
    if number >= place.events_end {
        return Err(Error::Bound("sealed tick event range"));
    }
    if required(&rows.app_tick, row)? != place.run_tick {
        return Err(Error::Bound("sealed tick event tick"));
    }
    *next = number + 1;

    let pid = required(&rows.pid, row)?;
    let creation = creation(rows, row)?;
    match required(&rows.app_kind, row)? {
        event_kind::START => {
            let event = read_tick_program_event(rows, row, number, pid, creation, programs)?;
            columns.push_start(&event).map_err(event_refused)
        }
        event_kind::EXEC => {
            let event = read_tick_program_event(rows, row, number, pid, creation, programs)?;
            columns.push_exec(&event).map_err(event_refused)
        }
        event_kind::EXIT => {
            let exit = ExitRow {
                number,
                pid,
                creation,
                usage: usage(rows, row)?,
            };
            columns.push_exit(&exit).map_err(event_refused)
        }
        _ => Err(Error::Bound("sealed tick event kind")),
    }
}

/// The start or exec of `row`, which names a program and holds no usage.
fn read_tick_program_event(
    rows: &TickRows<'_>,
    row: usize,
    number: u64,
    pid: i32,
    creation: Option<Creation>,
    programs: &mut Programs,
) -> Result<EventRow> {
    if required(&rows.usage_availability, row)? != availability::NOT_APPLICABLE {
        return Err(Error::Bound("sealed tick event usage"));
    }
    Ok(EventRow {
        number,
        pid,
        creation,
        program: program(rows, row, programs)?,
    })
}

fn creation(rows: &TickRows<'_>, row: usize) -> Result<Option<Creation>> {
    match (
        rows.process_unique_id.optional(row)?,
        rows.pid_version.optional(row)?,
    ) {
        (Some(unique_id), Some(pid_version)) => Ok(Some(Creation {
            unique_id,
            pid_version,
        })),
        (None, None) => Ok(None),
        _ => Err(Error::Bound("sealed tick creation")),
    }
}

/// The App of the program `row` names, indexed in `programs` the first time a minute names it,
/// or why the collector did not read the program.
fn program(
    rows: &TickRows<'_>,
    row: usize,
    programs: &mut Programs,
) -> Result<std::result::Result<u32, ProgramUnavailable>> {
    let unavailable = match required(&rows.identity_availability, row)? {
        identity_availability::MEASURED => None,
        identity_availability::REFUSED => Some(ProgramUnavailable::Refused),
        identity_availability::GONE => Some(ProgramUnavailable::Gone),
        identity_availability::UNSUPPORTED => Some(ProgramUnavailable::Unsupported),
        identity_availability::CHANGED => Some(ProgramUnavailable::Changed),
        identity_availability::INVALID => Some(ProgramUnavailable::Invalid),
        identity_availability::EXHAUSTED => Some(ProgramUnavailable::Exhausted),
        _ => return Err(Error::Bound("sealed tick identity availability")),
    };
    if let Some(unavailable) = unavailable {
        return Ok(Err(unavailable));
    }

    let program = Program {
        executable_name: required(&rows.executable_name, row)?,
        app_path: rows.app_path.optional(row)?,
    };
    let app = programs
        .intern(program)
        .ok_or(Error::Bound("sealed tick program index"))?;
    Ok(Ok(app))
}

fn usage(rows: &TickRows<'_>, row: usize) -> Result<std::result::Result<Usage, Measurement>> {
    match required(&rows.usage_availability, row)? {
        availability::MEASURED => {
            let usage = &rows.usage;
            Ok(Ok(Usage {
                awake_ns_at_start: required(&usage.awake_ns_at_start, row)?,
                user_ns: required(&usage.user_ns, row)?,
                system_ns: required(&usage.system_ns, row)?,
                energy_nj: required(&usage.energy_nj, row)?,
                disk_read_bytes: required(&usage.disk_read_bytes, row)?,
                disk_written_bytes: required(&usage.disk_written_bytes, row)?,
                idle_wakeups: required(&usage.idle_wakeups, row)?,
                pageins: required(&usage.pageins, row)?,
                resident_bytes: required(&usage.resident_bytes, row)?,
                footprint_bytes: required(&usage.footprint_bytes, row)?,
            }))
        }
        availability::REFUSED => Ok(Err(Measurement::Refused)),
        availability::GONE => Ok(Err(Measurement::Gone)),
        _ => Err(Error::Bound("sealed tick usage availability")),
    }
}

fn required<'frame, T: PolarsPhysicalType>(
    cells: &Cells<'frame, T>,
    row: usize,
) -> Result<T::Physical<'frame>> {
    cells
        .optional(row)?
        .ok_or(Error::Bound("sealed tick missing field"))
}

/// The error of a process the columns refused: a tick of more processes than any tick holds,
/// or a pid not above the one before it.
fn process_refused(refusal: Refusal) -> Error {
    match refusal {
        Refusal::Full => Error::Bound("sealed tick process bound"),
        Refusal::Order => Error::Bound("sealed tick pid order"),
    }
}

/// The error of an event the columns refused: a tick of more events than any tick holds.
fn event_refused(_: Full) -> Error {
    Error::Bound("sealed tick event bound")
}
