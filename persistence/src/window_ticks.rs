//! The ticks of a window the persister staged, as the columns the model's App rule reads, each
//! beside the tick before it, so the persister derives a window's Apps from what it staged
//! without reading back the tables it wrote.
use collector::ring::ProcessCreation;
use model::apps::{Creation, Measurement, ProgramUnavailable, Stamp, Tick, Usage};
use model::columns::{EventRow, ExitRow, Full, ProcessRow, Refusal, TickColumns};
use model::programs::{Program, Programs};

use crate::owned_identity::{self, OwnedIdentity};
use crate::schema::{availability, event_kind, identity_availability};
use crate::window::{EventRecord, PROCESSES_MAX, StagedTick};
use crate::{Error, Observation, Result, Window};

/// A tick of a window beside the tick before it, as the App rule reads them, with the index
/// that numbers their programs.
pub struct WindowTick<'ticks> {
    pub observation: Observation,
    pub programs: &'ticks Programs,
    pub previous: Tick<'ticks>,
    pub selected: Tick<'ticks>,
}

/// The columns of the ticks of windows, which keep the tick read last as the one before the
/// next, so the first tick of a window takes its rates from the window before it.
pub struct WindowTicks {
    /// The tick before the one read last, then the one read last.
    pair: [TickColumns; 2],
    /// The live processes of the tick being read, before they are put in pid order.
    processes: Vec<ProcessRow>,
    /// The entry in the run's index of each program of the window being read, none until a
    /// row names it.
    entries: Vec<Option<u32>>,
}

impl Default for WindowTicks {
    fn default() -> Self {
        let processes = model::u32_from_length(PROCESSES_MAX);
        let events = model::u32_from_length(collector::tracker::EVENTS_PER_TICK_MAX);
        Self {
            pair: [
                TickColumns::new(processes, events),
                TickColumns::new(processes, events),
            ],
            processes: Vec::with_capacity(PROCESSES_MAX),
            entries: vec![None; owned_identity::PROGRAMS_MAX],
        }
    }
}

