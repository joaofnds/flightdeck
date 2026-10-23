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
    continuous_step_ns: u64,
    awake_step_ns: u64,
    usage_override: Option<Usage>,
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
        let mut fixture = Self {
            root,
            writer,
            elapsed: 0,
            continuous_step_ns: 0,
            awake_step_ns: 0,
            usage_override: None,
            identity,
            process_capacity,
            owns_root: true,
        };
        fixture.set_clock_intervals(1_000_000_000, 1_000_000_000);
        fixture
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

        let mut fixture = Self {
            root: self.root.clone(),
            writer,
            elapsed: 0,
            continuous_step_ns: 0,
            awake_step_ns: 0,
            usage_override: None,
            identity: false,
            process_capacity: 2,
            owns_root: false,
        };
        fixture.set_clock_intervals(1_000_000_000, 1_000_000_000);
        fixture
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
        self.publish_sources(second, &[], &[]);
    }

    pub fn start_at_run_tick(&mut self, run_tick: u64) {
        assert_eq!(self.elapsed, 0);
        self.skip_run_ticks(run_tick);
    }

    pub fn skip_run_ticks(&mut self, count: u64) {
        self.elapsed = self.elapsed.checked_add(count).unwrap();
    }

    pub fn publish_with_system_rows(&mut self, second: u64) {
        self.publish_with_usage(second, &measured_process(self.elapsed).usage);
    }

    pub fn set_clock_intervals(&mut self, continuous_ns: u64, awake_ns: u64) {
        assert_eq!(self.elapsed, 0);
        assert!(continuous_ns > 0);
        assert!(awake_ns > 0);
        self.continuous_step_ns = continuous_ns;
        self.awake_step_ns = awake_ns;
    }

    pub fn publish_with_usage(&mut self, second: u64, usage: &Usage) {
        assert!(self.usage_override.is_none());
        self.usage_override = Some(*usage);
        let elapsed = u32::try_from(self.elapsed).unwrap();
        let cpus = [collector::machine::CPUTicks {
            user: elapsed,
            system: 0,
            nice: 0,
            idle: elapsed,
        }];
        let mut name = [0; 16];
        name[..3].copy_from_slice(b"en0");
        let interfaces = [collector::interfaces::Interface {
            index: 1,
            name: collector::interfaces::Name::from_bytes(name, 3).unwrap(),
            received_bytes: 0,
            sent_bytes: 0,
            received_packets: 0,
            sent_packets: 0,
            receive_errors: 0,
            send_errors: 0,
            receive_drops: 0,
        }];

        self.publish_sources(second, &cpus, &interfaces);
        assert!(self.usage_override.is_none());
    }

    fn publish_sources(
        &mut self,
        second: u64,
        cpus: &[collector::machine::CPUTicks],
        interfaces: &[collector::interfaces::Interface],
    ) {
        let mut process = measured_process(self.elapsed);
        if let Some(usage) = self.usage_override.take() {
            process.usage = usage;
        }
        let events = [Event::Exec { pid: 7 }];
        let memory = Memory::default();
        let mut readings = Readings {
            run_tick: self.elapsed,
            cpus,
            memory: &memory,
            interfaces,
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
            continuous_ns: self.elapsed * self.continuous_step_ns,
            awake_ns: self.elapsed * self.awake_step_ns,
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
        self.seal_with_observations(&[]);
    }

    /// Seals the published observations with the tables `derive` derives from them, as the
    /// persister does.
    pub fn seal_derived(
        &self,
        observations: &[persistence::Observation],
        derive: &dyn persistence::worker::Derive,
    ) {
        assert!(observations.len() <= 8);
        self.seal_window(None, observations, derive);
    }

    pub fn seal_minute(&self, minute: u64) {
        self.seal_minute_derived(minute, &Underived);
    }

    /// Seals the observations of run minute `minute` with the tables `derive` derives from
    /// them, as the persister does.
    pub fn seal_minute_derived(&self, minute: u64, derive: &dyn persistence::worker::Derive) {
        self.seal_window(Some(minute), &[], derive);
    }

    pub fn seal_with_observations(&self, observations: &[persistence::Observation]) {
        self.seal_derived(observations, &Underived);
    }

    fn seal_window(
        &self,
        minute: Option<u64>,
        observations: &[persistence::Observation],
        derive: &dyn persistence::worker::Derive,
    ) {
        let mut reader = RingReader::open_generation(self.writer.name()).unwrap();
        let window = self.seal_read(&mut reader, minute, observations);
        drop(reader);
        let store = persistence::Store::open(&self.root).unwrap();
        let latest = store.progress(&window.identity.run).unwrap();
        let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
            panic!("fixture already committed");
        };
        let mut tables = persistence::Encoder
            .encode(&window, pending.path())
            .unwrap();
        let predecessor = latest
            .filter(|latest| latest.publication_end == window.publication_range().start)
            .map(|latest| store.data_path(&latest));
        tables.extend(
            derive
                .derive(&persistence::worker::Derivation {
                    window: &window,
                    pending: pending.path(),
                    predecessor: predecessor.as_deref(),
                })
                .unwrap(),
        );
        let publications = window.publication_range();
        let events = window.event_range();
        // An observation of unknown wall time leaves the window's bounds unknown, as the
        // persister seals it.
        let walls: Option<Vec<u64>> = window.observations().map(|value| value.wall_ns).collect();
        let wall_ns = walls.map(|walls| persistence::TimeBounds {
            min: *walls.iter().min().unwrap(),
            max: *walls.iter().max().unwrap(),
        });
        store
            .commit(
                pending,
                persistence::Manifest {
                    identity: window.identity,
                    publication_first: publications.start,
                    publication_end: publications.end,
                    events_first: events.start,
                    events_end: events.end,
                    wall_ns,
                    continuous_ns: None,
                    awake_ns: None,
                    losses: persistence::Losses::default(),
                    data_directory: String::new(),
                    tables,
                },
            )
            .unwrap();
    }

    fn seal_read(
        &self,
        reader: &mut RingReader,
        minute: Option<u64>,
        observations: &[persistence::Observation],
    ) -> persistence::Window {
        let shape = reader.shape();
        let mut tick = persistence::tick_buffer(&shape).unwrap();
        let mut identities = Vec::with_capacity(2);
        let mut window = self.seal_read_window(minute, &shape);
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
            let (events_owned, event_identities) = seal_read_events(reader, &tick);
            if let Some(minute) = minute {
                if tick.run_tick / 60 != minute {
                    continue;
                }
            }
            let observation = seal_read_observation(&tick, publication, observations);
            let events: Vec<_> = events_owned
                .iter()
                .map(|(number, event)| persistence::NumberedEvent {
                    number: *number,
                    tick: tick.run_tick,
                    event,
                })
                .collect();
            if self.identity {
                window
                    .push_tick_with_identity(
                        &observation,
                        &tick,
                        &events,
                        &persistence::IdentitySource {
                            reader,
                            processes: &identities,
                            events: &event_identities,
                        },
                    )
                    .unwrap();
            } else {
                window.push_tick(&observation, &tick, &events).unwrap();
            }
        }
        window
    }

    fn seal_read_window(&self, minute: Option<u64>, shape: &Shape) -> persistence::Window {
        let window = minute.unwrap_or_default();
        persistence::Window::new(
            persistence::WindowIdentity {
                run: self.writer.identity().unwrap(),
                window,
            },
            persistence::Limits::from_shape(shape).unwrap(),
        )
    }
}

