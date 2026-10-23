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
        version: 1,
        unique_id: u64::try_from(pid).unwrap() * 100,
        threads: 1,
        gpu_ns: None,
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
    let mut fixture = Fixture::new();
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
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 600,
            coalition_capacity: 1,
            event_capacity: 1,
        },
        collector::ring::DictionaryCapacity {
            entries: 601,
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
    let times = collector::ring::ObservationTimes {
        wall_ns: (1000 + publication) * 1_000_000_000,
        continuous_ns: publication * interval_ns,
        awake_ns: publication * interval_ns,
    };
    publish_at(fixture, publication, rows, &times);
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
        .filter(|row| !row.exited)
        .filter_map(process)
        .collect();
    let live: Vec<_> = rows
        .iter()
        .zip(&identities)
        .filter(|(row, _)| !row.exited)
        .map(|(_, identity)| *identity)
        .collect();
    let event_identities: Vec<_> = rows
        .iter()
        .zip(&identities)
        .filter(|(row, _)| row.exited)
        .map(|(_, identity)| *identity)
        .collect();
    let events = publish_at_events(rows);
    let gpu_ns: Vec<_> = rows
        .iter()
        .filter(|row| !row.exited)
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

/// The exit of each row that exited.
fn publish_at_events(rows: &[Named<'_>]) -> Vec<collector::tracker::Event> {
    rows.iter()
        .filter(|row| row.exited)
        .map(|row| collector::tracker::Event::Exit {
            pid: row.pid,
            last: row.usage.ok_or(collector::processes::Unreadable::Refused),
            noticed: collector::tracker::Noticed::OnExit,
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
        usage: if row.usage.is_some() {
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

pub(crate) fn seal_selected(fixture: &Fixture, selected: &[u64]) {
    seal_selected_derived(fixture, selected, &crate::live::identity_fixture::Underived);
}

/// Seals the publications `selected` with the tables `derive` derives from them, as the
/// persister does.
pub(crate) fn seal_selected_derived(
    fixture: &Fixture,
    selected: &[u64],
    derive: &dyn persistence::worker::Derive,
) {
    assert!(selected.len() <= 8);
    let mut reader = collector::ring::RingReader::open_generation(fixture.writer.name()).unwrap();
    let shape = reader.shape();
    let mut tick = persistence::tick_buffer(&shape).unwrap();
    let mut identities = Vec::with_capacity(usize::try_from(shape.process_capacity).unwrap());
    let mut owned = Vec::with_capacity(32);
    let mut event_identities = Vec::with_capacity(32);
    let mut window = persistence::Window::new(
        persistence::WindowIdentity {
            run: fixture.writer.identity().unwrap(),
            window: 0,
        },
        persistence::Limits::from_shape(&shape).unwrap(),
    );
    for _index in 0..8 {
        let publication = match reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap()
        {
            collector::ring::Found::Publication { publication, .. } => publication,
            collector::ring::Found::Nothing => break,
            collector::ring::Found::Missed { .. } => panic!("selected sealing lost a publication"),
        };
        read_events(&mut reader, &tick, &mut owned, &mut event_identities);
        if !selected.contains(&publication) {
            continue;
        }
        let times = tick.times.unwrap();
        let events: Vec<_> = owned
            .iter()
            .map(|(number, run_tick, event)| persistence::NumberedEvent {
                number: *number,
                tick: *run_tick,
                event,
            })
            .collect();
        window
            .push_tick_with_identity(
                &persistence::Observation {
                    publication,
                    wall_ns: Some(times.wall_ns),
                    continuous_ns: Some(times.continuous_ns),
                    awake_ns: Some(times.awake_ns),
                },
                &tick,
                &events,
                &persistence::IdentitySource {
                    reader: &reader,
                    processes: &identities,
                    events: &event_identities,
                },
            )
            .unwrap();
    }
    seal_window(fixture, &window, derive);
}

fn read_events(
    reader: &mut collector::ring::RingReader,
    tick: &collector::ring::Tick,
    owned: &mut Vec<(u64, u64, collector::tracker::Event)>,
    identities: &mut Vec<IdentityObservation>,
) {
    owned.clear();
    identities.clear();
    assert!(tick.events.end - tick.events.start <= 32);
    for expected in tick.events.clone() {
        let mut identity = IdentityObservation::EMPTY;
        let collector::ring::FoundEvent::Event {
            number,
            tick,
            event,
        } = reader.read_event_identity(&mut identity).unwrap()
        else {
            panic!("missing exit event")
        };
        assert_eq!(number, expected);
        owned.push((number, tick, event));
        identities.push(identity);
    }
}

fn seal_window(
    fixture: &Fixture,
    window: &persistence::Window,
    derive: &dyn persistence::worker::Derive,
) {
    let store = persistence::Store::open(&fixture.root).unwrap();
    let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
        panic!("selected window is already committed");
    };
    let mut tables = persistence::Encoder.encode(window, pending.path()).unwrap();
    tables.extend(
        derive
            .derive(&persistence::worker::Derivation {
                window,
                pending: pending.path(),
            })
            .unwrap(),
    );
    let publications = window.publication_range();
    let events = window.event_range();
    let times: Vec<_> = window
        .observations()
        .filter_map(|observation| observation.wall_ns)
        .collect();
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
                    min: *times.iter().min().unwrap(),
                    max: *times.iter().max().unwrap(),
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
