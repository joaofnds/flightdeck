use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

use collector::processes::{Process, Task, Usage};
use collector::ring::{
    IdentityObservation, IdentityPublication, ProcessCreation, ProcessIdentity, ProgramBytes,
    Readings, UsageAvailability,
};

use crate::live::identity_fixture::Fixture;

pub(super) struct Named<'a> {
    pid: i32,
    name: &'a [u8],
    app: &'a [u8],
    pub usage: Option<Usage>,
    pub exited: bool,
}

pub(super) fn member<'a>(pid: i32, name: &'a [u8], app: &'a [u8], usage: Usage) -> Named<'a> {
    Named {
        pid,
        name,
        app,
        usage: Some(usage),
        exited: false,
    }
}

pub(super) fn dense() -> Fixture {
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

pub(super) fn dense_context(fixture: &mut Fixture, session: &mut crate::LiveSession) {
    let paths: Vec<_> = (0..600)
        .map(|index| {
            let mut path = format!("/Apps/{index:04}/").into_bytes();
            path.resize(1020, b'x');
            path.extend_from_slice(b".app");
            path
        })
        .collect();
    let mut rows: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            member(
                i32::try_from(index + 1).unwrap(),
                b"worker",
                path,
                super::usage(),
            )
        })
        .collect();
    for publication in 1..=900 {
        for row in &mut rows {
            let usage = row.usage.as_mut().unwrap();
            usage.user_ns += 1000;
            usage.energy_nj += 2000;
        }
        publish(fixture, publication, &rows);
        session.poll().unwrap();
    }
}

pub(super) fn publish(fixture: &mut Fixture, publication: u64, rows: &[Named<'_>]) {
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
                run_tick: fixture.elapsed + publication,
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
                continuous_ns: (fixture.elapsed + publication) * 1_000_000_000,
                awake_ns: (fixture.elapsed + publication) * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &live,
                events: &event_identities,
            },
        )
        .unwrap();
}

fn identity(fixture: &mut Fixture, row: &Named<'_>) -> IdentityObservation {
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
                unique_id: u64::try_from(row.pid).unwrap() * 100,
                pid_version: 1,
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
            threads: 1,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    })
}

/// Saves the first window, publishing `publications` with `rows` once the persister reads its
/// first publication, so that they overwrite the second in the ring before it gets there.
pub(super) fn seal_past_lost_second(
    fixture: &mut Fixture,
    publications: std::ops::Range<u64>,
    rows: &[Named<'_>],
) {
    let store = persistence::Store::open(&fixture.root).unwrap();
    let run = collector::recording::RegisteredRun {
        identity: fixture.writer.identity().unwrap(),
        ring_name: fixture.writer.name().to_owned(),
        collector_pid: 0,
    };
    persistence::worker::seal_first_window_overrun(
        &store,
        &run,
        &crate::live::identity_fixture::Underived,
        &mut || {
            for publication in publications.clone() {
                publish(fixture, publication, rows);
            }
        },
    )
    .unwrap();
}