fn seal_read_events(
    reader: &mut RingReader,
    tick: &collector::ring::Tick,
) -> (Vec<(u64, Event)>, Vec<IdentityObservation>) {
    let count = usize::try_from(tick.events.end - tick.events.start).unwrap();
    assert!(count <= 32);
    let mut events = Vec::with_capacity(count);
    let mut identities = Vec::with_capacity(count);
    for expected in tick.events.clone() {
        let mut identity = IdentityObservation::EMPTY;
        let FoundEvent::Event {
            number,
            tick: run_tick,
            event,
        } = reader.read_event_identity(&mut identity).unwrap()
        else {
            panic!("event missing");
        };
        assert_eq!(number, expected);
        assert_eq!(run_tick, tick.run_tick);
        events.push((number, event));
        identities.push(identity);
    }

    (events, identities)
}

fn seal_read_observation(
    tick: &collector::ring::Tick,
    publication: u64,
    observations: &[persistence::Observation],
) -> persistence::Observation {
    assert!(observations.len() <= 8);
    assert!(
        observations
            .iter()
            .filter(|value| value.publication == publication)
            .count()
            <= 1
    );
    if let Some(observation) = observations
        .iter()
        .find(|value| value.publication == publication)
    {
        return *observation;
    }
    let times = tick.times.unwrap();
    persistence::Observation {
        publication,
        wall_ns: Some(times.wall_ns),
        continuous_ns: Some(times.continuous_ns),
        awake_ns: Some(times.awake_ns),
    }
}

/// Derives no table, as the persister of a build before derived tables sealed every window.
pub(crate) struct Underived;

impl persistence::worker::Derive for Underived {
    fn derive(
        &self,
        _derivation: &persistence::worker::Derivation<'_>,
    ) -> persistence::Result<Vec<persistence::TableManifest>> {
        Ok(Vec::new())
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
