//! One tick of the collector's ring as the columns the model's App rule reads, in buffers taken
//! at startup.
use collector::processes::{Unreadable, Usage};
use collector::ring::{
    IdentityObservation, IdentityUnavailable, PROGRAM_PATH_BYTES_MAX, ProcessCreation,
    ProcessIdentity, ProgramBuffers, ProgramReference, RingReader, UsageAvailability,
};
use collector::tracker::{EVENTS_PER_TICK_MAX, Event};
use model::apps::{
    Creation, Execs, Exits, Measurement, Processes, ProgramUnavailable, Stamp, Starts, Tick, Usages,
};
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
    stamp: Stamp,
    processes: ProcessColumns,
    exits: ExitColumns,
    starts: EventColumns,
    execs: EventColumns,
    /// The tick's events before they are put in the rule's orders.
    exit_rows: Vec<ExitRow>,
    start_rows: Vec<EventRow>,
    exec_rows: Vec<EventRow>,
    executable_name: Box<[u8; PROGRAM_PATH_BYTES_MAX]>,
    app_path: Box<[u8; PROGRAM_PATH_BYTES_MAX]>,
}

/// The columns of a tick's processes.
struct ProcessColumns {
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    program: Vec<std::result::Result<u32, ProgramUnavailable>>,
    usage: UsageColumns,
    gpu_ns: Vec<Option<u64>>,
    threads: Vec<Option<u64>>,
}

/// The columns of a tick's exits.
struct ExitColumns {
    number: Vec<u64>,
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    usage: UsageColumns,
}

/// The columns of a tick's starts or of its execs.
struct EventColumns {
    number: Vec<u64>,
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    program: Vec<std::result::Result<u32, ProgramUnavailable>>,
}

/// The columns of the usage of a tick's processes or of its exits.
struct UsageColumns {
    measurement: Vec<Measurement>,
    awake_ns_at_start: Vec<u64>,
    user_ns: Vec<u64>,
    system_ns: Vec<u64>,
    energy_nj: Vec<u64>,
    disk_read_bytes: Vec<u64>,
    disk_written_bytes: Vec<u64>,
    idle_wakeups: Vec<u64>,
    pageins: Vec<u64>,
    resident_bytes: Vec<u64>,
    footprint_bytes: Vec<u64>,
}

#[derive(Clone, Copy)]
struct ExitRow {
    number: u64,
    pid: i32,
    creation: Option<Creation>,
    usage: std::result::Result<Usage, Measurement>,
}

#[derive(Clone, Copy)]
struct EventRow {
    number: u64,
    pid: i32,
    creation: Option<Creation>,
    program: std::result::Result<u32, ProgramUnavailable>,
}

impl RingTick {
    /// Buffers for the ticks of a ring of at most `processes` processes a tick.
    #[must_use]
    pub fn new(processes: usize) -> Self {
        Self {
            stamp: Stamp {
                recording: [0; 2],
                boot: [0; 2],
                run: [0; 2],
                publication: 0,
                continuous_ns: None,
            },
            processes: ProcessColumns {
                pid: Vec::with_capacity(processes),
                creation: Vec::with_capacity(processes),
                program: Vec::with_capacity(processes),
                usage: UsageColumns::new(processes),
                gpu_ns: Vec::with_capacity(processes),
                threads: Vec::with_capacity(processes),
            },
            exits: ExitColumns {
                number: Vec::with_capacity(EVENTS_PER_TICK_MAX),
                pid: Vec::with_capacity(EVENTS_PER_TICK_MAX),
                creation: Vec::with_capacity(EVENTS_PER_TICK_MAX),
                usage: UsageColumns::new(EVENTS_PER_TICK_MAX),
            },
            starts: EventColumns::new(EVENTS_PER_TICK_MAX),
            execs: EventColumns::new(EVENTS_PER_TICK_MAX),
            exit_rows: Vec::with_capacity(EVENTS_PER_TICK_MAX),
            start_rows: Vec::with_capacity(EVENTS_PER_TICK_MAX),
            exec_rows: Vec::with_capacity(EVENTS_PER_TICK_MAX),
            executable_name: Box::new([0; PROGRAM_PATH_BYTES_MAX]),
            app_path: Box::new([0; PROGRAM_PATH_BYTES_MAX]),
        }
    }

    /// Reads the tick of `source` into the columns, and indexes in `programs` each dictionary
    /// entry the tick names that `programs` has not met. A refusal leaves the columns empty.
    pub fn capture(&mut self, source: &Source<'_>, programs: &mut Programs) -> Result<()> {
        self.clear();
        let captured = self.capture_tick(source, programs);
        if captured.is_err() {
            self.clear();
        }
        captured
    }

