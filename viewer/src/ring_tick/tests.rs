use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::machine::Memory;
use collector::processes::{Process, Task, Unreadable, Usage};
use collector::recording::private;
use collector::ring::{
    DictionaryCapacity, Dropped, Found, FoundEvent, Identity, IdentityObservation,
    IdentityPublication, IdentityUnavailable, ObservationTimes, Place, ProcessCreation,
    ProcessIdentity, ProgramBytes, ProgramReference, Readings, RingReader, RunIdentity, Shape,
    SupplementalReadings, SystemPublication, UsageAvailability, Writer,
};
use collector::system::{MachineFacts, SystemReading};
use collector::tracker::{Event, Noticed};
use model::apps::{Apps, Creation, Measurement, ProgramUnavailable};
use model::programs::{AppKey, Capacity, Programs};
use persistence::NumberedEvent;

use super::{RingTick, Source};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SHAPE: Shape = Shape {
    slot_count: 2,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 4,
    coalition_capacity: 1,
    event_capacity: 8,
};
const SAFARI: ProgramBytes = ProgramBytes {
    executable_name: b"Safari",
    app_path: Some(b"/Applications/Safari.app"),
};
const GIT: ProgramBytes = ProgramBytes {
    executable_name: b"git",
    app_path: None,
};

#[test]
fn hands_out_each_process_with_its_usage_and_its_dictionary_entry() {
    // Goal: the rule reads each process of a ring tick with its creation, its usage and its
    // program as the dictionary entry the index met it by.
    // Method: publish Safari, then git twice, one of them refused its usage, and capture the
    // tick into a fresh index.
    let mut ring = Ring::new(true);
    let safari = ring.intern(&SAFARI);
    let git = ring.intern(&GIT);
    ring.gpu_ns = vec![Some(600), None, None];
    ring.publish(
        1,
        &[process(7, 4_000_000), process(8, 1_000_000)],
        &[
            measured(7, UsageAvailability::Measured, safari),
            measured(8, UsageAvailability::Measured, git),
            measured(9, UsageAvailability::Refused, git),
        ],
        &[],
        &[],
    );
    let mut programs = programs();
    let mut tick = RingTick::new(4);

    ring.capture(&mut tick, &mut programs).unwrap();

    let read = tick.tick();
    assert_eq!(read.stamp.tick, 1);
    assert_eq!(read.stamp.run, [3, 4]);
    assert_eq!(read.stamp.continuous_ns, Some(2_000_000_000));
    assert_eq!(read.processes.pid, [7, 8, 9]);
    assert_eq!(read.processes.creation[2], Some(creation(9)));
    assert_eq!(read.processes.program, [Ok(0), Ok(1), Ok(1)]);
    assert_eq!(
        read.processes.usage.measurement,
        [
            Measurement::Measured,
            Measurement::Measured,
            Measurement::Refused
        ]
    );
    assert_eq!(read.processes.usage.user_ns, [4_000_000, 1_000_000, 0]);
    assert_eq!(read.processes.threads, [Some(3), Some(3), None]);
    assert_eq!(read.processes.gpu_ns, [Some(600), None, None]);
    assert_eq!(
        programs.key(programs.app(0)),
        AppKey::Bundle(b"/Applications/Safari.app")
    );
    assert_eq!(programs.key(programs.app(1)), AppKey::Executable(b"git"));
}

#[test]
fn hands_out_the_events_in_the_orders_the_rule_reads_them() {
    // Goal: the rule reads exits by pid and unique id, starts by pid, unique id and number,
    // and execs by pid and number, whatever order the collector noticed them in.
    // Method: publish an exit of pid 9, a start of pid 8, an exec of pid 8, an exit of pid 7,
    // a start of pid 7 and an exec of pid 7, and capture the tick.
    let mut ring = Ring::new(true);
    let git = ring.intern(&GIT);
    ring.publish(
        1,
        &[],
        &[],
        &[
            exit(9),
            Event::Start {
                pid: 8,
                coalition: None,
            },
            Event::Exec { pid: 8 },
            exit(7),
            Event::Start {
                pid: 7,
                coalition: None,
            },
            Event::Exec { pid: 7 },
        ],
        &[
            ended(9),
            event_identity(8, git),
            event_identity(8, git),
            ended(7),
            event_identity(7, git),
            event_identity(7, git),
        ],
    );
    let mut programs = programs();
    let mut tick = RingTick::new(4);

    ring.capture(&mut tick, &mut programs).unwrap();

    let read = tick.tick();
    assert_eq!(read.exits.pid, [7, 9]);
    assert_eq!(read.exits.number, [4, 1]);
    assert_eq!(
        read.exits.usage.measurement,
        [Measurement::Gone, Measurement::Gone]
    );
    assert_eq!(read.starts.pid, [7, 8]);
    assert_eq!(read.starts.number, [5, 2]);
    assert_eq!(read.starts.program, [Ok(0), Ok(0)]);
    assert_eq!(read.execs.pid, [7, 8]);
    assert_eq!(read.execs.number, [6, 3]);
}

