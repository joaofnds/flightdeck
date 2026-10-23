//! One tick of the collector's ring as the columns the model's App rule reads, in buffers taken
//! at startup.
use collector::processes::{Unreadable, Usage};
use collector::ring::{
    IdentityObservation, IdentityUnavailable, PROGRAM_PATH_BYTES_MAX, ProcessCreation,
    ProcessIdentity, ProgramBuffers, ProgramReference, RingReader, UsageAvailability,
};
use collector::tracker::{EVENTS_PER_TICK_MAX, Event};
use model::apps::{Creation, Measurement, ProgramUnavailable, Stamp, Tick};
use model::columns::{EventRow, ExitRow, Full, ProcessRow, Refusal, TickColumns};
use model::programs::{Program, Programs};
use persistence::{Error, NumberedEvent, Result};

/// A tick of the ring as the store read it, with the events the collector published with it.
/// Every reference comes from one read of `reader`, since a program reference belongs to the
/// reader that decoded it.
pub struct Source<'source> {
    pub reader: &'source RingReader,
    pub tick: &'source collector::ring::Tick,
    /// The identity of each process of the tick, in ascending pid order, where the ring holds
    /// identities.
    pub identities: &'source [IdentityObservation],
    pub publication: u64,
    pub events: &'source [NumberedEvent<'source>],
    pub event_identities: &'source [IdentityObservation],
}

/// The columns of one tick of the ring.
pub struct RingTick {
    columns: TickColumns,
    executable_name: Box<[u8; PROGRAM_PATH_BYTES_MAX]>,
    app_path: Box<[u8; PROGRAM_PATH_BYTES_MAX]>,
}

impl RingTick {
    /// Buffers for the ticks of a ring of at most `processes` processes a tick.
    #[must_use]
    pub fn new(processes: u32) -> Self {
        Self {
            columns: TickColumns::new(processes, model::u32_from_length(EVENTS_PER_TICK_MAX)),
            executable_name: Box::new([0; PROGRAM_PATH_BYTES_MAX]),
            app_path: Box::new([0; PROGRAM_PATH_BYTES_MAX]),
        }
    }

    /// Reads the tick of `source` into the columns, and indexes in `programs` each dictionary
    /// entry the tick names that `programs` has not met. A refusal leaves the columns empty.
    pub fn capture(&mut self, source: &Source<'_>, programs: &mut Programs) -> Result<()> {
        self.columns.clear();
        let captured = self.capture_tick(source, programs);
        if captured.is_err() {
            self.columns.clear();
        }
        captured
    }

