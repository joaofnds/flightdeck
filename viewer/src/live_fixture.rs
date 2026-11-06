//! Real registered sources shared by focused live identity adapter tests.
use collector::machine::Memory;
use collector::processes::{Process, Task, Unreadable, Usage};
use collector::recording::{RegisteredRun, Registry, bootstrap, private};
use collector::ring::{
    DictionaryCapacity, Dropped, IdentityObservation, IdentityPublication, ObservationTimes, Place,
    ProcessCreation, ProcessIdentity, ProgramBytes, ProgramReference, Readings, RunIdentity, Shape,
    UsageAvailability, Writer,
};
use collector::tracker::Event;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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
    pub elapsed: u64,
    continuous_step_ns: u64,
    awake_step_ns: u64,
    usage_override: Option<Usage>,
    identity: bool,
    process_capacity: u64,
    owns_root: bool,
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
        Self::registered_of(true, 2)
    }

    /// A run whose ring keeps each process's identity where `identity` says so.
    fn registered_of(identity: bool, process_capacity: u64) -> Self {
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
        let writer = if identity {
            Writer::create_with_identity(&place, shape, capacity, &run, 0).unwrap()
        } else {
            Writer::create_recording(&place, shape, &run, 0).unwrap()
        };
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
            identity,
            process_capacity,
            owns_root: true,
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
        Self::registered_of(identity, process_capacity)
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
        self.seal_registered(minute, derive);
        if !observations.is_empty() {
            saved_clocks_rewrite(&self.marker(minute.unwrap_or_default()), observations);
        }
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

    /// The file that commits this run's saved window `window`.
    pub fn marker(&self, window: u64) -> PathBuf {
        saved_commitment(&self.root, &self.writer.identity().unwrap(), window)
    }
}

/// The file that commits what `run` saved as `window`, of those `root` holds.
pub(crate) fn saved_commitment(root: &Path, run: &RunIdentity, window: u64) -> PathBuf {
    persistence::commitment_path(root, &saved_window(root, run, window))
}

/// The manifest of what `run` saved as `window`, of those `root` holds.
pub(crate) fn saved_window(root: &Path, run: &RunIdentity, window: u64) -> persistence::Manifest {
    let budget = std::sync::Arc::new(persistence::resources::ByteBudget::new(1 << 28));
    persistence::canonical::catalog::Catalog::open(root, &budget)
        .unwrap()
        .windows_of(run, &mut persistence::canonical::ReadWork::default())
        .unwrap()
        .into_iter()
        .find(|saved| saved.identity.window == window)
        .unwrap_or_else(|| panic!("the run saved no window {window}"))
}

/// Rewrites the clocks of `observations` in the ticks table beside `commitment`, which the
/// persister never writes, so that a reader's refusal of such a table can be seen.
pub(crate) fn saved_clocks_rewrite(commitment: &Path, observations: &[persistence::Observation]) {
    type Clock = fn(&persistence::Observation) -> Option<u64>;
    let path = commitment.with_file_name("ticks.parquet");
    let mut ticks = persistence::engine::scan(&path).unwrap().collect().unwrap();
    let publications: Vec<u64> = ticks
        .column("publication")
        .unwrap()
        .u64()
        .unwrap()
        .into_no_null_iter()
        .collect();
    for observation in observations {
        assert!(publications.contains(&observation.publication));
    }
    let clocks: [(&str, Clock); 3] = [
        ("wall_ns", |observation| observation.wall_ns),
        ("continuous_ns", |observation| observation.continuous_ns),
        ("awake_ns", |observation| observation.awake_ns),
    ];
    for (name, clock) in clocks {
        let saved: Vec<Option<u64>> = ticks.column(name).unwrap().u64().unwrap().iter().collect();
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
