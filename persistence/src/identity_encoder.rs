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
    gpu_ns: Option<u64>,
}

struct EventRow<'window> {
    tick: &'window StagedTick,
    event: &'window EventRecord,
    identity: Option<&'window OwnedIdentity>,
}

pub(crate) fn processes(window: &Window) -> Result<DataFrame> {
    processes_frame(window, &processes_rows(window))
}

/// The processes frame in the order its file keeps, by pid, start and publication, which an
/// integer sort of the rows reaches sooner than a sort of the frame's columns.
pub(crate) fn processes_in_file_order(window: &Window) -> Result<DataFrame> {
    let mut rows = processes_rows(window);
    rows.sort_by_key(|row| {
        let awake = row.process.map(|value| value.usage.awake_ns_at_start);
        (
            row.pid,
            awake.is_none(),
            awake,
            row.tick.observation.publication,
        )
    });
    processes_frame(window, &rows)
}

fn processes_frame(window: &Window, rows: &[ProcessRow<'_>]) -> Result<DataFrame> {
    let mut columns = columns!(rows;
        "publication" => |row| row.tick.observation.publication,
        "pid" => |row| row.pid,
    );
    columns.extend(usage_columns(rows, |row| {
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
        columns.extend(identity_columns(rows, programs, |row| row.identity));
        if window.system_captured() {
            columns.extend(columns!(rows; "gpu_ns" => |row| row.gpu_ns));
        }
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
            if !tick.process_gpu_ns.is_empty() {
                assert_eq!(tick.process_gpu_ns.len(), identities.processes.len());
            }
            let mut measured = 0;
            for (index, identity) in identities.processes.iter().enumerate() {
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
                    gpu_ns: tick.process_gpu_ns.get(index).copied().flatten(),
                });
            }
            assert_eq!(measured, tick.processes.len());
        } else {
            assert!(window.programs.is_none());
            assert!(tick.process_gpu_ns.is_empty());
            for process in &tick.processes {
                rows.push(ProcessRow {
                    tick,
                    pid: process.pid,
                    process: Some(process),
                    identity: None,
                    gpu_ns: None,
                });
            }
        }
    }
    assert_eq!(rows.len(), count);

    rows
}

pub(crate) fn events(window: &Window) -> Result<DataFrame> {
    events_frame(window, &events_rows(window))
}

/// The events frame in the order its file keeps, by pid, start and event.
pub(crate) fn events_in_file_order(window: &Window) -> Result<DataFrame> {
    let mut rows = events_rows(window);
    rows.sort_by_key(|row| {
        let awake = row
            .event
            .usage
            .as_ref()
            .map(|value| value.awake_ns_at_start);
        (row.event.pid, awake.is_none(), awake, row.event.number)
    });
    events_frame(window, &rows)
}

fn events_frame(window: &Window, rows: &[EventRow<'_>]) -> Result<DataFrame> {
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
    columns.extend(usage_columns(rows, |row| row.event.usage.as_ref()));
    if let Some(programs) = window.programs.as_deref() {
        columns.extend(identity_columns(rows, programs, |row| row.identity));
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
