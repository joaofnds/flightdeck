//! Real registered sources shared by focused live identity adapter tests.
use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::recording::{RegisteredRun, Registration, Registry, private};
use collector::ring::{
    DictionaryCapacity, Dropped, Found, FoundEvent, IdentityObservation, IdentityPublication,
    ObservationTimes, Place, ProcessCreation, ProcessIdentity, ProgramBytes, ProgramReference,
    Readings, RingReader, Shape, UsageAvailability, Writer,
};
use collector::tracker::Event;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SHAPE: Shape = Shape {
    slot_count: 8,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 2,
    coalition_capacity: 1,
    event_capacity: 32,
};

pub struct Fixture {
    pub root: PathBuf,
    pub writer: Writer,
    elapsed: u64,
    identity: bool,
    process_capacity: u64,
    owns_root: bool,
}

impl Fixture {
    pub fn new() -> Self {
        Self::new_mode(true, 2, None)
    }

    pub fn new_mode(identity: bool, process_capacity: u64, recording: Option<&Self>) -> Self {
        if let Some(recording) = recording {
            assert!(!identity);
            assert_eq!(process_capacity, 2);
            return recording.new_mode_legacy();
        }

        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-live-identity-{}-{serial}",
            std::process::id()
        ));
        private::create_private_dir(&root).unwrap();
        let place = Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdv{}x{serial}", std::process::id()),
        };
        let shape = Shape {
            slot_count: if process_capacity == 2 { 8 } else { 64 },
            process_capacity,
            ..SHAPE
        };
        let (mut writer, registered) = Registration::start(&root, &place, shape, 0)
            .unwrap()
            .into_parts();
        registered.unwrap();
        if identity {
            Self::new_mode_identity(&root, &shape, serial, &mut writer);
        }
        Self {
            root,
            writer,
            elapsed: 0,
            identity,
            process_capacity,
            owns_root: true,
        }
    }

    fn new_mode_identity(root: &std::path::Path, shape: &Shape, serial: u64, writer: &mut Writer) {
        let place = Place {
            lock_path: root.join("identity.lock"),
            name_prefix: format!("/fdvi{}x{serial}", std::process::id()),
        };
        let next = Writer::create_with_identity(
            &place,
            *shape,
            DictionaryCapacity {
                entries: 4,
                arena_bytes: 32_768,
            },
            &writer.identity().unwrap(),
            0,
        )
        .unwrap();
        collector::ring::unlink_generation(writer.name()).unwrap();
        *writer = next;
        Registry {
            runs: vec![RegisteredRun {
                identity: writer.identity().unwrap(),
                ring_name: writer.name().to_owned(),
                collector_pid: 0,
            }],
            evictions: 0,
        }
        .write(root)
        .unwrap();
    }

    fn new_mode_legacy(&self) -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let place = Place {
            lock_path: self.root.join("legacy.lock"),
            name_prefix: format!("/fdvl{}x{serial}", std::process::id()),
        };
        let mut identity = self.writer.identity().unwrap();
        identity.run.0[0] ^= 1;
        let writer = Writer::create_recording(&place, SHAPE, &identity, 0).unwrap();
        Registry {
            runs: vec![
                RegisteredRun {
                    identity: self.writer.identity().unwrap(),
                    ring_name: self.writer.name().to_owned(),
                    collector_pid: 0,
                },
                RegisteredRun {
                    identity,
                    ring_name: writer.name().to_owned(),
                    collector_pid: 0,
                },
            ],
            evictions: 0,
        }
        .write(&self.root)
        .unwrap();

        Self {
            root: self.root.clone(),
            writer,
            elapsed: 0,
            identity: false,
            process_capacity: 2,
            owns_root: false,
        }
    }

    fn intern(&mut self, name: &[u8], app: &[u8]) -> ProgramReference {
        let mut program = None;
        self.writer
            .intern_program(
                &ProgramBytes {
                    executable_name: name,
                    app_path: Some(app),
                },
                &mut program,
            )
            .unwrap();
        program.unwrap()
    }

    pub fn publish(&mut self, second: u64) {
        let process = measured_process(self.elapsed);
        let events = [Event::Exec { pid: 7 }];
        let memory = Memory::default();
        let mut readings = Readings {
            run_tick: self.elapsed,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: std::slice::from_ref(&process),
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &events,
            dropped: Dropped::default(),
        };
        let times = ObservationTimes {
            wall_ns: second * 1_000_000_000,
            continuous_ns: self.elapsed * 1_000_000_000,
            awake_ns: self.elapsed * 1_000_000_000,
        };
        if self.identity {
            self.publish_identity(&mut readings, &times);
        } else {
            self.writer.publish_observed(&readings, &times);
        }
        self.elapsed += 1;
    }

    fn publish_identity(&mut self, readings: &mut Readings<'_>, times: &ObservationTimes) {
        let refused = self.intern(b"refused\xff", b"/Apps/Outer\xfe.app");
        let current = self.intern(b"current", b"/Apps/Current.app");
        let departed = self.intern(b"departed\xfd", b"/Old.app");
        let processes = if self.process_capacity == 2 {
            vec![
                observation(1, UsageAvailability::Refused, &refused),
                observation(7, UsageAvailability::Measured, &current),
            ]
        } else {
            (1..=i32::try_from(self.process_capacity).unwrap())
                .map(|pid| {
                    if pid == 7 {
                        observation(pid, UsageAvailability::Measured, &current)
                    } else {
                        observation(pid, UsageAvailability::Refused, &refused)
                    }
                })
                .collect()
        };
        let events = [IdentityObservation {
            pid: 7,
            usage: UsageAvailability::NotApplicable,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: 700,
                    pid_version: -70,
                },
                program: departed,
            },
        }];
        readings.processes_refused = u32::try_from(self.process_capacity - 1).unwrap();
        self.writer
            .publish_with_identity(
                readings,
                Some(times),
                &IdentityPublication {
                    processes: &processes,
                    events: &events,
                },
            )
            .unwrap();
    }

    pub fn seal(&self) {
        let mut reader = RingReader::open_generation(self.writer.name()).unwrap();
        let window = self.seal_read(&mut reader);
        drop(reader);
        let store = persistence::Store::open(&self.root).unwrap();
        let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
            panic!("fixture already committed");
        };
        let tables = persistence::Encoder
            .encode(&window, pending.path())
            .unwrap();
        let publications = window.publication_range();
        let events = window.event_range();
        let first = window
            .observations()
            .map(|value| value.wall_ns.unwrap())
            .min()
            .unwrap();
        let last = window
            .observations()
            .map(|value| value.wall_ns.unwrap())
            .max()
            .unwrap();
        store
            .commit(
                pending,
                persistence::Manifest {
                    identity: window.identity,
                    publication_first: publications.start,
                    publication_end: publications.end,
                    events_first: events.start,
                    events_end: events.end,
                    wall_ns: Some(persistence::TimeBounds {
                        min: first,
                        max: last,
                    }),
                    continuous_ns: None,
                    awake_ns: None,
                    losses: persistence::Losses::default(),
                    data_directory: String::new(),
                    tables,
                },
            )
            .unwrap();
    }

    fn seal_read(&self, reader: &mut RingReader) -> persistence::Window {
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        let mut identities = Vec::with_capacity(2);
        let mut window = persistence::Window::new(
            persistence::WindowIdentity {
                run: self.writer.identity().unwrap(),
                window: 0,
            },
            persistence::Limits::from_shape(&SHAPE).unwrap(),
        );
        for _index in 0..8 {
            let found = if self.identity {
                reader.read_identity_into(&mut tick, &mut identities)
            } else {
                reader.read_into(&mut tick)
            }
            .unwrap();
            let publication = match found {
                Found::Nothing => break,
                Found::Missed { .. } => panic!("fixture source overwritten"),
                Found::Publication { publication, .. } => publication,
            };
            let mut identity = IdentityObservation::EMPTY;
            let FoundEvent::Event {
                number,
                tick: run_tick,
                event,
            } = reader.read_event_identity(&mut identity).unwrap()
            else {
                panic!("event missing");
            };
            let times = tick.times.unwrap();
            let observation = persistence::Observation {
                publication,
                wall_ns: Some(times.wall_ns),
                continuous_ns: Some(times.continuous_ns),
                awake_ns: Some(times.awake_ns),
            };
            let events = [persistence::NumberedEvent {
                number,
                tick: run_tick,
                event: &event,
            }];
            if self.identity {
                window
                    .push_tick_with_identity(
                        &observation,
                        &tick,
                        &events,
                        &persistence::IdentitySource {
                            reader,
                            processes: &identities,
                            events: &[identity],
                        },
                    )
                    .unwrap();
            } else {
                window.push_tick(&observation, &tick, &events).unwrap();
            }
        }
        window
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        if self.owns_root {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
}

fn observation(
    pid: i32,
    usage: UsageAvailability,
    program: &ProgramReference,
) -> IdentityObservation {
    IdentityObservation {
        pid,
        usage,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: u64::try_from(pid).unwrap(),
                pid_version: -pid,
            },
            program: *program,
        },
    }
}

fn measured_process(tick: u64) -> Process {
    Process {
        pid: 7,
        usage: Usage {
            awake_ns_at_start: 0,
            user_ns: 9_007_199_254_740_993 + tick,
            system_ns: 3,
            energy_nj: 4,
            instructions: 5,
            super_instructions: 6,
            cycles: 7,
            idle_wakeups: 8,
            interrupt_wakeups: 9,
            pageins: 10,
            resident_bytes: 11,
            footprint_bytes: 12,
            disk_read_bytes: 13,
            disk_written_bytes: 14,
        },
        task: Task {
            threads: 1,
            faults: 2,
            context_switches: 3,
            mach_syscalls: 4,
            unix_syscalls: 5,
        },
    }
}
