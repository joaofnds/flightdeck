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

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SystemPoint {
    pub load_average: Option<[f64; 3]>,
    pub open_files: Option<u64>,
    pub open_files_limit: Option<u64>,
    pub open_files_percent: Option<f64>,
    pub thermal_pressure: Option<u64>,
    pub gpu_memory_in_use_bytes: Option<u64>,
    pub gpu_memory_allocated_bytes: Option<u64>,
    pub uptime_seconds: Option<u64>,
    pub machine: ComputerPoint,
}

pub(super) fn capture(system: &SystemTick<'_>) -> Result<SystemPoint> {
    let SystemTick {
        ticks,
        tick,
        wall_ns,
    } = *system;
    if !captured(ticks) {
        return Ok(SystemPoint::default());
    }
    let averages = ["load_average_1", "load_average_5", "load_average_15"]
        .map(|name| ticks.number(tick, name));
    let [one, five, fifteen] = averages;
    let load_average = match (one?, five?, fifteen?) {
        (Some(one), Some(five), Some(fifteen)) => Some([one, five, fifteen]),
        _ => None,
    };
    let open_files = ticks.integer(tick, "open_files")?;
    let open_files_limit = ticks.integer(tick, "open_files_limit")?;
    Ok(SystemPoint {
        load_average,
        open_files,
        open_files_limit,
        open_files_percent: open_files_percent(open_files, open_files_limit),
        thermal_pressure: ticks.integer(tick, "thermal_pressure")?,
        gpu_memory_in_use_bytes: ticks.integer(tick, "gpu_memory_in_use_bytes")?,
        gpu_memory_allocated_bytes: ticks.integer(tick, "gpu_memory_allocated_bytes")?,
        uptime_seconds: uptime_seconds(wall_ns, ticks.integer(tick, "boot_wall_ns")?),
        machine: machine(ticks, tick)?,
    })
}

impl SystemPoint {
    pub fn write(&self, value: &mut Value) {
        value["load_average"] = json!(self.load_average);
        value["open_files"] = json!(self.open_files);
        value["open_files_limit"] = json!(self.open_files_limit);
        value["open_files_percent"] = json!(self.open_files_percent);
        value["thermal_pressure"] = json!(self.thermal_pressure);
        value["gpu_memory_in_use_bytes"] = json!(self.gpu_memory_in_use_bytes);
        value["gpu_memory_allocated_bytes"] = json!(self.gpu_memory_allocated_bytes);
        value["uptime_seconds"] = json!(self.uptime_seconds);
        value["machine_model"] = json!(self.machine.model.as_deref());
        value["machine_chip"] = json!(self.machine.chip.as_deref());
        value["machine_architecture"] = json!(self.machine.architecture.as_deref());
        value["machine_cpu_cores"] = json!(self.machine.cpu_cores);
        value["machine_gpu_cores"] = json!(self.machine.gpu_cores);
        value["machine_memory_bytes"] = json!(self.machine.memory_bytes);
        value["performance_levels"] = self.machine.levels.as_ref().map_or(Value::Null, |levels| {
            json!(
                levels
                    .each_ref()
                    .map(|level| json!({"name": level.name.as_str(),
                "logical_cpus": level.logical_cpus}))
            )
        });
    }
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

fn captured(ticks: &View<'_>) -> bool {
    ticks.holds(SYSTEM_COLUMN)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ComputerPoint {
    model: Option<String>,
    chip: Option<String>,
    architecture: Option<String>,
    cpu_cores: Option<u64>,
    gpu_cores: Option<u64>,
    memory_bytes: Option<u64>,
    levels: Option<[ComputerLevel; 2]>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ComputerLevel {
    name: String,
    logical_cpus: u64,
}

impl ComputerPoint {
    pub fn bytes(&self) -> usize {
        self.model.as_ref().map_or(0, String::capacity)
            + self.chip.as_ref().map_or(0, String::capacity)
            + self.architecture.as_ref().map_or(0, String::capacity)
            + self.levels.as_ref().map_or(0, |levels| {
                levels
                    .iter()
                    .map(|level| level.name.capacity())
                    .sum::<usize>()
            })
    }
}

fn machine(ticks: &View<'_>, tick: usize) -> Result<ComputerPoint> {
    let mut levels = std::array::from_fn(|_| ComputerLevel::default());
    let mut complete = true;
    for (level, (name, cpus)) in levels.iter_mut().zip([
        (
            "machine_performance_level0_name",
            "machine_performance_level0_logical_cpus",
        ),
        (
            "machine_performance_level1_name",
            "machine_performance_level1_logical_cpus",
        ),
    ]) {
        match (machine_text(ticks, tick, name)?, ticks.integer(tick, cpus)?) {
            (Some(name), Some(logical_cpus)) => {
                *level = ComputerLevel { name, logical_cpus };
            }
            _ => complete = false,
        }
    }
    Ok(ComputerPoint {
        model: machine_text(ticks, tick, "machine_model")?,
        chip: machine_text(ticks, tick, "machine_chip")?,
        architecture: machine_text(ticks, tick, "machine_architecture")?,
        cpu_cores: ticks.integer(tick, "machine_cpu_cores")?,
        gpu_cores: ticks.integer(tick, "machine_gpu_cores")?,
        memory_bytes: ticks.integer(tick, "machine_memory_bytes")?,
        levels: complete.then_some(levels),
    })
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
