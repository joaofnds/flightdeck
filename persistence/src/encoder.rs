use std::path::Path;

use duckdb::{Connection, params};

use crate::schema::{TABLES, Table};
use crate::window::StagedTick;
use crate::{Error, Result, Window};

/// Encoder connections live for one window so native state cannot accumulate across seals.
pub struct Encoder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableManifest {
    pub name: &'static str,
    pub rows: u64,
}

impl Encoder {
    /// Writes complete typed files into an already private directory. The caller owns
    /// synchronization/publication, and must release its ring mappings before this call.
    pub fn encode(&self, window: &Window, pending: &Path) -> Result<Vec<TableManifest>> {
        if window.ticks.is_empty() {
            return Err(Error::Bound("empty window"));
        }
        // Configure before open so construction never starts the default CPU-sized pool.
        let config = duckdb::Config::default().threads(1)?;
        let connection = Connection::open_in_memory_with_flags(config)?;
        connection.execute_batch(
            "SET memory_limit='512MiB'; \
            SET max_temp_directory_size='0B'; SET autoinstall_known_extensions=false; \
            SET autoload_known_extensions=false;",
        )?;
        let mut tables = Vec::with_capacity(TABLES.len());
        for (table, columns, order) in TABLES {
            let name = table.name();
            connection.execute_batch(&format!("CREATE TABLE {name} ({columns})"))?;
            append(&connection, table, window)?;
            let count: u64 =
                connection.query_row(&format!("SELECT count(*) FROM {name}"), [], |row| {
                    row.get(0)
                })?;
            let path = pending.join(format!("{name}.parquet"));
            if path.try_exists()? {
                return Err(Error::Bound("existing table"));
            }
            let path = path
                .to_str()
                .ok_or(Error::Bound("path encoding"))?
                .replace('\'', "''");
            connection.execute_batch(&format!(
                "COPY (SELECT * FROM {name} ORDER BY {order}) \
                TO '{path}' (FORMAT PARQUET, COMPRESSION ZSTD, COMPRESSION_LEVEL 3, \
                ROW_GROUP_SIZE 16384, WRITE_BLOOM_FILTER false)"
            ))?;
            connection.execute_batch(&format!("DROP TABLE {name}"))?;
            tables.push(TableManifest { name, rows: count });
        }
        Ok(tables)
    }
}

pub fn append(connection: &Connection, table: Table, window: &Window) -> Result<()> {
    let mut appender = connection.appender(table.name())?;
    for tick in &window.ticks {
        match table {
            Table::Ticks => append_tick(&mut appender, tick)?,
            Table::Memory => append_memory(&mut appender, tick)?,
            Table::Cpus => append_cpus(&mut appender, tick)?,
            Table::Interfaces => append_interfaces(&mut appender, tick)?,
            Table::GpuStates => append_gpu(&mut appender, tick)?,
            Table::CoreStates => append_cores(&mut appender, tick)?,
            Table::Fans => append_fans(&mut appender, tick)?,
            Table::Processes => append_processes(&mut appender, tick)?,
            Table::Coalitions => append_coalitions(&mut appender, tick)?,
            Table::Events => append_events(&mut appender, tick)?,
        }
    }
    appender.flush()?;
    Ok(())
}

fn append_tick(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    let observation = tick.observation;
    let energy = tick.ioreport_read.then_some(tick.gpu_energy_nj);
    let power = tick.smc_read.then_some(tick.power_watts);
    let temperature = tick.smc_read.then_some(tick.soc_celsius);
    let states = if tick.ioreport_read {
        tick.gpu_active_ns.len()
    } else {
        0
    };
    let cores = if tick.ioreport_read {
        tick.cores.len()
    } else {
        0
    };
    appender.append_row(params![
        observation.publication,
        tick.run_tick,
        observation.wall_ns,
        observation.continuous_ns,
        observation.awake_ns,
        tick.ioreport_read,
        tick.smc_read,
        energy,
        power,
        temperature,
        tick.processes_refused,
        tick.processes_gone,
        tick.events_first,
        tick.events_end,
        tick.dropped.starts,
        tick.dropped.execs,
        tick.dropped.exits,
        u32::try_from(states).map_err(|_| Error::Bound("gpu states"))?,
        u32::try_from(cores).map_err(|_| Error::Bound("cores"))?
    ])?;
    Ok(())
}

