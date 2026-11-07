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
