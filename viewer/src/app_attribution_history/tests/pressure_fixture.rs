use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::ring::{
    DictionaryCapacity, IdentityObservation, IdentityPublication, ProcessCreation, ProcessIdentity,
    ProgramBytes, Readings, Shape, UsageAvailability,
};

pub(super) struct Recording {
    pub root: PathBuf,
    writer: collector::ring::Writer,
}

static NEXT: AtomicU64 = AtomicU64::new(0);
const SHAPE: Shape = Shape {
    slot_count: 2,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 300,
    coalition_capacity: 1,
    event_capacity: 1,
};

impl Recording {
    pub fn new() -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-attribution-pressure-{}-{serial}",
            std::process::id()
        ));
        collector::recording::private::create_private_dir(&root).unwrap();
        let place = collector::ring::Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdap{}x{serial}", std::process::id()),
        };
        let (writer, registered) = collector::recording::Registration::start_with_identity(
            &root,
            &place,
            SHAPE,
            DictionaryCapacity {
                entries: 301,
                arena_bytes: 2 * 1024 * 1024,
            },
            0,
        )
        .unwrap()
        .into_parts();
        registered.unwrap();
        let mut recording = Self { root, writer };
        recording.capture();
        collector::recording::Registry {
            runs: Vec::new(),
            evictions: 0,
        }
        .write(&recording.root)
        .unwrap();
        recording
    }

    fn capture(&mut self) {
        let mut reader = collector::ring::RingReader::open_generation(self.writer.name()).unwrap();
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        let mut identities = Vec::with_capacity(300);
        for minute in 0..3 {
            let mut window = persistence::Window::new(
                persistence::WindowIdentity {
                    run: self.writer.identity().unwrap(),
                    window: minute,
                },
                persistence::Limits::from_shape(&SHAPE).unwrap(),
            );
            let end = (minute * 60 + 60).min(128);
            for run_tick in minute * 60..end {
                self.publish(run_tick);
                let found = reader
                    .read_identity_into(&mut tick, &mut identities)
                    .unwrap();
                let collector::ring::Found::Publication { publication, .. } = found else {
                    panic!("pressure fixture lost a scheduled reading");
                };
                let times = tick.times.unwrap();
                window
                    .push_tick_with_identity(
                        &persistence::Observation {
                            publication,
                            wall_ns: Some(times.wall_ns),
                            continuous_ns: Some(times.continuous_ns),
                            awake_ns: Some(times.awake_ns),
                        },
                        &tick,
                        &[],
                        &persistence::IdentitySource {
                            reader: &reader,
                            processes: &identities,
                            events: &[],
                        },
                    )
                    .unwrap();
            }
            self.seal(&window, end);
        }
    }

    fn publish(&mut self, run_tick: u64) {
        let mut processes = Vec::with_capacity(300);
        let mut identities = Vec::with_capacity(300);
        self.publish_members(run_tick, &mut processes, &mut identities);
        self.writer
            .publish_with_identity(
                &Readings {
                    run_tick,
                    cpus: &[collector::machine::CPUTicks {
                        user: u32::try_from(run_tick).unwrap(),
                        system: 0,
                        nice: 0,
                        idle: u32::try_from(run_tick).unwrap(),
                    }],
                    memory: &collector::machine::Memory::default(),
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes: &processes,
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[],
                    dropped: collector::ring::Dropped::default(),
                },
                Some(&collector::ring::ObservationTimes {
                    wall_ns: (1000 + run_tick) * 1_000_000_000,
                    continuous_ns: run_tick * 1_000_000_000,
                    awake_ns: run_tick * 1_000_000_000,
                }),
                &IdentityPublication {
                    processes: &identities,
                    events: &[],
                },
            )
            .unwrap();
    }

    fn publish_members(
        &mut self,
        run_tick: u64,
        processes: &mut Vec<collector::processes::Process>,
        identities: &mut Vec<IdentityObservation>,
    ) {
        for index in 0_u32..300 {
            let mut path = format!("/Apps/{index:04}/").into_bytes();
            path.resize(2044, b'x');
            path.extend_from_slice(b".app");
            let mut program = None;
            self.writer
                .intern_program(
                    &ProgramBytes {
                        executable_name: b"main",
                        app_path: Some(&path),
                    },
                    &mut program,
                )
                .unwrap();
            let pid = i32::try_from(index + 1).unwrap();
            identities.push(IdentityObservation {
                pid,
                usage: UsageAvailability::Measured,
                identity: ProcessIdentity::Measured {
                    creation: ProcessCreation {
                        unique_id: u64::try_from(pid).unwrap(),
                        pid_version: 1,
                    },
                    program: program.unwrap(),
                },
            });
            processes.push(collector::processes::Process {
                pid,
                usage: super::usage((run_tick + 1) * u64::from(index + 1) * 1000),
                task: collector::processes::Task {
                    threads: 1,
                    faults: 0,
                    context_switches: 0,
                    mach_syscalls: 0,
                    unix_syscalls: 0,
                },
            });
        }
    }

    fn seal(&self, window: &persistence::Window, end: u64) {
        let store = persistence::Store::open(&self.root).unwrap();
        let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
            panic!("pressure window was already committed");
        };
        let tables = persistence::Encoder.encode(window, pending.path()).unwrap();
        let publications = window.publication_range();
        let events = window.event_range();
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
                        min: (1000 + window.identity.window * 60) * 1_000_000_000,
                        max: (999 + end) * 1_000_000_000,
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
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.writer.finish();
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