impl WindowTicks {
    /// Hands `consume` each tick of `window` in publication order beside the tick before it,
    /// the first beside the last tick read before this window, and indexes in `programs`, the
    /// index of the window's run, each program the window names. A refusal, `consume`'s
    /// included, leaves no tick to take rates from.
    pub fn read(
        &mut self,
        window: &Window,
        programs: &mut Programs,
        consume: impl FnMut(&WindowTick<'_>) -> Result<()>,
    ) -> Result<()> {
        let read = self.read_window(window, programs, consume);
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

    fn read_window(
        &mut self,
        window: &Window,
        programs: &mut Programs,
        mut consume: impl FnMut(&WindowTick<'_>) -> Result<()>,
    ) -> Result<()> {
        self.entries.fill(None);
        let mut index = Index {
            window: window.programs.as_deref(),
            entries: &mut self.entries,
            programs,
        };
        // The processes table keeps each process's GPU time only where a tick of the window
        // read the system.
        let gpu_read = window.system_captured();

        for tick in &window.ticks {
            // The oldest columns take the tick, which then follows the one read before it.
            let columns = &mut self.pair[0];
            columns.start(stamp(window, tick));
            read_tick_processes(tick, gpu_read, &mut index, &mut self.processes)?;
            for process in &self.processes {
                columns.push_process(process).map_err(process_refused)?;
            }
            read_tick_events(tick, &mut index, columns)?;
            columns.finish();
            self.pair.swap(0, 1);

            consume(&WindowTick {
                observation: tick.observation,
                programs: index.programs,
                previous: self.pair[0].tick(),
                selected: self.pair[1].tick(),
            })?;
        }
        Ok(())
    }
}

/// The run's index of programs, and where each program of the window lies in it.
struct Index<'read> {
    /// The window's own dictionary, none in a window of a source that names no programs.
    window: Option<&'read owned_identity::Programs>,
    entries: &'read mut [Option<u32>],
    programs: &'read mut Programs,
}

impl Index<'_> {
    /// The entry of the program `identity` names, or why the collector did not read it.
    fn program(
        &mut self,
        identity: Option<&OwnedIdentity>,
    ) -> Result<std::result::Result<u32, ProgramUnavailable>> {
        let Some(identity) = identity else {
            return Ok(Err(ProgramUnavailable::Unsupported));
        };
        let Some(program) = identity.program else {
            return Ok(Err(program_unavailable(identity.availability)));
        };
        assert_eq!(identity.availability, identity_availability::MEASURED);

        if let Some(entry) = self.entries[program] {
            return Ok(Ok(entry));
        }
        let Some(window) = self.window else {
            unreachable!("a window with identities holds their programs");
        };
        let entry = self
            .programs
            .intern(Program {
                executable_name: window.name(program),
                app_path: window.app(program),
            })
            .ok_or(Error::Bound("window program index"))?;
        self.entries[program] = Some(entry);
        Ok(Ok(entry))
    }
}

fn stamp(window: &Window, tick: &StagedTick) -> Stamp {
    let run = window.identity.run;
    Stamp {
        recording: run.recording.0,
        boot: run.boot.0,
        run: run.run.0,
        tick: tick.run_tick,
        continuous_ns: tick.observation.continuous_ns,
    }
}

/// Fills `rows` with the live processes of `tick`, in pid order.
fn read_tick_processes(
    tick: &StagedTick,
    gpu_read: bool,
    index: &mut Index<'_>,
    rows: &mut Vec<ProcessRow>,
) -> Result<()> {
    rows.clear();
    match &tick.identities {
        Some(identities) => {
            // The measured processes are the identities with measured usage, in their order.
            let mut measured = tick.processes.iter();
            for (position, identity) in identities.processes.iter().enumerate() {
                let process = if identity.usage == availability::MEASURED {
                    let Some(process) = measured.next() else {
                        unreachable!("the window holds a process for each measured identity");
                    };
                    assert_eq!(process.pid, identity.pid);
                    Some(process)
                } else {
                    None
                };
                let gpu_ns = if gpu_read {
                    tick.process_gpu_ns.get(position).copied().flatten()
                } else {
                    None
                };
                rows.push(ProcessRow {
                    pid: identity.pid,
                    creation: identity.creation.map(creation),
                    program: index.program(Some(identity))?,
                    usage: process_usage(identity.usage, process),
                    gpu_ns,
                    threads: process.and_then(threads),
                });
            }
            assert!(measured.next().is_none());
        }
        None => {
            for process in &tick.processes {
                rows.push(ProcessRow {
                    pid: process.pid,
                    creation: None,
                    program: index.program(None)?,
                    usage: Ok(usage(&process.usage)),
                    gpu_ns: None,
                    threads: threads(process),
                });
            }
        }
    }
    rows.sort_unstable_by_key(|row| row.pid);
    Ok(())
}

/// Adds the events of `tick` to `columns`, each by its kind.
fn read_tick_events(
    tick: &StagedTick,
    index: &mut Index<'_>,
    columns: &mut TickColumns,
) -> Result<()> {
    let identities = tick.identities.as_ref().map(|rows| rows.events.as_slice());
    if let Some(identities) = identities {
        assert_eq!(identities.len(), tick.events.len());
    }

    for (position, event) in tick.events.iter().enumerate() {
        let identity = identities.map(|rows| &rows[position]);
        if let Some(identity) = identity {
            assert_eq!(identity.pid, event.pid);
        }
        let creation = identity.and_then(|value| value.creation).map(creation);
        match event.kind {
            event_kind::START => {
                let start = read_tick_program_event(event, creation, identity, index)?;
                columns.push_start(&start).map_err(event_refused)?;
            }
            event_kind::EXEC => {
                let exec = read_tick_program_event(event, creation, identity, index)?;
                columns.push_exec(&exec).map_err(event_refused)?;
            }
            event_kind::EXIT => {
                let exit = ExitRow {
                    number: event.number,
                    pid: event.pid,
                    creation,
                    usage: exit_usage(event),
                };
                columns.push_exit(&exit).map_err(event_refused)?;
            }
            _ => unreachable!("the window stages only starts, execs and exits"),
        }
    }
    Ok(())
}

/// The start or exec of `event`, which names a program and holds no usage.
fn read_tick_program_event(
    event: &EventRecord,
    creation: Option<Creation>,
    identity: Option<&OwnedIdentity>,
    index: &mut Index<'_>,
) -> Result<EventRow> {
    assert_eq!(event.availability, availability::NOT_APPLICABLE);

    Ok(EventRow {
        number: event.number,
        pid: event.pid,
        creation,
        program: index.program(identity)?,
    })
}

/// The usage of a live process the collector read `code` of, which is `process` where it
/// measured it.
fn process_usage(
    code: u8,
    process: Option<&collector::processes::Process>,
) -> std::result::Result<Usage, Measurement> {
    match (code, process) {
        (availability::MEASURED, Some(process)) => Ok(usage(&process.usage)),
        (availability::REFUSED, None) => Err(Measurement::Refused),
        (availability::GONE, None) => Err(Measurement::Gone),
        _ => unreachable!("the window stages a live process's usage with its availability"),
    }
}

/// The usage an exit read last.
fn exit_usage(event: &EventRecord) -> std::result::Result<Usage, Measurement> {
    match (event.availability, &event.usage) {
        (availability::MEASURED, Some(last)) => Ok(usage(last)),
        (availability::REFUSED, None) => Err(Measurement::Refused),
        (availability::GONE, None) => Err(Measurement::Gone),
        _ => unreachable!("the window stages an exit's usage with its availability"),
    }
}

fn usage(source: &collector::processes::Usage) -> Usage {
    Usage {
        awake_ns_at_start: source.awake_ns_at_start,
        user_ns: source.user_ns,
        system_ns: source.system_ns,
        energy_nj: source.energy_nj,
        disk_read_bytes: source.disk_read_bytes,
        disk_written_bytes: source.disk_written_bytes,
        idle_wakeups: source.idle_wakeups,
        pageins: source.pageins,
        resident_bytes: source.resident_bytes,
        footprint_bytes: source.footprint_bytes,
    }
}

fn creation(source: ProcessCreation) -> Creation {
    Creation {
        unique_id: source.unique_id,
        pid_version: source.pid_version,
    }
}

/// The threads of `process`, none where the collector wrote a negative count for a count it
/// read none of.
fn threads(process: &collector::processes::Process) -> Option<u64> {
    u64::try_from(process.task.threads).ok()
}

/// Why the collector did not read a program, by the code the window keeps for it.
fn program_unavailable(code: u8) -> ProgramUnavailable {
    match code {
        identity_availability::REFUSED => ProgramUnavailable::Refused,
        identity_availability::GONE => ProgramUnavailable::Gone,
        identity_availability::UNSUPPORTED => ProgramUnavailable::Unsupported,
        identity_availability::CHANGED => ProgramUnavailable::Changed,
        identity_availability::INVALID => ProgramUnavailable::Invalid,
        identity_availability::EXHAUSTED => ProgramUnavailable::Exhausted,
        _ => unreachable!("the window names a program wherever the collector read one"),
    }
}

/// The error of a process the columns refused: a tick of more processes than any tick holds,
/// or two processes of one pid.
fn process_refused(refusal: Refusal) -> Error {
    match refusal {
        Refusal::Full => Error::Bound("window tick process bound"),
        Refusal::Order => Error::Bound("window tick pid order"),
    }
}

/// The error of an event the columns refused: a tick of more events than any tick holds.
fn event_refused(_: Full) -> Error {
    Error::Bound("window tick event bound")
}

#[cfg(test)]
mod tests;