    /// The tick last captured, as the App rule reads it.
    #[must_use]
    pub fn tick(&self) -> Tick<'_> {
        self.columns.tick()
    }

    fn capture_tick(&mut self, source: &Source<'_>, programs: &mut Programs) -> Result<()> {
        validate(source, self.columns.processes_max())?;
        let identity = source
            .reader
            .identity()
            .ok_or(Error::Bound("ring tick run identity"))?;
        self.columns.start(Stamp {
            recording: identity.recording.0,
            boot: identity.boot.0,
            run: identity.run.0,
            tick: source.tick.run_tick,
            continuous_ns: source.tick.times.map(|times| times.continuous_ns),
        });
        if source.reader.dictionary_capacity().is_none() {
            self.capture_unidentified(source)?;
            self.columns.finish();
            return Ok(());
        }

        self.capture_processes(source, programs)?;
        self.capture_events(source, programs)?;
        self.columns.finish();
        Ok(())
    }

    /// The processes of a ring that records no identities, each with its usage and no program.
    fn capture_unidentified(&mut self, source: &Source<'_>) -> Result<()> {
        for process in &source.tick.processes {
            let row = ProcessRow {
                pid: process.pid,
                creation: None,
                program: Err(ProgramUnavailable::Unsupported),
                usage: Ok(usage(&process.usage)),
                gpu_ns: None,
                threads: threads(process.task.threads),
            };
            self.columns.push_process(&row).map_err(process_refused)?;
        }
        Ok(())
    }

    fn capture_processes(&mut self, source: &Source<'_>, programs: &mut Programs) -> Result<()> {
        let gpu_ns = source.tick.process_gpu_ns();
        if !gpu_ns.is_empty() {
            if gpu_ns.len() != source.identities.len() {
                return Err(Error::Bound("ring tick GPU time association"));
            }
        }

        let mut measured = 0;
        for (index, identity) in source.identities.iter().enumerate() {
            let program = self.program(source.reader, programs, &identity.identity)?;
            let (usage, task_threads) = match identity.usage {
                UsageAvailability::Measured => {
                    let process = source
                        .tick
                        .processes
                        .get(measured)
                        .ok_or(Error::Bound("ring tick missing numeric row"))?;
                    if process.pid != identity.pid {
                        return Err(Error::Bound("ring tick numeric association"));
                    }
                    measured += 1;
                    (Ok(usage(&process.usage)), threads(process.task.threads))
                }
                UsageAvailability::Refused => (Err(Measurement::Refused), None),
                UsageAvailability::Gone => (Err(Measurement::Gone), None),
                UsageAvailability::NotApplicable => {
                    return Err(Error::Bound("ring tick process usage availability"));
                }
            };
            let row = ProcessRow {
                pid: identity.pid,
                creation: creation(identity),
                program,
                usage,
                gpu_ns: gpu_ns.get(index).copied().flatten(),
                threads: task_threads,
            };
            self.columns.push_process(&row).map_err(process_refused)?;
        }
        if measured != source.tick.processes.len() {
            return Err(Error::Bound("ring tick unmatched numeric rows"));
        }
        Ok(())
    }

    fn capture_events(&mut self, source: &Source<'_>, programs: &mut Programs) -> Result<()> {
        let mut next = source.tick.events.start;
        for (numbered, identity) in source.events.iter().zip(source.event_identities) {
            if numbered.tick != source.tick.run_tick {
                return Err(Error::Bound("ring tick event tick association"));
            }
            if numbered.number < next {
                return Err(Error::Bound("ring tick event order"));
            }
            if numbered.number >= source.tick.events.end {
                return Err(Error::Bound("ring tick event range"));
            }
            next = numbered.number + 1;

            let (pid, start) = match numbered.event {
                Event::Exit { pid, last, .. } => {
                    if *pid != identity.pid {
                        return Err(Error::Bound("ring tick exit pid association"));
                    }
                    let row = ExitRow {
                        number: numbered.number,
                        pid: *pid,
                        creation: creation(identity),
                        usage: match last {
                            Ok(last) => Ok(usage(last)),
                            Err(Unreadable::Refused) => Err(Measurement::Refused),
                            Err(Unreadable::Gone) => Err(Measurement::Gone),
                        },
                    };
                    self.columns.push_exit(&row).map_err(event_refused)?;
                    continue;
                }
                Event::Start { pid, .. } => (*pid, true),
                Event::Exec { pid } => (*pid, false),
            };
            if pid != identity.pid {
                return Err(Error::Bound("ring tick program event pid association"));
            }
            if identity.usage != UsageAvailability::NotApplicable {
                return Err(Error::Bound("ring tick program event usage availability"));
            }
            let row = EventRow {
                number: numbered.number,
                pid,
                creation: creation(identity),
                program: self.program(source.reader, programs, &identity.identity)?,
            };
            if start {
                self.columns.push_start(&row).map_err(event_refused)?;
            } else {
                self.columns.push_exec(&row).map_err(event_refused)?;
            }
        }
        Ok(())
    }

    /// The program of `identity` as its entry in the run's dictionary, indexed in `programs`
    /// the first time a tick names it, or why the collector did not read it.
    fn program(
        &mut self,
        reader: &RingReader,
        programs: &mut Programs,
        identity: &ProcessIdentity,
    ) -> Result<std::result::Result<u32, ProgramUnavailable>> {
        match identity {
            ProcessIdentity::Measured { program, .. } => {
                Ok(Ok(self.entry(reader, programs, program)?))
            }
            ProcessIdentity::Unavailable { reason, .. } => Ok(Err(unavailable(*reason))),
        }
    }

    fn entry(
        &mut self,
        reader: &RingReader,
        programs: &mut Programs,
        program: &ProgramReference,
    ) -> Result<u32> {
        let entry = program
            .number()
            .checked_sub(1)
            .and_then(|entry| u32::try_from(entry).ok())
            .filter(|entry| model::usize_from_u32(*entry) < programs.entries_max())
            .ok_or(Error::Bound("ring tick dictionary entry"))?;
        if programs.knows(entry) {
            return Ok(entry);
        }

        let (name, path) = reader.copy_program(
            program,
            &mut ProgramBuffers {
                executable_name: &mut self.executable_name,
                app_path: &mut self.app_path,
            },
        )?;
        programs.add(
            entry,
            Program {
                executable_name: &self.executable_name[..name],
                app_path: if path == 0 {
                    None
                } else {
                    Some(&self.app_path[..path])
                },
            },
        );
        Ok(entry)
    }
}

