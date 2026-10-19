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
    }
}

pub(crate) fn dense() -> Fixture {
    let mut fixture = Fixture::new();
    let identity = fixture.writer.identity().unwrap();
    let writer = collector::ring::Writer::create_with_identity(
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
    let events: Vec<_> = rows
        .iter()
        .filter(|row| row.exited)
        .map(|row| collector::tracker::Event::Exit {
            pid: row.pid,
            last: row.usage.ok_or(collector::processes::Unreadable::Refused),
            noticed: collector::tracker::Noticed::OnExit,
        })
        .collect();
    fixture
        .writer
        .publish_with_identity(
            &Readings {
                run_tick: publication,
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
                processes_refused: u32::try_from(
                    rows.iter().filter(|row| row.usage.is_none()).count(),
                )
                .unwrap(),
                processes_gone: 0,
                coalitions: &[],
                events: &events,
                dropped: collector::ring::Dropped::default(),
            },
            Some(&collector::ring::ObservationTimes {
                wall_ns: (1000 + publication) * 1_000_000_000,
                continuous_ns: publication * interval_ns,
                awake_ns: publication * interval_ns,
            }),
            &IdentityPublication {
                processes: &live,
                events: &event_identities,
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
    seal_window(fixture, &window);
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

fn seal_window(fixture: &Fixture, window: &persistence::Window) {
    let store = persistence::Store::open(&fixture.root).unwrap();
    let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
        panic!("selected window is already committed");
    };
    let tables = persistence::Encoder.encode(window, pending.path()).unwrap();
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
