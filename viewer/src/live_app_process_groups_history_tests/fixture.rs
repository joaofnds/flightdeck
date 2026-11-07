use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

use collector::processes::{Process, Task, Usage};
use collector::ring::{
    IdentityObservation, IdentityPublication, ProcessCreation, ProcessIdentity, ProgramBytes,
    Readings, UsageAvailability,
};

use crate::live::identity_fixture::Fixture;

pub(crate) struct Named<'a> {
    pid: i32,
    name: &'a [u8],
    app: &'a [u8],
    pub usage: Option<Usage>,
    pub exited: bool,
    /// Whether the tick holds only the process's start, with no reading of it.
    pub started: bool,
    /// Whether the tick holds only the process's exec, with no reading of it.
    pub execed: bool,
    pub version: i32,
    pub unique_id: u64,
    pub threads: i32,
    /// The GPU time the tick reads for the process, which a ring with system records carries.
    pub gpu_ns: Option<u64>,
}

pub(crate) fn member<'a>(pid: i32, name: &'a [u8], app: &'a [u8], usage: Usage) -> Named<'a> {
    Named {
        pid,
        name,
        app,
        usage: Some(usage),
        exited: false,
        started: false,
        execed: false,
        version: 1,
        unique_id: u64::try_from(pid).unwrap() * 100,
        threads: 1,
        gpu_ns: None,
    }
}

impl Named<'_> {
    /// Whether the tick holds a reading of the process rather than an event of it.
    fn read(&self) -> bool {
        !self.exited && !self.started && !self.execed
    }
}

type CreateWriter = fn(
    &collector::ring::Place,
    collector::ring::Shape,
    collector::ring::DictionaryCapacity,
    &collector::ring::RunIdentity,
    u64,
) -> std::io::Result<collector::ring::Writer>;

pub(crate) fn dense() -> Fixture {
    dense_with(collector::ring::Writer::create_with_identity)
}

/// A dense ring whose ticks also carry the system records, each process's GPU time among them.
pub(crate) fn dense_with_system() -> Fixture {
    dense_with(collector::ring::Writer::create_with_system)
}

fn dense_with(create: CreateWriter) -> Fixture {
    dense_over(Fixture::new(), create)
}

