use std::path::Path;

use collector::processes::Usage;
use duckdb::types::{ToSqlOutput, ValueRef};
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
        for (table, _columns, order) in TABLES {
            let name = table.name();
            let columns = table_columns(table, window);
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
            Table::Processes => match (&tick.identities, &window.programs) {
                (Some(identities), Some(programs)) => crate::identity_encoder::append_processes(
                    &mut appender,
                    tick,
                    identities,
                    programs,
                )?,
                (None, None) => append_processes(&mut appender, tick)?,
                _ => unreachable!("consistent window identity mode"),
            },
            Table::Coalitions => append_coalitions(&mut appender, tick)?,
            Table::Events => match (&tick.identities, &window.programs) {
                (Some(identities), Some(programs)) => crate::identity_encoder::append_events(
                    &mut appender,
                    tick,
                    identities,
                    programs,
                )?,
                (None, None) => append_events(&mut appender, tick)?,
                _ => unreachable!("consistent window identity mode"),
            },
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
        let mut row = [ValueRef::Null; 21];
        row[0] = ValueRef::UBigInt(tick.observation.publication);
        row[1] = ValueRef::Int(process.pid);
        usage_fields(&process.usage, &mut row[2..16]);
        row[16] = ValueRef::Int(process.task.threads);
        row[17] = ValueRef::Int(process.task.faults);
        row[18] = ValueRef::Int(process.task.context_switches);
        row[19] = ValueRef::Int(process.task.mach_syscalls);
        row[20] = ValueRef::Int(process.task.unix_syscalls);

        appender.append_row(duckdb::appender_params_from_iter(
            row.iter().copied().map(ToSqlOutput::Borrowed),
        ))?;
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
        let mut row = [ValueRef::Null; 22];
        row[0] = ValueRef::UBigInt(tick.observation.publication);
        row[1] = ValueRef::UBigInt(event.number);
        row[2] = ValueRef::UBigInt(event.tick);
        row[3] = ValueRef::Int(event.pid);
        row[4] = ValueRef::UTinyInt(event.kind);
        row[5] = event.coalition.map_or(ValueRef::Null, ValueRef::UBigInt);
        row[6] = ValueRef::UTinyInt(event.availability);
        row[7] = event.noticed.map_or(ValueRef::Null, ValueRef::UTinyInt);
        if let Some(usage) = &event.usage {
            usage_fields(usage, &mut row[8..22]);
        }

        appender.append_row(duckdb::appender_params_from_iter(
            row.iter().copied().map(ToSqlOutput::Borrowed),
        ))?;
    }

    Ok(())
}

pub(crate) fn usage_fields(usage: &Usage, fields: &mut [ValueRef<'_>]) {
    assert_eq!(fields.len(), 14);
    fields[0] = ValueRef::UBigInt(usage.awake_ns_at_start);
    fields[1] = ValueRef::UBigInt(usage.user_ns);
    fields[2] = ValueRef::UBigInt(usage.system_ns);
    fields[3] = ValueRef::UBigInt(usage.energy_nj);
    fields[4] = ValueRef::UBigInt(usage.instructions);
    fields[5] = ValueRef::UBigInt(usage.super_instructions);
    fields[6] = ValueRef::UBigInt(usage.cycles);
    fields[7] = ValueRef::UBigInt(usage.idle_wakeups);
    fields[8] = ValueRef::UBigInt(usage.interrupt_wakeups);
    fields[9] = ValueRef::UBigInt(usage.pageins);
    fields[10] = ValueRef::UBigInt(usage.resident_bytes);
    fields[11] = ValueRef::UBigInt(usage.footprint_bytes);
    fields[12] = ValueRef::UBigInt(usage.disk_read_bytes);
    fields[13] = ValueRef::UBigInt(usage.disk_written_bytes);
}

/// The complete encoder-owned definition for this window's optional extensions.
#[must_use]
pub fn table_columns(table: Table, window: &Window) -> String {
    let Some((_, columns, _)) = TABLES.iter().find(|entry| entry.0.name() == table.name()) else {
        unreachable!("every table has a schema");
    };

    let mut columns = (*columns).to_owned();
    if window.programs.is_some() {
        if matches!(table, Table::Processes | Table::Events) {
            for (name, kind) in crate::schema::IDENTITY_FIELDS {
                columns.push_str(", ");
                columns.push_str(name);
                columns.push(' ');
                columns.push_str(kind);
            }
        }
    }

    columns
}