/// The usage the App rule reads, of the counters the collector read for a process.
fn usage(read: &Usage) -> model::apps::Usage {
    model::apps::Usage {
        awake_ns_at_start: read.awake_ns_at_start,
        user_ns: read.user_ns,
        system_ns: read.system_ns,
        energy_nj: read.energy_nj,
        disk_read_bytes: read.disk_read_bytes,
        disk_written_bytes: read.disk_written_bytes,
        idle_wakeups: read.idle_wakeups,
        pageins: read.pageins,
        resident_bytes: read.resident_bytes,
        footprint_bytes: read.footprint_bytes,
    }
}

/// The error of a process the columns refused, which `validate` and the ring's pid order rule
/// out unless the ring breaks its own order.
fn process_refused(refusal: Refusal) -> Error {
    match refusal {
        Refusal::Full => Error::Bound("ring tick identity capacity"),
        Refusal::Order => Error::Bound("ring tick identity order"),
    }
}

/// The error of an event the columns refused, which `validate`'s bound on a tick's events
/// rules out.
fn event_refused(_: Full) -> Error {
    Error::Bound("ring tick event bound")
}

fn validate(source: &Source<'_>, processes_max: usize) -> Result<()> {
    if source.publication == 0 {
        return Err(Error::Bound("ring tick unpublished"));
    }
    if source.publication.checked_add(1) != Some(source.reader.next_publication()) {
        return Err(Error::Bound("ring tick publication association"));
    }
    if source.tick.processes.len() > processes_max {
        return Err(Error::Bound("ring tick numeric capacity"));
    }
    if source.identities.len() > processes_max {
        return Err(Error::Bound("ring tick identity capacity"));
    }
    if source.events.len() > EVENTS_PER_TICK_MAX {
        return Err(Error::Bound("ring tick event bound"));
    }
    if source.events.len() != source.event_identities.len() {
        return Err(Error::Bound("ring tick event identity count"));
    }
    if source.reader.dictionary_capacity().is_none() {
        if !source.identities.is_empty() {
            return Err(Error::Bound("ring tick legacy identity rows"));
        }
    }
    Ok(())
}

fn creation(identity: &IdentityObservation) -> Option<Creation> {
    identity
        .creation()
        .map(|creation: &ProcessCreation| Creation {
            unique_id: creation.unique_id,
            pid_version: creation.pid_version,
        })
}

fn unavailable(reason: IdentityUnavailable) -> ProgramUnavailable {
    match reason {
        IdentityUnavailable::Refused => ProgramUnavailable::Refused,
        IdentityUnavailable::Gone => ProgramUnavailable::Gone,
        IdentityUnavailable::Unsupported => ProgramUnavailable::Unsupported,
        IdentityUnavailable::Changed => ProgramUnavailable::Changed,
        IdentityUnavailable::Invalid => ProgramUnavailable::Invalid,
        IdentityUnavailable::Exhausted => ProgramUnavailable::Exhausted,
    }
}

/// A thread count the collector read, which it writes as a negative number where it read none.
fn threads(value: i32) -> Option<u64> {
    u64::try_from(value).ok()
}

#[cfg(test)]
mod tests;