#[test]
fn gives_the_rule_two_ticks_it_derives_apps_from() {
    // Goal: two captured ticks of one run give the App rule the CPU a process spent between
    // them.
    // Method: publish Safari at 1 ms of CPU and then at 6 ms, and derive the second tick's
    // Apps after the first.
    let mut ring = Ring::new(true);
    let safari = ring.intern(&SAFARI);
    let identities = [measured(7, UsageAvailability::Measured, safari)];
    let mut programs = programs();
    let mut previous = RingTick::new(4);
    let mut selected = RingTick::new(4);
    ring.publish(1, &[process(7, 1_000_000)], &identities, &[], &[]);
    ring.capture(&mut previous, &mut programs).unwrap();
    ring.publish(2, &[process(7, 6_000_000)], &identities, &[], &[]);
    ring.capture(&mut selected, &mut programs).unwrap();
    let mut apps = Apps::new(4, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    assert_eq!(apps.elapsed_ns(), Some(1_000_000_000));
    assert_eq!(apps.totals().len(), 1);
    assert_eq!(apps.totals()[0].cpu_ns.known, 5_000_000);
}

#[test]
fn gives_the_rule_no_rates_across_a_missed_tick() {
    // Goal: two publications in a row whose ticks are not in a row span a missed tick, which
    // has no readings, so the rule takes no rates across them.
    // Method: publish Safari at tick 1 and then at tick 3, which the collector publishes next
    // when tick 2 runs late, and derive the second publication's Apps after the first.
    let mut ring = Ring::new(true);
    let safari = ring.intern(&SAFARI);
    let identities = [measured(7, UsageAvailability::Measured, safari)];
    let mut programs = programs();
    let mut previous = RingTick::new(4);
    let mut selected = RingTick::new(4);
    ring.publish(1, &[process(7, 1_000_000)], &identities, &[], &[]);
    ring.capture(&mut previous, &mut programs).unwrap();
    ring.publish(3, &[process(7, 6_000_000)], &identities, &[], &[]);
    ring.capture(&mut selected, &mut programs).unwrap();
    let mut apps = Apps::new(4, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    assert_eq!(apps.elapsed_ns(), None);
}

#[test]
fn hands_out_the_processes_of_a_ring_without_identities_with_no_program() {
    // Goal: a ring that records no identities still gives the rule each process's usage, and
    // says its program was not read.
    // Method: publish one process into a ring made without a dictionary and capture it.
    let mut ring = Ring::new(false);
    ring.publish(1, &[process(7, 4_000_000)], &[], &[], &[]);
    let mut programs = programs();
    let mut tick = RingTick::new(4);

    ring.capture(&mut tick, &mut programs).unwrap();

    let read = tick.tick();
    assert_eq!(read.processes.pid, [7]);
    assert_eq!(read.processes.creation, [None]);
    assert_eq!(
        read.processes.program,
        [Err(ProgramUnavailable::Unsupported)]
    );
    assert_eq!(read.processes.usage.user_ns, [4_000_000]);
}

#[test]
fn names_why_the_collector_did_not_read_a_program() {
    // Goal: a process whose program the collector could not read reaches the rule with the
    // reason, and the index meets no entry for it.
    // Method: publish one process whose identity macOS refused, and capture it.
    let mut ring = Ring::new(true);
    ring.publish(
        1,
        &[process(7, 0)],
        &[IdentityObservation {
            pid: 7,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Unavailable {
                creation: Some(ProcessCreation {
                    unique_id: 700,
                    pid_version: 1,
                }),
                reason: IdentityUnavailable::Refused,
            },
        }],
        &[],
        &[],
    );
    let mut programs = programs();
    let mut tick = RingTick::new(4);

    ring.capture(&mut tick, &mut programs).unwrap();

    assert_eq!(
        tick.tick().processes.program,
        [Err(ProgramUnavailable::Refused)]
    );
    assert!(!programs.knows(0));
}

#[test]
fn refuses_events_without_their_identities_and_leaves_the_columns_empty() {
    // Goal: a tick whose events and identities do not pair up is refused, and the rule never
    // reads a half-captured tick.
    // Method: capture a tick of one process, then the next tick with an exit of pid 9 paired
    // with the identity of pid 8.
    let mut ring = Ring::new(true);
    let git = ring.intern(&GIT);
    let identities = [measured(7, UsageAvailability::Measured, git)];
    let mut programs = programs();
    let mut tick = RingTick::new(4);
    ring.publish(1, &[process(7, 0)], &identities, &[], &[]);
    ring.capture(&mut tick, &mut programs).unwrap();
    ring.publish(2, &[process(7, 0)], &identities, &[exit(9)], &[ended(9)]);
    ring.read();
    let (number, run_tick, event) = &ring.events[0];

    let refused = tick.capture(
        &Source {
            reader: &ring.reader,
            tick: &ring.tick,
            identities: &ring.identities,
            publication: 2,
            events: &[NumberedEvent {
                number: *number,
                tick: *run_tick,
                event,
            }],
            event_identities: &[ended(8)],
        },
        &mut programs,
    );

    assert!(refused.is_err());
    assert!(tick.tick().processes.pid.is_empty());
    assert_eq!(tick.tick().stamp.tick, 0);
}

/// A ring of one collector run that a test publishes into and reads back.
struct Ring {
    root: PathBuf,
    name: String,
    writer: Writer,
    reader: RingReader,
    tick: collector::ring::Tick,
    identities: Vec<IdentityObservation>,
    events: Vec<(u64, u64, Event)>,
    event_identities: Vec<IdentityObservation>,
    publication: u64,
    /// The GPU time of each process the next publication names, none for each where empty.
    gpu_ns: Vec<Option<u64>>,
}

impl Ring {
    fn new(identity: bool) -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-ring-tick-{}-{serial}",
            std::process::id(),
        ));
        private::create_private_dir(&root).unwrap();
        let place = Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdrt{}x{serial}", std::process::id()),
        };
        let run = RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        };
        let writer = if identity {
            Writer::create_with_system(
                &place,
                SHAPE,
                DictionaryCapacity {
                    entries: 4,
                    arena_bytes: 4096,
                },
                &run,
                0,
            )
        } else {
            Writer::create_recording(&place, SHAPE, &run, 0)
        }
        .unwrap();
        let name = writer.name().to_owned();
        let reader = RingReader::open_generation(&name).unwrap();
        Self {
            root,
            name,
            writer,
            reader,
            tick: persistence::tick_buffer(&SHAPE).unwrap(),
            identities: Vec::with_capacity(4),
            events: Vec::with_capacity(8),
            event_identities: Vec::with_capacity(8),
            publication: 0,
            gpu_ns: Vec::new(),
        }
    }

    fn intern(&mut self, program: &ProgramBytes) -> ProgramReference {
        let mut reference = None;
        self.writer.intern_program(program, &mut reference).unwrap();
        reference.unwrap()
    }

    /// Publishes the tick of `second` with `processes`, their `identities`, and `events` with
    /// theirs.
    fn publish(
        &mut self,
        second: u64,
        processes: &[Process],
        identities: &[IdentityObservation],
        events: &[Event],
        event_identities: &[IdentityObservation],
    ) {
        let memory = Memory::default();
        let readings = Readings {
            run_tick: second,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes,
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events,
            dropped: Dropped::default(),
        };
        let times = ObservationTimes {
            wall_ns: 1_800_000_000_000_000_000 + second * 1_000_000_000,
            continuous_ns: (second + 1) * 1_000_000_000,
            awake_ns: (second + 1) * 1_000_000_000,
        };
        if self.reader.dictionary_capacity().is_none() {
            self.writer.publish_observed(&readings, &times);
            return;
        }
        if self.gpu_ns.is_empty() {
            self.gpu_ns.resize(identities.len(), None);
        }
        self.writer
            .publish_with_disks(
                &readings,
                Some(&times),
                &IdentityPublication {
                    processes: identities,
                    events: event_identities,
                },
                &SupplementalReadings {
                    battery: None,
                    disks: None,
                    system: Some(&SystemPublication {
                        reading: &SystemReading::default(),
                        machine: &MachineFacts::default(),
                        clock: None,
                        process_gpu_ns: &self.gpu_ns,
                    }),
                },
            )
            .unwrap();
        self.gpu_ns.clear();
    }

    /// Reads the ring's next tick and its events, as the store does.
    fn read(&mut self) {
        let found = if self.reader.dictionary_capacity().is_some() {
            self.reader
                .read_identity_into(&mut self.tick, &mut self.identities)
        } else {
            self.identities.clear();
            self.reader.read_into(&mut self.tick)
        }
        .unwrap();
        let Found::Publication { publication, .. } = found else {
            panic!("the ring holds no publication");
        };
        self.publication = publication;

        self.events.clear();
        self.event_identities.clear();
        if self.tick.events.is_empty() {
            return;
        }
        self.reader.seek_event(self.tick.events.start).unwrap();
        while self.reader.next_event() < self.tick.events.end {
            let mut identity = IdentityObservation::EMPTY;
            let FoundEvent::Event {
                number,
                tick,
                event,
            } = self.reader.read_event_identity(&mut identity).unwrap()
            else {
                panic!("the ring lost an event");
            };
            self.events.push((number, tick, event));
            self.event_identities.push(identity);
        }
    }

    /// Reads the ring's next tick into `tick`.
    fn capture(&mut self, tick: &mut RingTick, programs: &mut Programs) -> persistence::Result<()> {
        self.read();
        let events: Vec<NumberedEvent> = self
            .events
            .iter()
            .map(|(number, tick, event)| NumberedEvent {
                number: *number,
                tick: *tick,
                event,
            })
            .collect();
        tick.capture(
            &Source {
                reader: &self.reader,
                tick: &self.tick,
                identities: &self.identities,
                publication: self.publication,
                events: &events,
                event_identities: &self.event_identities,
            },
            programs,
        )
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        self.writer.finish();
        collector::ring::unlink_generation(&self.name).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn programs() -> Programs {
    Programs::new(Capacity {
        entries: 4,
        key_bytes: 4096,
    })
}

fn process(pid: i32, user_ns: u64) -> Process {
    Process {
        pid,
        usage: Usage {
            awake_ns_at_start: 10,
            user_ns,
            system_ns: 0,
            energy_nj: 0,
            instructions: 0,
            super_instructions: 0,
            cycles: 0,
            idle_wakeups: 0,
            interrupt_wakeups: 0,
            pageins: 0,
            resident_bytes: 4096,
            footprint_bytes: 2048,
            disk_read_bytes: 0,
            disk_written_bytes: 0,
        },
        task: Task {
            threads: 3,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    }
}

fn creation(pid: i32) -> Creation {
    Creation {
        unique_id: u64::try_from(pid).unwrap() * 100,
        pid_version: 1,
    }
}

fn ring_creation(pid: i32) -> ProcessCreation {
    let creation = creation(pid);
    ProcessCreation {
        unique_id: creation.unique_id,
        pid_version: creation.pid_version,
    }
}

fn measured(pid: i32, usage: UsageAvailability, program: ProgramReference) -> IdentityObservation {
    IdentityObservation {
        pid,
        usage,
        identity: ProcessIdentity::Measured {
            creation: ring_creation(pid),
            program,
        },
    }
}

fn event_identity(pid: i32, program: ProgramReference) -> IdentityObservation {
    measured(pid, UsageAvailability::NotApplicable, program)
}

fn exit(pid: i32) -> Event {
    Event::Exit {
        pid,
        last: Err(Unreadable::Gone),
        noticed: Noticed::OnExit,
    }
}

fn ended(pid: i32) -> IdentityObservation {
    IdentityObservation {
        pid,
        usage: UsageAvailability::Gone,
        identity: ProcessIdentity::Unavailable {
            creation: Some(ring_creation(pid)),
            reason: IdentityUnavailable::Unsupported,
        },
    }
}
