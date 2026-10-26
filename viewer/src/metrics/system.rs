//! The Mac's system readings and the machine's facts as the collector recorded them in a tick.

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::metric_view::View;

/// The column that shows a source holds the system group, which comes whole or not at all.
const SYSTEM_COLUMN: &str = "load_average_1";

/// The system readings of a tick at its wall clock time.
pub(super) struct SystemTick<'tick> {
    pub ticks: &'tick View<'tick>,
    pub tick: usize,
    pub wall_ns: u64,
}

/// Adds to `value` the system readings of a tick and the machine's facts it carries. A
/// reading the collector did not get reads null, and so does every reading of a source that
/// holds no system group.
pub(super) fn point(system: &SystemTick<'_>, value: &mut Value) -> Result<()> {
    let SystemTick {
        ticks,
        tick,
        wall_ns,
    } = *system;
    if !captured(ticks) {
        for name in FIELDS {
            value[name] = Value::Null;
        }
        return Ok(());
    }

    let averages = ["load_average_1", "load_average_5", "load_average_15"]
        .map(|name| ticks.number(tick, name));
    let [one, five, fifteen] = averages;
    value["load_average"] = match (one?, five?, fifteen?) {
        (Some(one), Some(five), Some(fifteen)) => json!([one, five, fifteen]),
        _ => Value::Null,
    };

    let open_files = ticks.integer(tick, "open_files")?;
    let open_files_limit = ticks.integer(tick, "open_files_limit")?;
    value["open_files"] = json!(open_files);
    value["open_files_limit"] = json!(open_files_limit);
    value["open_files_percent"] = json!(open_files_percent(open_files, open_files_limit));
    value["thermal_pressure"] = json!(ticks.integer(tick, "thermal_pressure")?);
    for name in ["gpu_memory_in_use_bytes", "gpu_memory_allocated_bytes"] {
        value[name] = json!(ticks.integer(tick, name)?);
    }
    value["uptime_seconds"] = json!(uptime_seconds(
        wall_ns,
        ticks.integer(tick, "boot_wall_ns")?
    ));
    machine(ticks, tick, value)
}

/// The machine's two performance levels as `tick` recorded them, or none where the collector
/// read no two levels.
pub(crate) fn levels(
    ticks: &View<'_>,
    tick: usize,
) -> Result<Option<collector::machine::PerformanceLevels>> {
    if !captured(ticks) {
        return Ok(None);
    }

    let fastest = ticks.integer(tick, "machine_performance_level0_logical_cpus")?;
    let slower = ticks.integer(tick, "machine_performance_level1_logical_cpus")?;
    let (Some(fastest), Some(slower)) = (fastest, slower) else {
        return Ok(None);
    };
    let logical_cpus = [fastest, slower]
        .map(|count| u32::try_from(count).map_err(|_| Error::Bound("level logical CPUs")));
    let [fastest, slower] = logical_cpus;
    Ok(Some(collector::machine::PerformanceLevels {
        logical_cpus: [fastest?, slower?],
    }))
}

/// The point fields `point` writes.
const FIELDS: [&str; 15] = [
    "load_average",
    "open_files",
    "open_files_limit",
    "open_files_percent",
    "thermal_pressure",
    "gpu_memory_in_use_bytes",
    "gpu_memory_allocated_bytes",
    "uptime_seconds",
    "machine_model",
    "machine_chip",
    "machine_architecture",
    "machine_cpu_cores",
    "machine_gpu_cores",
    "machine_memory_bytes",
    "performance_levels",
];

fn captured(ticks: &View<'_>) -> bool {
    ticks.holds(SYSTEM_COLUMN)
}

fn machine(ticks: &View<'_>, tick: usize, value: &mut Value) -> Result<()> {
    for name in ["machine_model", "machine_chip", "machine_architecture"] {
        value[name] = json!(machine_text(ticks, tick, name)?);
    }
    for name in [
        "machine_cpu_cores",
        "machine_gpu_cores",
        "machine_memory_bytes",
    ] {
        value[name] = json!(ticks.integer(tick, name)?);
    }

    let mut levels = Vec::with_capacity(2);
    for level in 0..2 {
        let name = machine_text(
            ticks,
            tick,
            &format!("machine_performance_level{level}_name"),
        )?;
        let cpus = format!("machine_performance_level{level}_logical_cpus");
        let logical_cpus = ticks.integer(tick, &cpus)?;
        if let (Some(name), Some(logical_cpus)) = (name, logical_cpus) {
            levels.push(json!({"name": name, "logical_cpus": logical_cpus}));
        }
    }
    value["performance_levels"] = if levels.len() == 2 {
        json!(levels)
    } else {
        Value::Null
    };
    Ok(())
}

fn machine_text(ticks: &View<'_>, tick: usize, name: &str) -> Result<Option<String>> {
    Ok(ticks.field(tick, name)?.map(str::to_owned))
}

/// The share of the limit in use, or none where macOS reported no limit.
fn open_files_percent(count: Option<u64>, limit: Option<u64>) -> Option<f64> {
    let (Some(count), Some(limit)) = (count, limit) else {
        return None;
    };
    if limit == 0 {
        return None;
    }
    Some(super::number(count) / super::number(limit) * 100.0)
}

/// The whole seconds since boot at `wall_ns`, or none where the boot time is unknown or later.
fn uptime_seconds(wall_ns: u64, boot_wall_ns: Option<u64>) -> Option<u64> {
    let second = wall_ns / 1_000_000_000;
    let boot = boot_wall_ns? / 1_000_000_000;
    second.checked_sub(boot)
}
