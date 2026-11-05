//! Real registered sources shared by focused live identity adapter tests.
use collector::machine::Memory;
use collector::processes::{Process, Task, Unreadable, Usage};
use collector::recording::{RegisteredRun, Registration, Registry, bootstrap, private};
use collector::ring::{
    DictionaryCapacity, Dropped, Found, FoundEvent, IdentityObservation, IdentityPublication,
    ObservationTimes, Place, ProcessCreation, ProcessIdentity, ProgramBytes, ProgramReference,
    Readings, RingReader, Shape, UsageAvailability, Writer,
};
use collector::tracker::Event;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SAVED_WINDOWS_MAX: u64 = 4096;
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
    pub elapsed: u64,
    continuous_step_ns: u64,
    awake_step_ns: u64,
    usage_override: Option<Usage>,
    identity: bool,
    process_capacity: u64,
    owns_root: bool,
    registered: bool,
    clocks: Clocks,
    persister: Mutex<persistence::worker::HeldWindows>,
}

/// What the fixture's next reading says of its clocks.
enum Clocks {
    /// Those of its second and run tick.
    Known,
    Unknown,
    Exactly(ObservationTimes),
}

impl Fixture {
    pub fn new() -> Self {
        Self::new_mode(true, 2, None)
    }

    /// A fixture whose run records into a registered recording, whose minutes it saves through the
    /// persister's own worker.
    pub fn registered() -> Self {
        Self::registered_of(2)
    }

