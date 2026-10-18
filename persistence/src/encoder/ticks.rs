use polars::prelude::*;

use crate::encoder::columns;
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