    /// The tick last captured, as the App rule reads it.
    #[must_use]
    pub fn tick(&self) -> Tick<'_> {
        Tick {
            stamp: self.stamp,
            processes: Processes {
                pid: &self.processes.pid,
                creation: &self.processes.creation,
                program: &self.processes.program,
                usage: self.processes.usage.usages(),
                gpu_ns: &self.processes.gpu_ns,
                threads: &self.processes.threads,
            },
            exits: Exits {
                number: &self.exits.number,
                pid: &self.exits.pid,
                creation: &self.exits.creation,
                usage: self.exits.usage.usages(),
            },
            starts: Starts {
                number: &self.starts.number,
                pid: &self.starts.pid,
                creation: &self.starts.creation,
                program: &self.starts.program,
            },
            execs: Execs {
                number: &self.execs.number,
                pid: &self.execs.pid,
                creation: &self.execs.creation,
                program: &self.execs.program,
            },
        }
    }

    fn clear(&mut self) {
        self.stamp.publication = 0;
        self.stamp.continuous_ns = None;
        self.processes.clear();
        self.exits.clear();
        self.starts.clear();
        self.execs.clear();
        self.exit_rows.clear();
        self.start_rows.clear();
        self.exec_rows.clear();
    }

    fn capture_tick(&mut self, source: &Source<'_>, programs: &mut Programs) -> Result<()> {
        validate(source, self.processes.pid.capacity())?;
        let identity = source
            .reader
            .identity()
            .ok_or(Error::Bound("ring tick run identity"))?;
        self.stamp = Stamp {
            recording: identity.recording.0,
            boot: identity.boot.0,
            run: identity.run.0,
            publication: source.publication,
            continuous_ns: source.tick.times.map(|times| times.continuous_ns),
        };
        if source.reader.dictionary_capacity().is_none() {
            self.capture_unidentified(source);
            return Ok(());
        }
        self.capture_processes(source, programs)?;
        self.capture_events(source, programs)?;
        self.order_events();
        Ok(())
    }

    /// The processes of a ring that records no identities, each with its usage and no program.
    fn capture_unidentified(&mut self, source: &Source<'_>) {
        for process in &source.tick.processes {
            self.processes
                .push(process.pid, None, Err(ProgramUnavailable::Unsupported));
            self.processes.usage.push(Ok(&process.usage));
            self.processes.gpu_ns.push(None);
            self.processes.threads.push(threads(process.task.threads));
        }
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
            if let Some(&previous) = self.processes.pid.last() {
                if previous >= identity.pid {
                    return Err(Error::Bound("ring tick identity order"));
                }
            }
            let program = self.program(source.reader, programs, &identity.identity)?;
            self.processes
                .push(identity.pid, creation(identity), program);
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
                    (Ok(&process.usage), threads(process.task.threads))
                }
                UsageAvailability::Refused => (Err(Measurement::Refused), None),
                UsageAvailability::Gone => (Err(Measurement::Gone), None),
                UsageAvailability::NotApplicable => {
                    return Err(Error::Bound("ring tick process usage availability"));
                }
            };
            self.processes.usage.push(usage);
            self.processes
                .gpu_ns
                .push(gpu_ns.get(index).copied().flatten());
            self.processes.threads.push(task_threads);
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
                    self.exit_rows.push(ExitRow {
                        number: numbered.number,
                        pid: *pid,
                        creation: creation(identity),
                        usage: match last {
                            Ok(usage) => Ok(*usage),
                            Err(Unreadable::Refused) => Err(Measurement::Refused),
                            Err(Unreadable::Gone) => Err(Measurement::Gone),
                        },
                    });
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
                self.start_rows.push(row);
            } else {
                self.exec_rows.push(row);
            }
        }
        Ok(())
    }

    /// Puts the exits and the program events in the orders the App rule reads them in.
    fn order_events(&mut self) {
        self.exit_rows
            .sort_unstable_by_key(|row| (row.pid, row.creation.map(unique_id), row.number));
        for row in &self.exit_rows {
            self.exits.number.push(row.number);
            self.exits.pid.push(row.pid);
            self.exits.creation.push(row.creation);
            self.exits
                .usage
                .push(row.usage.as_ref().map_err(|reason| *reason));
        }

        self.start_rows
            .sort_unstable_by_key(|row| (row.pid, row.creation.map(unique_id), row.number));
        self.starts.extend(&self.start_rows);

        self.exec_rows
            .sort_unstable_by_key(|row| (row.pid, row.number));
        self.execs.extend(&self.exec_rows);
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

impl ProcessColumns {
    fn push(
        &mut self,
        pid: i32,
        creation: Option<Creation>,
        program: std::result::Result<u32, ProgramUnavailable>,
    ) {
        self.pid.push(pid);
        self.creation.push(creation);
        self.program.push(program);
    }

    fn clear(&mut self) {
        self.pid.clear();
        self.creation.clear();
        self.program.clear();
        self.usage.clear();
        self.gpu_ns.clear();
        self.threads.clear();
    }
}

impl ExitColumns {
    fn clear(&mut self) {
        self.number.clear();
        self.pid.clear();
        self.creation.clear();
        self.usage.clear();
    }
}

impl EventColumns {
    fn new(capacity: usize) -> Self {
        Self {
            number: Vec::with_capacity(capacity),
            pid: Vec::with_capacity(capacity),
            creation: Vec::with_capacity(capacity),
            program: Vec::with_capacity(capacity),
        }
    }

    fn extend(&mut self, rows: &[EventRow]) {
        for row in rows {
            self.number.push(row.number);
            self.pid.push(row.pid);
            self.creation.push(row.creation);
            self.program.push(row.program);
        }
    }

    fn clear(&mut self) {
        self.number.clear();
        self.pid.clear();
        self.creation.clear();
        self.program.clear();
    }
}

impl UsageColumns {
    fn new(capacity: usize) -> Self {
        Self {
            measurement: Vec::with_capacity(capacity),
            awake_ns_at_start: Vec::with_capacity(capacity),
            user_ns: Vec::with_capacity(capacity),
            system_ns: Vec::with_capacity(capacity),
            energy_nj: Vec::with_capacity(capacity),
            disk_read_bytes: Vec::with_capacity(capacity),
            disk_written_bytes: Vec::with_capacity(capacity),
            idle_wakeups: Vec::with_capacity(capacity),
            pageins: Vec::with_capacity(capacity),
            resident_bytes: Vec::with_capacity(capacity),
            footprint_bytes: Vec::with_capacity(capacity),
        }
    }

    /// Adds a row of `usage`, or of zeros with the reason it was not read.
    fn push(&mut self, usage: std::result::Result<&Usage, Measurement>) {
        let (measurement, usage) = match usage {
            Ok(usage) => (Measurement::Measured, *usage),
            Err(reason) => (reason, UNREAD),
        };
        self.measurement.push(measurement);
        self.awake_ns_at_start.push(usage.awake_ns_at_start);
        self.user_ns.push(usage.user_ns);
        self.system_ns.push(usage.system_ns);
        self.energy_nj.push(usage.energy_nj);
        self.disk_read_bytes.push(usage.disk_read_bytes);
        self.disk_written_bytes.push(usage.disk_written_bytes);
        self.idle_wakeups.push(usage.idle_wakeups);
        self.pageins.push(usage.pageins);
        self.resident_bytes.push(usage.resident_bytes);
        self.footprint_bytes.push(usage.footprint_bytes);
    }

    fn clear(&mut self) {
        self.measurement.clear();
        self.awake_ns_at_start.clear();
        self.user_ns.clear();
        self.system_ns.clear();
        self.energy_nj.clear();
        self.disk_read_bytes.clear();
        self.disk_written_bytes.clear();
        self.idle_wakeups.clear();
        self.pageins.clear();
        self.resident_bytes.clear();
        self.footprint_bytes.clear();
    }

    fn usages(&self) -> Usages<'_> {
        Usages {
            measurement: &self.measurement,
            awake_ns_at_start: &self.awake_ns_at_start,
            user_ns: &self.user_ns,
            system_ns: &self.system_ns,
            energy_nj: &self.energy_nj,
            disk_read_bytes: &self.disk_read_bytes,
            disk_written_bytes: &self.disk_written_bytes,
            idle_wakeups: &self.idle_wakeups,
            pageins: &self.pageins,
            resident_bytes: &self.resident_bytes,
            footprint_bytes: &self.footprint_bytes,
        }
    }
}

/// The values a row of usage the collector did not read holds, which the rule never reads.
const UNREAD: Usage = Usage {
    awake_ns_at_start: 0,
    user_ns: 0,
    system_ns: 0,
    energy_nj: 0,
    instructions: 0,
    super_instructions: 0,
    cycles: 0,
    idle_wakeups: 0,
    interrupt_wakeups: 0,
    pageins: 0,
    resident_bytes: 0,
    footprint_bytes: 0,
    disk_read_bytes: 0,
    disk_written_bytes: 0,
};

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

fn unique_id(creation: Creation) -> u64 {
    creation.unique_id
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
