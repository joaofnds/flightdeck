use polars::prelude::*;

use crate::encoder::columns;
use crate::schema::SYSTEM_FIELDS;
use crate::window::StagedTick;
use crate::{Error, Result, Window};

pub(super) fn frame(window: &Window) -> Result<DataFrame> {
    let rows = &window.ticks;
    let mut columns = columns!(rows;
        "publication" => |tick| tick.observation.publication,
        "run_tick" => |tick| tick.run_tick,
        "wall_ns" => |tick| tick.observation.wall_ns,
        "continuous_ns" => |tick| tick.observation.continuous_ns,
        "awake_ns" => |tick| tick.observation.awake_ns,
        "ioreport_read" => |tick| tick.ioreport_read,
        "smc_read" => |tick| tick.smc_read,
        "gpu_energy_nj" => |tick| tick.ioreport_read.then_some(tick.gpu_energy_nj),
        "power_watts" => |tick| tick.smc_read.then_some(tick.power_watts),
        "soc_celsius" => |tick| tick.smc_read.then_some(tick.soc_celsius),
        "processes_refused" => |tick| tick.processes_refused,
        "processes_gone" => |tick| tick.processes_gone,
        "events_first" => |tick| tick.events_first,
        "events_end" => |tick| tick.events_end,
        "dropped_starts" => |tick| tick.dropped.starts,
        "dropped_execs" => |tick| tick.dropped.execs,
        "dropped_exits" => |tick| tick.dropped.exits,
    );
    let states = rows
        .iter()
        .map(|tick| frame_count(tick.ioreport_read, tick.gpu_active_ns.len()))
        .collect::<Result<Vec<_>>>()?;
    let cores = rows
        .iter()
        .map(|tick| frame_count(tick.ioreport_read, tick.cores.len()))
        .collect::<Result<Vec<_>>>()?;
    columns.push(Column::new("gpu_state_count".into(), states));
    columns.push(Column::new("core_count".into(), cores));
    if window.battery_captured() {
        columns.extend(frame_battery(window));
    }
    if window.disks_captured() {
        let disks = rows
            .iter()
            .map(|tick| tick.disks.as_ref().map(crate::disks::encode).transpose())
            .collect::<Result<Vec<_>>>()?;
        columns.push(Column::new("physical_disks".into(), disks));
    }

    if window.temperatures_captured() {
        columns.extend(columns!(rows;
            "cpu_celsius" => |tick| tick.cpu_celsius,
            "gpu_celsius" => |tick| tick.gpu_celsius,
        ));
    }

    if window.system_captured() {
        columns.extend(frame_system(window));
    }

    Ok(DataFrame::new(rows.len(), columns)?)
}

fn frame_count(captured: bool, count: usize) -> Result<u32> {
    if captured {
        u32::try_from(count).map_err(|_| Error::Bound("machine state count"))
    } else {
        Ok(0)
    }
}

fn frame_battery(window: &Window) -> Vec<Column> {
    let rows = &window.ticks;
    columns!(rows;
        "battery_version" => |_| crate::schema::BATTERY_VERSION,
        "battery_source_continuous_ns" => |tick| {
            tick.battery.map(|value| value.source_continuous_ns)
        },
        "battery_charge_current" => |tick| tick.battery.and_then(|value| value.charge_current),
        "battery_charge_maximum" => |tick| tick.battery.and_then(|value| value.charge_maximum),
        "battery_full_charge_capacity" => |tick| {
            tick.battery.and_then(|value| value.full_charge_capacity)
        },
        "battery_design_capacity" => |tick| tick.battery.and_then(|value| value.design_capacity),
        "battery_cycle_count" => |tick| tick.battery.and_then(|value| value.cycle_count),
        "battery_charging" => |tick| tick.battery.and_then(|value| value.charging),
        "battery_external_power" => |tick| tick.battery.and_then(|value| value.external_power),
        "battery_voltage_mv" => |tick| tick.battery.and_then(|value| value.voltage_mv),
        "battery_current_ma" => |tick| tick.battery.and_then(|value| value.current_ma),
        "battery_adapter_watts" => |tick| tick.battery.and_then(|value| value.adapter_watts),
    )
}

/// The system columns in the order of `SYSTEM_FIELDS`, NULL in a tick that published none.
fn frame_system(window: &Window) -> Vec<Column> {
    let rows = &window.ticks;
    let columns = columns!(rows;
        "load_average_1" => |tick| load_average(tick, 0),
        "load_average_5" => |tick| load_average(tick, 1),
        "load_average_15" => |tick| load_average(tick, 2),
        "open_files" => |tick| reading(tick).and_then(|value| value.open_files),
        "open_files_limit" => |tick| reading(tick).and_then(|value| value.open_files_limit),
        "thermal_pressure" => |tick| reading(tick).and_then(|value| value.thermal_pressure),
        "gpu_memory_in_use_bytes" => |tick| {
            reading(tick).and_then(|value| value.gpu_memory?.in_use_bytes)
        },
        "gpu_memory_allocated_bytes" => |tick| {
            reading(tick).and_then(|value| value.gpu_memory?.allocated_bytes)
        },
        "boot_wall_ns" => |tick| reading(tick).and_then(|value| value.boot_wall_ns),
        "machine_model" => |tick| text(machine(tick).and_then(|value| value.model.as_ref())),
        "machine_chip" => |tick| text(machine(tick).and_then(|value| value.chip.as_ref())),
        "machine_architecture" => |tick| {
            text(machine(tick).and_then(|value| value.architecture.as_ref()))
        },
        "machine_cpu_cores" => |tick| machine(tick).and_then(|value| value.cpu_cores),
        "machine_gpu_cores" => |tick| machine(tick).and_then(|value| value.gpu_cores),
        "machine_memory_bytes" => |tick| machine(tick).and_then(|value| value.memory_bytes),
        "machine_performance_level0_name" => |tick| text(level(tick, 0).map(|value| &value.name)),
        "machine_performance_level0_logical_cpus" => |tick| {
            level(tick, 0).map(|value| value.logical_cpus)
        },
        "machine_performance_level1_name" => |tick| text(level(tick, 1).map(|value| &value.name)),
        "machine_performance_level1_logical_cpus" => |tick| {
            level(tick, 1).map(|value| value.logical_cpus)
        },
    );

    assert_eq!(columns.len(), SYSTEM_FIELDS.len());
    for (column, (name, _kind)) in columns.iter().zip(SYSTEM_FIELDS) {
        assert_eq!(column.name().as_str(), name);
    }
    columns
}

fn reading(tick: &StagedTick) -> Option<&collector::system::SystemReading> {
    tick.system.as_ref().map(|system| &system.reading)
}

fn machine(tick: &StagedTick) -> Option<&collector::system::MachineFacts> {
    tick.system.as_ref().map(|system| &system.machine)
}

fn load_average(tick: &StagedTick, index: usize) -> Option<f64> {
    reading(tick)?.load_average.map(|averages| averages[index])
}

fn level(tick: &StagedTick, index: usize) -> Option<&collector::system::PerformanceLevel> {
    machine(tick)?.levels.as_ref().map(|levels| &levels[index])
}

fn text(fact: Option<&collector::system::FactText>) -> Option<&str> {
    fact.map(collector::system::FactText::as_str)
}