fn dense_over(mut fixture: Fixture, create: CreateWriter) -> Fixture {
    let identity = fixture.writer.identity().unwrap();
    let writer = create(
        &collector::ring::Place {
            lock_path: fixture.root.join("dense.lock"),
            name_prefix: format!(
                "/fdpg{}x{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
        },
        collector::ring::Shape {
            slot_count: 16,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            // One App more than a page of Apps lists, each a process of its own.
            process_capacity: 2_049,
            coalition_capacity: 1,
            // An exit a tick for each tick the ring holds and the one it publishes.
            event_capacity: 3,
        },
        collector::ring::DictionaryCapacity {
            entries: 2_050,
            arena_bytes: 2 * 1024 * 1024,
        },
        &identity,
        0,
    )
    .unwrap();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = writer;
    collector::recording::Registry {
        runs: vec![collector::recording::RegisteredRun {
            identity,
            ring_name: fixture.writer.name().to_owned(),
            collector_pid: 0,
        }],
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    fixture
}

pub(crate) fn publish(fixture: &mut Fixture, publication: u64, rows: &[Named<'_>]) {
    publish_with_interval(fixture, publication, rows, 1_000_000_000);
}

pub(crate) fn publish_with_interval(
    fixture: &mut Fixture,
    publication: u64,
    rows: &[Named<'_>],
    interval_ns: u64,
) {
    let run_tick = fixture.elapsed + publication;
    let times = collector::ring::ObservationTimes {
        wall_ns: (1000 + publication) * 1_000_000_000,
        continuous_ns: run_tick * interval_ns,
        awake_ns: run_tick * interval_ns,
    };
    publish_at(fixture, run_tick, rows, &times);
}

/// Publishes `rows` as the tick `run_tick` of the run, taken at `times`.
pub(crate) fn publish_at(
    fixture: &mut Fixture,
    run_tick: u64,
    rows: &[Named<'_>],
    times: &collector::ring::ObservationTimes,
) {
    let identities: Vec<_> = rows.iter().map(|row| identity(fixture, row)).collect();
    let processes: Vec<_> = rows
        .iter()
        .filter(|row| row.read())
        .filter_map(process)
        .collect();
    let live: Vec<_> = rows
        .iter()
        .zip(&identities)
        .filter(|(row, _)| row.read())
        .map(|(_, identity)| *identity)
        .collect();
    let event_identities: Vec<_> = rows
        .iter()
        .zip(&identities)
        .filter(|(row, _)| !row.read())
        .map(|(_, identity)| *identity)
        .collect();
    let events = publish_at_events(rows);
    let gpu_ns: Vec<_> = rows
        .iter()
        .filter(|row| row.read())
        .map(|row| row.gpu_ns)
        .collect();
    let readings = Readings {
        run_tick,
        cpus: &[collector::machine::CPUTicks {
            user: 1,
            system: 0,
            nice: 0,
            idle: 1,
        }],
        memory: &collector::machine::Memory::default(),
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &processes,
        processes_refused: u32::try_from(rows.iter().filter(|row| row.usage.is_none()).count())
            .unwrap(),
        processes_gone: 0,
        coalitions: &[],
        events: &events,
        dropped: collector::ring::Dropped::default(),
    };
    let identities = IdentityPublication {
        processes: &live,
        events: &event_identities,
    };
    publish_at_write(
        fixture,
        &Publication {
            readings: &readings,
            times,
            identities: &identities,
            gpu_ns: &gpu_ns,
        },
    );
}

/// The event of each row the tick holds as an event rather than as a reading.
fn publish_at_events(rows: &[Named<'_>]) -> Vec<collector::tracker::Event> {
    rows.iter()
        .filter(|row| !row.read())
        .map(|row| {
            if row.started {
                collector::tracker::Event::Start {
                    pid: row.pid,
                    coalition: None,
                }
            } else if row.execed {
                collector::tracker::Event::Exec { pid: row.pid }
            } else {
                collector::tracker::Event::Exit {
                    pid: row.pid,
                    last: row.usage.ok_or(collector::processes::Unreadable::Refused),
                    noticed: collector::tracker::Noticed::OnExit,
                }
            }
        })
        .collect()
}

/// What one publication of the fixture writes to the ring.
struct Publication<'a> {
    readings: &'a Readings<'a>,
    times: &'a collector::ring::ObservationTimes,
    identities: &'a IdentityPublication<'a>,
    /// Each live process's GPU time, which only a ring with a system tail holds.
    gpu_ns: &'a [Option<u64>],
}

fn publish_at_write(fixture: &mut Fixture, publication: &Publication<'_>) {
    let Publication {
        readings,
        times,
        identities,
        gpu_ns,
    } = *publication;
    if !fixture.writer.system_supported() {
        assert!(gpu_ns.iter().all(Option::is_none));
        fixture
            .writer
            .publish_with_identity(readings, Some(times), identities)
            .unwrap();
        return;
    }
    let system = collector::ring::SystemPublication {
        reading: &collector::system::SystemReading::default(),
        machine: &collector::system::MachineFacts::default(),
        clock: None,
        process_gpu_ns: gpu_ns,
    };
    fixture
        .writer
        .publish_with_disks(
            readings,
            Some(times),
            identities,
            &collector::ring::SupplementalReadings {
                battery: None,
                disks: None,
                system: Some(&system),
            },
        )
        .unwrap();
}

fn identity(fixture: &mut Fixture, row: &Named<'_>) -> IdentityObservation {
    if row.name.is_empty() {
        return IdentityObservation {
            pid: row.pid,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Unavailable {
                creation: Some(ProcessCreation {
                    unique_id: row.unique_id,
                    pid_version: row.version,
                }),
                reason: collector::ring::IdentityUnavailable::Refused,
            },
        };
    }
    let mut program = None;
    fixture
        .writer
        .intern_program(
            &ProgramBytes {
                executable_name: row.name,
                app_path: Some(row.app),
            },
            &mut program,
        )
        .unwrap();
    IdentityObservation {
        pid: row.pid,
        usage: if row.started || row.execed {
            UsageAvailability::NotApplicable
        } else if row.usage.is_some() {
            UsageAvailability::Measured
        } else {
            UsageAvailability::Refused
        },
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: row.unique_id,
                pid_version: row.version,
            },
            program: program.unwrap(),
        },
    }
}

fn process(row: &Named<'_>) -> Option<Process> {
    row.usage.map(|usage| Process {
        pid: row.pid,
        usage,
        task: Task {
            threads: row.threads,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    })
}
