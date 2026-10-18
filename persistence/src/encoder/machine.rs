use polars::prelude::*;

use crate::encoder::columns;
use crate::{Error, Result, Window};

pub(super) fn memory(window: &Window) -> Result<DataFrame> {
    let rows = &window.ticks;
    let columns = columns!(rows;
        "publication" => |tick| tick.observation.publication,
        "total_bytes" => |tick| tick.memory.total_bytes,
        "free_bytes" => |tick| tick.memory.free_bytes,
        "active_bytes" => |tick| tick.memory.active_bytes,
        "inactive_bytes" => |tick| tick.memory.inactive_bytes,
        "speculative_bytes" => |tick| tick.memory.speculative_bytes,
        "wired_bytes" => |tick| tick.memory.wired_bytes,
        "compressed_bytes" => |tick| tick.memory.compressed_bytes,
        "anonymous_bytes" => |tick| tick.memory.anonymous_bytes,
        "file_backed_bytes" => |tick| tick.memory.file_backed_bytes,
        "purgeable_bytes" => |tick| tick.memory.purgeable_bytes,
        "swap_total_bytes" => |tick| tick.memory.swap_total_bytes,
        "swap_used_bytes" => |tick| tick.memory.swap_used_bytes,
        "swapped_in_bytes" => |tick| tick.memory.swapped_in_bytes,
        "swapped_out_bytes" => |tick| tick.memory.swapped_out_bytes,
        "pageins" => |tick| tick.memory.pageins,
        "pageouts" => |tick| tick.memory.pageouts,
        "compressions" => |tick| tick.memory.compressions,
        "decompressions" => |tick| tick.memory.decompressions,
        "pressure" => |tick| match tick.memory.pressure {
            collector::machine::Pressure::Normal => crate::schema::pressure::NORMAL,
            collector::machine::Pressure::Warning => crate::schema::pressure::WARNING,
            collector::machine::Pressure::Critical => crate::schema::pressure::CRITICAL,
        },
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}

pub(super) fn cpus(window: &Window) -> Result<DataFrame> {
    let mut rows = Vec::with_capacity(super::bounds::rows(crate::schema::Table::Cpus, window));
    for tick in &window.ticks {
        for (index, cpu) in tick.cpus.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| Error::Bound("cpu index"))?;
            rows.push((tick.observation.publication, index, cpu));
        }
    }
    let columns = columns!(rows;
        "publication" => |row| row.0,
        "cpu" => |row| row.1,
        "user_ticks" => |row| row.2.user,
        "system_ticks" => |row| row.2.system,
        "nice_ticks" => |row| row.2.nice,
        "idle_ticks" => |row| row.2.idle,
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}

pub(super) fn interfaces(window: &Window) -> Result<DataFrame> {
    let rows = window
        .ticks
        .iter()
        .flat_map(|tick| {
            tick.interfaces
                .iter()
                .map(move |interface| (tick.observation.publication, interface))
        })
        .collect::<Vec<_>>();
    let columns = columns!(rows;
        "publication" => |row| row.0,
        "interface" => |row| row.1.index,
        "name" => |row| row.1.name.as_str(),
        "received_bytes" => |row| row.1.received_bytes,
        "sent_bytes" => |row| row.1.sent_bytes,
        "received_packets" => |row| row.1.received_packets,
        "sent_packets" => |row| row.1.sent_packets,
        "receive_errors" => |row| row.1.receive_errors,
        "send_errors" => |row| row.1.send_errors,
        "receive_drops" => |row| row.1.receive_drops,
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}

pub(super) fn gpu(window: &Window) -> Result<DataFrame> {
    let mut rows = Vec::with_capacity(super::bounds::rows(crate::schema::Table::GpuStates, window));
    for tick in &window.ticks {
        if tick.ioreport_read {
            for (index, active_ns) in tick.gpu_active_ns.iter().enumerate() {
                let index = u32::try_from(index).map_err(|_| Error::Bound("gpu state index"))?;
                rows.push((tick.observation.publication, index, *active_ns));
            }
        }
    }
    let columns = columns!(rows;
        "publication" => |row| row.0,
        "state" => |row| row.1,
        "active_ns" => |row| row.2,
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}

pub(super) fn cores(window: &Window) -> Result<DataFrame> {
    let mut rows = Vec::with_capacity(super::bounds::rows(
        crate::schema::Table::CoreStates,
        window,
    ));
    for tick in &window.ticks {
        if tick.ioreport_read {
            for (core, states) in tick.cores.iter().enumerate() {
                let core = u32::try_from(core).map_err(|_| Error::Bound("core index"))?;
                for (state, active_ns) in states.iter().enumerate() {
                    let state =
                        u32::try_from(state).map_err(|_| Error::Bound("core state index"))?;
                    rows.push((tick.observation.publication, core, state, *active_ns));
                }
            }
        }
    }
    let columns = columns!(rows;
        "publication" => |row| row.0,
        "core" => |row| row.1,
        "state" => |row| row.2,
        "active_ns" => |row| row.3,
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}

pub(super) fn fans(window: &Window) -> Result<DataFrame> {
    let mut rows = Vec::with_capacity(super::bounds::rows(crate::schema::Table::Fans, window));
    for tick in &window.ticks {
        if tick.smc_read {
            for (index, rpm) in tick.fan_rpm.iter().enumerate() {
                let index = u32::try_from(index).map_err(|_| Error::Bound("fan index"))?;
                rows.push((tick.observation.publication, index, *rpm));
            }
        }
    }
    let columns = columns!(rows;
        "publication" => |row| row.0,
        "fan" => |row| row.1,
        "rpm" => |row| row.2,
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}

pub(super) fn coalitions(window: &Window) -> Result<DataFrame> {
    let rows = window
        .ticks
        .iter()
        .flat_map(|tick| {
            tick.coalitions
                .iter()
                .map(move |coalition| (tick.observation.publication, coalition))
        })
        .collect::<Vec<_>>();
    let columns = columns!(rows;
        "publication" => |row| row.0,
        "coalition" => |row| row.1.id,
        "tasks_started" => |row| row.1.tasks_started,
        "tasks_exited" => |row| row.1.tasks_exited,
        "cpu_ns" => |row| row.1.cpu_ns,
        "interrupt_wakeups" => |row| row.1.interrupt_wakeups,
        "idle_wakeups" => |row| row.1.idle_wakeups,
        "disk_read_bytes" => |row| row.1.disk_read_bytes,
        "disk_written_bytes" => |row| row.1.disk_written_bytes,
        "gpu_ns" => |row| row.1.gpu_ns,
        "energy_nj" => |row| row.1.energy_nj,
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}
