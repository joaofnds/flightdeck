//! Typed process and event columns retain measured payloads and original identity bytes.
use polars::prelude::*;

use crate::encoder::{columns, usage_columns};
use crate::owned_identity::{OwnedIdentity, Programs};
use crate::window::{EventRecord, StagedTick};
use crate::{Result, Window, schema};

struct ProcessRow<'window> {
    tick: &'window StagedTick,
    pid: i32,
    process: Option<&'window collector::processes::Process>,
    identity: Option<&'window OwnedIdentity>,
}

struct EventRow<'window> {
    tick: &'window StagedTick,
    event: &'window EventRecord,
    identity: Option<&'window OwnedIdentity>,
}

pub(crate) fn processes(window: &Window) -> Result<DataFrame> {
    let rows = processes_rows(window);
    let mut columns = columns!(rows;
        "publication" => |row| row.tick.observation.publication,
        "pid" => |row| row.pid,
    );
    columns.extend(usage_columns(&rows, |row| {
        row.process.map(|value| &value.usage)
    }));
    columns.extend(columns!(rows;
        "threads" => |row| row.process.map(|value| value.task.threads),
        "faults" => |row| row.process.map(|value| value.task.faults),
        "context_switches" => |row| row.process.map(|value| value.task.context_switches),
        "mach_syscalls" => |row| row.process.map(|value| value.task.mach_syscalls),
        "unix_syscalls" => |row| row.process.map(|value| value.task.unix_syscalls),
    ));
    if let Some(programs) = window.programs.as_deref() {
        columns.extend(identity_columns(&rows, programs, |row| row.identity));
    }

    Ok(DataFrame::new(rows.len(), columns)?)
}

fn processes_rows(window: &Window) -> Vec<ProcessRow<'_>> {
    let count = window
        .ticks
        .iter()
        .map(|tick| {
            tick.identities
                .as_ref()
                .map_or(tick.processes.len(), |value| value.processes.len())
        })
        .sum();
    let mut rows = Vec::with_capacity(count);
    for tick in &window.ticks {
        if let Some(identities) = &tick.identities {
            assert!(window.programs.is_some());
            let mut measured = 0;
            for identity in &identities.processes {
                let process = if identity.usage == schema::availability::MEASURED {
                    let process = &tick.processes[measured];
                    assert_eq!(process.pid, identity.pid);
                    measured += 1;
                    Some(process)
                } else {
                    None
                };
                rows.push(ProcessRow {
                    tick,
                    pid: identity.pid,
                    process,
                    identity: Some(identity),
                });
            }
            assert_eq!(measured, tick.processes.len());
        } else {
            assert!(window.programs.is_none());
            for process in &tick.processes {
                rows.push(ProcessRow {
                    tick,
                    pid: process.pid,
                    process: Some(process),
                    identity: None,
                });
            }
        }
    }
    assert_eq!(rows.len(), count);

    rows
}

pub(crate) fn events(window: &Window) -> Result<DataFrame> {
    let rows = events_rows(window);
    let mut columns = columns!(rows;
        "publication" => |row| row.tick.observation.publication,
        "event" => |row| row.event.number,
        "run_tick" => |row| row.event.tick,
        "pid" => |row| row.event.pid,
        "kind" => |row| row.event.kind,
        "coalition" => |row| row.event.coalition,
        "availability" => |row| row.event.availability,
        "noticed" => |row| row.event.noticed,
    );
    columns.extend(usage_columns(&rows, |row| row.event.usage.as_ref()));
    if let Some(programs) = window.programs.as_deref() {
        columns.extend(identity_columns(&rows, programs, |row| row.identity));
    }

    Ok(DataFrame::new(rows.len(), columns)?)
}

fn events_rows(window: &Window) -> Vec<EventRow<'_>> {
    let count = window.ticks.iter().map(|tick| tick.events.len()).sum();
    let mut rows = Vec::with_capacity(count);
    for tick in &window.ticks {
        if let Some(identities) = &tick.identities {
            assert!(window.programs.is_some());
            assert_eq!(tick.events.len(), identities.events.len());
            for (event, identity) in tick.events.iter().zip(&identities.events) {
                assert_eq!(event.pid, identity.pid);
                assert_eq!(event.availability, identity.usage);
                rows.push(EventRow {
                    tick,
                    event,
                    identity: Some(identity),
                });
            }
        } else {
            assert!(window.programs.is_none());
            for event in &tick.events {
                rows.push(EventRow {
                    tick,
                    event,
                    identity: None,
                });
            }
        }
    }
    assert_eq!(rows.len(), count);

    rows
}

fn identity_columns<'window, Row>(
    rows: &[Row],
    programs: &Programs,
    identity: impl Fn(&Row) -> Option<&'window OwnedIdentity>,
) -> Vec<Column> {
    columns!(rows;
        "identity_version" => |_| schema::IDENTITY_VERSION,
        "identity_availability" => |row| identity(row).map(|value| value.availability),
        "usage_availability" => |row| identity(row).map(|value| value.usage),
        "process_unique_id" => |row| {
            identity(row).and_then(|value| value.creation.map(|creation| creation.unique_id))
        },
        "pid_version" => |row| {
            identity(row).and_then(|value| value.creation.map(|creation| creation.pid_version))
        },
        "executable_name" => |row| {
            identity(row).and_then(|value| value.program.map(|index| programs.name(index)))
        },
        "app_path" => |row| {
            identity(row).and_then(|value| value.program.and_then(|index| programs.app(index)))
        },
    )
}