fn append_memory(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    let memory = tick.memory;
    let pressure: u8 = match memory.pressure {
        collector::machine::Pressure::Normal => crate::schema::pressure::NORMAL,
        collector::machine::Pressure::Warning => crate::schema::pressure::WARNING,
        collector::machine::Pressure::Critical => crate::schema::pressure::CRITICAL,
    };
    appender.append_row(params![
        tick.observation.publication,
        memory.total_bytes,
        memory.free_bytes,
        memory.active_bytes,
        memory.inactive_bytes,
        memory.speculative_bytes,
        memory.wired_bytes,
        memory.compressed_bytes,
        memory.anonymous_bytes,
        memory.file_backed_bytes,
        memory.purgeable_bytes,
        memory.swap_total_bytes,
        memory.swap_used_bytes,
        memory.swapped_in_bytes,
        memory.swapped_out_bytes,
        memory.pageins,
        memory.pageouts,
        memory.compressions,
        memory.decompressions,
        pressure
    ])?;
    Ok(())
}

fn append_cpus(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    for (index, cpu) in tick.cpus.iter().enumerate() {
        appender.append_row(params![
            tick.observation.publication,
            u32::try_from(index).map_err(|_| Error::Bound("cpu index"))?,
            cpu.user,
            cpu.system,
            cpu.nice,
            cpu.idle
        ])?;
    }
    Ok(())
}

fn append_interfaces(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    for interface in &tick.interfaces {
        appender.append_row(params![
            tick.observation.publication,
            interface.index,
            interface.name.as_str(),
            interface.received_bytes,
            interface.sent_bytes,
            interface.received_packets,
            interface.sent_packets,
            interface.receive_errors,
            interface.send_errors,
            interface.receive_drops
        ])?;
    }
    Ok(())
}

fn append_gpu(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    if !tick.ioreport_read {
        return Ok(());
    }
    for (index, active_ns) in tick.gpu_active_ns.iter().enumerate() {
        appender.append_row(params![
            tick.observation.publication,
            u32::try_from(index).map_err(|_| Error::Bound("state index"))?,
            active_ns
        ])?;
    }
    Ok(())
}

fn append_cores(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    if !tick.ioreport_read {
        return Ok(());
    }
    for (core, states) in tick.cores.iter().enumerate() {
        for (state, active_ns) in states.iter().enumerate() {
            appender.append_row(params![
                tick.observation.publication,
                u32::try_from(core).map_err(|_| Error::Bound("core index"))?,
                u32::try_from(state).map_err(|_| Error::Bound("state index"))?,
                active_ns
            ])?;
        }
    }
    Ok(())
}

fn append_fans(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    if !tick.smc_read {
        return Ok(());
    }
    for (index, rpm) in tick.fan_rpm.iter().enumerate() {
        appender.append_row(params![
            tick.observation.publication,
            u32::try_from(index).map_err(|_| Error::Bound("fan index"))?,
            rpm
        ])?;
    }
    Ok(())
}

fn append_processes(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    for process in &tick.processes {
        let usage = process.usage;
        let task = process.task;
        appender.append_row(params![
            tick.observation.publication,
            process.pid,
            usage.awake_ns_at_start,
            usage.user_ns,
            usage.system_ns,
            usage.energy_nj,
            usage.instructions,
            usage.super_instructions,
            usage.cycles,
            usage.idle_wakeups,
            usage.interrupt_wakeups,
            usage.pageins,
            usage.resident_bytes,
            usage.footprint_bytes,
            usage.disk_read_bytes,
            usage.disk_written_bytes,
            task.threads,
            task.faults,
            task.context_switches,
            task.mach_syscalls,
            task.unix_syscalls
        ])?;
    }
    Ok(())
}

fn append_coalitions(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    for coalition in &tick.coalitions {
        appender.append_row(params![
            tick.observation.publication,
            coalition.id,
            coalition.tasks_started,
            coalition.tasks_exited,
            coalition.cpu_ns,
            coalition.interrupt_wakeups,
            coalition.idle_wakeups,
            coalition.disk_read_bytes,
            coalition.disk_written_bytes,
            coalition.gpu_ns,
            coalition.energy_nj
        ])?;
    }
    Ok(())
}

fn append_events(appender: &mut duckdb::Appender<'_>, tick: &StagedTick) -> Result<()> {
    for event in &tick.events {
        let usage = event.usage;
        appender.append_row(params![
            tick.observation.publication,
            event.number,
            event.tick,
            event.pid,
            event.kind,
            event.coalition,
            event.availability,
            event.noticed,
            usage.map(|value| value.awake_ns_at_start),
            usage.map(|value| value.user_ns),
            usage.map(|value| value.system_ns),
            usage.map(|value| value.energy_nj),
            usage.map(|value| value.instructions),
            usage.map(|value| value.super_instructions),
            usage.map(|value| value.cycles),
            usage.map(|value| value.idle_wakeups),
            usage.map(|value| value.interrupt_wakeups),
            usage.map(|value| value.pageins),
            usage.map(|value| value.resident_bytes),
            usage.map(|value| value.footprint_bytes),
            usage.map(|value| value.disk_read_bytes),
            usage.map(|value| value.disk_written_bytes)
        ])?;
    }
    Ok(())
}
