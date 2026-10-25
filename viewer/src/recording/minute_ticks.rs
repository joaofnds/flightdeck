//! The ticks of a sealed minute as the columns the model's App rule reads, each beside the tick
//! before it, so a store or the persister derives a minute's Apps from its processes and events
//! without building frames.

use model::apps::{Creation, Measurement, ProgramUnavailable, Stamp, Tick, Usage};
use model::columns::{EventRow, ExitRow, Full, ProcessRow, Refusal, TickColumns};
use model::programs::{Program, Programs};
use persistence::engine::Cells;
use persistence::schema::{availability, event_kind, identity_availability};
use persistence::{Error, Result};
use polars::prelude::{
    BinaryType, DataFrame, Int32Type, PolarsPhysicalType, UInt8Type, UInt64Type,
};

use super::processes::read_processes_batch;
use super::{Refused, ScopedSealedMinute as SealedMinute};
use crate::live_apps::committed::{ENDPOINTS_MAX, Endpoint};

/// A tick of a sealed minute beside the tick before it, as the App rule reads them, with the
/// index that numbers their programs.
pub struct MinuteTick<'ticks> {
    pub programs: &'ticks Programs,
    pub previous: Tick<'ticks>,
    pub selected: Tick<'ticks>,
}

/// The columns of the ticks of sealed minutes, which keep the tick read last as the one before
/// the next, so the first tick of a minute takes its rates from the minute before it.
pub struct MinuteTicks {
    /// The tick before the one read last, then the one read last.
    pair: [TickColumns; 2],
}

impl Default for MinuteTicks {
    fn default() -> Self {
        let processes = model::u32_from_length(crate::apps::PROCESS_INTERVALS_MAX);
        let events = model::u32_from_length(collector::tracker::EVENTS_PER_TICK_MAX);
        Self {
            pair: [
                TickColumns::new(processes, events),
                TickColumns::new(processes, events),
            ],
        }
    }
}

impl MinuteTicks {
    /// Hands `consume` each tick of `minute` in publication order beside the tick before it,
    /// the first beside the last tick read before this minute, and indexes in `programs` each
    /// App the minute names. A refusal, `consume`'s included, leaves no tick to take rates from.
    pub fn read(
        &mut self,
        minute: &SealedMinute,
        programs: &mut Programs,
        consume: impl FnMut(&MinuteTick<'_>) -> Result<()>,
    ) -> std::result::Result<(), Refused> {
        let read = self.read_minute(minute, programs, consume);
        if read.is_err() {
            self.clear();
        }
        read
    }

    /// Forgets the tick read last, so the next tick takes no rates from it.
    pub fn clear(&mut self) {
        for columns in &mut self.pair {
            columns.clear();
        }
    }

    fn read_minute(
        &mut self,
        minute: &SealedMinute,
        programs: &mut Programs,
        mut consume: impl FnMut(&MinuteTick<'_>) -> Result<()>,
    ) -> std::result::Result<(), Refused> {
        let batch = read_processes_batch(minute)?;
        read_minute_endpoints(&batch.endpoints)?;
        let rows = Rows::new(&batch.rows)?;

        let mut placed = 0;
        for endpoint in &batch.endpoints {
            // The oldest columns take the tick, which then follows the one read before it.
            let columns = &mut self.pair[0];
            columns.start(stamp(endpoint));
            placed += read_tick(&rows, &batch.order[placed..], endpoint, programs, columns)?;
            columns.finish();
            self.pair.swap(0, 1);

            consume(&MinuteTick {
                programs,
                previous: self.pair[0].tick(),
                selected: self.pair[1].tick(),
            })?;
        }
        // The placement puts every row with one of the endpoints, in endpoint order.
        assert_eq!(placed, batch.order.len());
        Ok(())
    }
}

/// The columns of a minute's process and event rows the rule reads, by the names the batch
/// gives them.
struct Rows<'frame> {
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

impl<'frame> Rows<'frame> {
    fn new(frame: &'frame DataFrame) -> Result<Self> {
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

/// Refuses `endpoints` unless they are at most `ENDPOINTS_MAX` ticks, each with an event range.
/// The placement has already refused ticks out of publication order.
fn read_minute_endpoints(endpoints: &[Endpoint]) -> Result<()> {
    if endpoints.len() > ENDPOINTS_MAX {
        return Err(Error::Bound("sealed tick count"));
    }
    for endpoint in endpoints {
        if endpoint.events_first > endpoint.events_end {
            return Err(Error::Bound("sealed tick event range"));
        }
    }
    Ok(())
}

fn stamp(endpoint: &Endpoint) -> Stamp {
    Stamp {
        recording: endpoint.identity.recording.0,
        boot: endpoint.identity.boot.0,
        run: endpoint.identity.run.0,
        tick: endpoint.run_tick,
        continuous_ns: endpoint.stamp.continuous_ns,
    }
}

/// Reads into `columns` the rows at the start of `order` that belong to `endpoint`'s tick, and
/// returns how many there are.
fn read_tick(
    rows: &Rows<'_>,
    order: &[usize],
    endpoint: &Endpoint,
    programs: &mut Programs,
    columns: &mut TickColumns,
) -> Result<usize> {
    let mut next = endpoint.events_first;
    let mut count = 0;
    for &row in order {
        if rows.publication.get(row)? != endpoint.stamp.publication {
            break;
        }
        count += 1;

        match required(&rows.app_origin, row)? {
            0 => {
                let process = read_tick_process(rows, row, programs)?;
                columns.push_process(&process).map_err(process_refused)?;
            }
            1 => read_tick_event(rows, row, endpoint, &mut next, programs, columns)?,
            _ => return Err(Error::Bound("sealed tick row origin")),
        }
    }
    Ok(count)
}

fn read_tick_process(rows: &Rows<'_>, row: usize, programs: &mut Programs) -> Result<ProcessRow> {
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

/// Reads the event of `row` into `columns`, its number at least `next` and in `endpoint`'s
/// range, and moves `next` past it.
fn read_tick_event(
    rows: &Rows<'_>,
    row: usize,
    endpoint: &Endpoint,
    next: &mut u64,
    programs: &mut Programs,
    columns: &mut TickColumns,
) -> Result<()> {
    let number = required(&rows.app_event, row)?;
    if number < *next {
        return Err(Error::Bound("sealed tick event order"));
    }
    if number >= endpoint.events_end {
        return Err(Error::Bound("sealed tick event range"));
    }
    if required(&rows.app_tick, row)? != endpoint.run_tick {
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
    rows: &Rows<'_>,
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

fn creation(rows: &Rows<'_>, row: usize) -> Result<Option<Creation>> {
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
    rows: &Rows<'_>,
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

fn usage(rows: &Rows<'_>, row: usize) -> Result<std::result::Result<Usage, Measurement>> {
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