    fn registered_of(process_capacity: u64) -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-live-registered-{}-{serial}",
            std::process::id()
        ));
        private::create_private_dir(&root).unwrap();
        let directory = private::PrivateDirectory::open(&root).unwrap();
        let lock = private::FileLock::try_lock_in(&directory, "registration.lock").unwrap();
        let recording = bootstrap::initialize(&directory, &lock, &mut [0; 4097]).unwrap();
        drop(lock);
        let place = Place {
            lock_path: root.join("identity.lock"),
            name_prefix: format!("/fdvr{}x{serial}", std::process::id()),
        };
        let slot_count = if process_capacity == 2 { 8 } else { 64 };
        let shape = Shape {
            slot_count,
            process_capacity,
            // Room for the one event `publish` gives each tick the ring holds and the tick the
            // writer logs.
            event_capacity: SHAPE.event_capacity.max(slot_count + 1),
            ..SHAPE
        };
        let run = collector::recording::new_run_identity(recording).unwrap();
        let capacity = DictionaryCapacity {
            entries: 4,
            arena_bytes: 32_768,
        };
        let writer = Writer::create_with_identity(&place, shape, capacity, &run, 0).unwrap();
        Registry {
            runs: vec![RegisteredRun {
                identity: run,
                ring_name: writer.name().to_owned(),
                collector_pid: 0,
            }],
            evictions: 0,
        }
        .write(&root)
        .unwrap();
        let mut fixture = Self {
            root,
            writer,
            elapsed: 0,
            continuous_step_ns: 0,
            awake_step_ns: 0,
            usage_override: None,
            identity: true,
            process_capacity,
            owns_root: true,
            registered: true,
            clocks: Clocks::Known,
            persister: Mutex::default(),
        };
        fixture.set_clock_intervals(1_000_000_000, 1_000_000_000);
        fixture
    }

    pub fn new_mode(identity: bool, process_capacity: u64, recording: Option<&Self>) -> Self {
        if let Some(recording) = recording {
            assert_eq!(process_capacity, 2);
            return recording.new_mode_next_run(identity);
        }
        if identity {
            return Self::registered_of(process_capacity);
        }
        Self::legacy_of(identity, process_capacity)
    }

    /// A recording of the format before registered recordings, whose rings may hold identities.
    pub fn legacy_of(identity: bool, process_capacity: u64) -> Self {
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
        let slot_count = if process_capacity == 2 { 8 } else { 64 };
        let shape = Shape {
            slot_count,
            process_capacity,
            // Room for the one event `publish` gives each tick the ring holds and the tick the
            // writer logs.
            event_capacity: SHAPE.event_capacity.max(slot_count + 1),
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
            registered: false,
            clocks: Clocks::Known,
            persister: Mutex::default(),
        };
        fixture.set_clock_intervals(1_000_000_000, 1_000_000_000);
        fixture
    }

    fn new_mode_identity(root: &Path, shape: &Shape, serial: u64, writer: &mut Writer) {
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

    /// A fixture that records the run after `self`'s into the same recording, which keeps each
    /// process's identity where `identity` says so.
    fn new_mode_next_run(&self, identity: bool) -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut run_identity = self.writer.identity().unwrap();
        run_identity.run.0[0] ^= 1;
        let writer = if identity {
            assert!(self.identity);
            let place = Place {
                lock_path: self.root.join("next.lock"),
                name_prefix: format!("/fdvn{}x{serial}", std::process::id()),
            };
            let capacity = DictionaryCapacity {
                entries: 4,
                arena_bytes: 32_768,
            };
            Writer::create_with_identity(&place, SHAPE, capacity, &run_identity, 0).unwrap()
        } else {
            let place = Place {
                lock_path: self.root.join("legacy.lock"),
                name_prefix: format!("/fdvl{}x{serial}", std::process::id()),
            };
            Writer::create_recording(&place, SHAPE, &run_identity, 0).unwrap()
        };

        self.new_mode_registered(writer, identity)
    }

    /// A fixture that records through `writer` the run after `self`'s, registered after it.
    fn new_mode_registered(&self, writer: Writer, identity: bool) -> Self {
        Registry {
            runs: vec![
                RegisteredRun {
                    identity: self.writer.identity().unwrap(),
                    ring_name: self.writer.name().to_owned(),
                    collector_pid: 0,
                },
                RegisteredRun {
                    identity: writer.identity().unwrap(),
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
            identity,
            process_capacity: 2,
            owns_root: false,
            registered: self.registered,
            clocks: Clocks::Known,
            persister: Mutex::default(),
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
        self.publish_with_events(second, &[Event::Exec { pid: 7 }]);
    }

    /// Publishes the tick of `second` with the processes `publish` publishes and `events`, each
    /// of a measured process whose program departed.
    pub fn publish_with_events(&mut self, second: u64, events: &[Event]) {
        self.publish_sources(second, &[], &[], events);
    }

    pub fn start_at_run_tick(&mut self, run_tick: u64) {
        assert_eq!(self.elapsed, 0);
        self.skip_run_ticks(run_tick);
    }

    pub fn skip_run_ticks(&mut self, count: u64) {
        self.elapsed = self.elapsed.checked_add(count).unwrap();
    }

    /// Publishes a reading whose clocks are unknown, as a source with no clocks leaves it.
    pub fn publish_without_clock(&mut self) {
        self.set_next_clocks(None);
        self.publish_with_system_rows(0);
    }

    /// Gives the next reading exactly `times`, or no clocks if there are none, rather than those of
    /// its second and run tick.
    pub fn set_next_clocks(&mut self, times: Option<ObservationTimes>) {
        self.clocks = times.map_or(Clocks::Unknown, Clocks::Exactly);
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

        self.publish_sources(second, &cpus, &interfaces, &[Event::Exec { pid: 7 }]);
        assert!(self.usage_override.is_none());
    }

    fn publish_sources(
        &mut self,
        second: u64,
        cpus: &[collector::machine::CPUTicks],
        interfaces: &[collector::interfaces::Interface],
        events: &[Event],
    ) {
        let mut process = measured_process(self.elapsed);
        if let Some(usage) = self.usage_override.take() {
            process.usage = usage;
        }
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
            events,
            dropped: Dropped::default(),
        };
        let times = match std::mem::replace(&mut self.clocks, Clocks::Known) {
            Clocks::Known => Some(ObservationTimes {
                wall_ns: second * 1_000_000_000,
                continuous_ns: self.elapsed * self.continuous_step_ns,
                awake_ns: self.elapsed * self.awake_step_ns,
            }),
            Clocks::Unknown => None,
            Clocks::Exactly(times) => Some(times),
        };
        match times {
            Some(times) if !self.identity => self.writer.publish_observed(&readings, &times),
            times => self.publish_identity(&mut readings, times.as_ref()),
        }
        self.elapsed += 1;
    }

    fn publish_identity(&mut self, readings: &mut Readings<'_>, times: Option<&ObservationTimes>) {
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
        let events: Vec<_> = readings
            .events
            .iter()
            .map(|event| departed_observation(event, &departed))
            .collect();
        readings.processes_refused = u32::try_from(self.process_capacity - 1).unwrap();
        self.writer
            .publish_with_identity(
                readings,
                times,
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

    pub(crate) fn seal_window(
        &self,
        minute: Option<u64>,
        observations: &[persistence::Observation],
        derive: &dyn persistence::worker::Derive,
    ) {
        assert!(observations.len() <= 8);
        if self.registered {
            self.seal_registered(minute, derive);
            if !observations.is_empty() {
                self.seal_registered_change_clocks(minute.unwrap_or_default(), observations);
            }
            return;
        }
        let mut reader = RingReader::open_generation(self.writer.name()).unwrap();
        let window = self.seal_read(&mut reader, minute, observations);
        drop(reader);
        let store = persistence::Store::open(&self.root).unwrap();
        let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
            panic!("fixture already committed");
        };
        let mut tables = persistence::Encoder
            .encode(&window, pending.path())
            .unwrap();
        tables.extend(
            derive
                .derive(&persistence::worker::Derivation {
                    window: &window,
                    pending: &private::PrivateDirectory::open(pending.path()).unwrap(),
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
                    event_batches: None,
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

    fn seal_registered(&self, window_last: Option<u64>, derive: &dyn persistence::worker::Derive) {
        let store = persistence::Store::open(&self.root).unwrap();
        let run = RegisteredRun {
            identity: self.writer.identity().unwrap(),
            ring_name: self.writer.name().to_owned(),
            collector_pid: 0,
        };
        let mut held = self.persister.lock().unwrap();
        persistence::worker::seal_held_windows(&store, &run, window_last, &mut held, derive)
            .unwrap();
    }

    /// Rewrites the clocks of `observations` in the ticks table saved for `window`, which the
    /// persister never writes, so that a reader's refusal of such a table can be seen.
    fn seal_registered_change_clocks(
        &self,
        window: u64,
        observations: &[persistence::Observation],
    ) {
        type Clock = fn(&persistence::Observation) -> Option<u64>;
        let manifest = persistence::read_committed(&self.root, &self.marker(window)).unwrap();
        let path = self
            .root
            .join(manifest.data_directory)
            .join("ticks.parquet");
        let mut ticks = persistence::engine::scan(&path).unwrap().collect().unwrap();
        let publications: Vec<u64> = ticks
            .column("publication")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect();
        let clocks: [(&str, Clock); 3] = [
            ("wall_ns", |observation| observation.wall_ns),
            ("continuous_ns", |observation| observation.continuous_ns),
            ("awake_ns", |observation| observation.awake_ns),
        ];
        for (name, clock) in clocks {
            let saved: Vec<Option<u64>> =
                ticks.column(name).unwrap().u64().unwrap().iter().collect();
            let changed: Vec<Option<u64>> = publications
                .iter()
                .zip(saved)
                .map(|(publication, saved)| {
                    observations
                        .iter()
                        .find(|observation| observation.publication == *publication)
                        .map_or(saved, clock)
                })
                .collect();
            ticks
                .with_column(polars::prelude::Column::new(name.into(), changed))
                .unwrap();
        }
        std::fs::remove_file(&path).unwrap();
        polars::prelude::ParquetWriter::new(private::open_private_file(&path, true).unwrap())
            .finish(&mut ticks)
            .unwrap();
    }

    /// The file that commits this run's saved window `window`.
    pub fn marker(&self, window: u64) -> PathBuf {
        let run = self.writer.identity().unwrap();
        if !self.registered {
            return persistence::layout::WindowKey::from(&persistence::WindowIdentity {
                run,
                window,
            })
            .marker(&self.root);
        }
        let budget = std::sync::Arc::new(persistence::resources::ByteBudget::new(1 << 28));
        let catalog = persistence::canonical::catalog::Catalog::open(&self.root, &budget).unwrap();
        let mut work = persistence::canonical::ReadWork::default();
        let mut publication = u64::MAX;
        for _saved in 0..SAVED_WINDOWS_MAX {
            let saved = catalog
                .window_at_or_before(&run, publication, &mut work)
                .unwrap()
                .unwrap();
            if saved.identity.window == window {
                return persistence::commitment_path(&self.root, &saved);
            }
            publication = saved.publication_first - 1;
        }
        panic!("the run saved no window {window}");
    }

    fn seal_read(
        &self,
        reader: &mut RingReader,
        minute: Option<u64>,
        observations: &[persistence::Observation],
    ) -> persistence::Window {
        let shape = reader.shape();
        let mut tick = persistence::tick_buffer(&shape).unwrap();
        let mut identities = Vec::with_capacity(usize::try_from(shape.process_capacity).unwrap());
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
    persistence::Observation {
        publication,
        wall_ns: tick.times.map(|times| times.wall_ns),
        continuous_ns: tick.times.map(|times| times.continuous_ns),
        awake_ns: tick.times.map(|times| times.awake_ns),
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

/// The identity of `event`'s process, whose program `departed` names, with the usage the
/// ring holds for an event of its kind.
fn departed_observation(event: &Event, departed: &ProgramReference) -> IdentityObservation {
    let (pid, usage) = match event {
        Event::Start { pid, .. } | Event::Exec { pid } => (*pid, UsageAvailability::NotApplicable),
        Event::Exit { pid, last, .. } => match last {
            Ok(_) => (*pid, UsageAvailability::Measured),
            Err(Unreadable::Refused) => (*pid, UsageAvailability::Refused),
            Err(Unreadable::Gone) => (*pid, UsageAvailability::Gone),
        },
    };
    IdentityObservation {
        pid,
        usage,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: u64::try_from(pid).unwrap() * 100,
                pid_version: -10 * pid,
            },
            program: *departed,
        },
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
