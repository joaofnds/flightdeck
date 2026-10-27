//! Reduced machine values shared by the held Store and legacy JSON presentation.
use persistence::Result;
use serde_json::{Value, json};

use super::{MEMORY_RATES, TRAFFIC_RATES, battery, storage, system};
use crate::session::Observation;

pub(super) const MEMORY_FIELDS: [(&str, &str); 13] = [
    ("memory_total_bytes", "total_bytes"),
    ("memory_free_bytes", "free_bytes"),
    ("memory_active_bytes", "active_bytes"),
    ("memory_inactive_bytes", "inactive_bytes"),
    ("memory_speculative_bytes", "speculative_bytes"),
    ("memory_wired_bytes", "wired_bytes"),
    ("memory_compressed_bytes", "compressed_bytes"),
    ("memory_anonymous_bytes", "anonymous_bytes"),
    ("memory_file_backed_bytes", "file_backed_bytes"),
    ("memory_purgeable_bytes", "purgeable_bytes"),
    ("swap_total_bytes", "swap_total_bytes"),
    ("swap_used_bytes", "swap_used_bytes"),
    ("pressure", "pressure"),
];

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Point {
    pub cpu_percent: Option<f64>,
    pub cpu_percent_one_core: Option<f64>,
    pub cpu_level_percent: Option<[Option<f64>; 2]>,
    pub cpu_level_ghz: Option<[Option<f64>; 2]>,
    pub logical_cpu_count: Option<u32>,
    pub gpu_active_percent: Option<f64>,
    pub gpu_watts: Option<f64>,
    pub power_watts: Option<f64>,
    pub soc_celsius: Option<f64>,
    pub cpu_celsius: Option<f64>,
    pub gpu_celsius: Option<f64>,
    pub fan_rpm: Option<Vec<Option<f64>>>,
    pub memory: [Option<u64>; 13],
    /// Memory's six rates followed by the six external network rates.
    pub rates: [Option<f64>; 12],
    pub battery: Option<battery::BatteryPoint>,
    pub storage: Option<storage::StoragePoint>,
    pub system: system::SystemPoint,
}

impl Point {
    pub fn json(&self, current: &Observation, segment: u64) -> Value {
        let mut value = json!({"wall_ns": current.wall, "publication": current.publication,
            "run_tick": current.run_tick, "continuous_ns": current.continuous,
            "segment": segment, "cpu_percent": self.cpu_percent,
            "cpu_percent_one_core": self.cpu_percent_one_core,
            "cpu_level_percent": self.cpu_level_percent, "cpu_level_ghz": self.cpu_level_ghz,
            "logical_cpu_count": self.logical_cpu_count,
            "gpu_active_percent": self.gpu_active_percent, "gpu_watts": self.gpu_watts,
            "power_watts": self.power_watts, "soc_celsius": self.soc_celsius,
            "cpu_celsius": self.cpu_celsius, "gpu_celsius": self.gpu_celsius,
            "fan_rpm": self.fan_rpm,
            "interval_unavailable": "Requires consecutive compatible counter samples"});
        value["battery"] = self
            .battery
            .as_ref()
            .map_or(Value::Null, battery::BatteryPoint::json);
        value["storage"] = self
            .storage
            .as_ref()
            .map_or(Value::Null, storage::StoragePoint::json);
        self.system.write(&mut value);
        for ((name, _column), amount) in MEMORY_FIELDS.iter().zip(self.memory) {
            value[*name] = json!(amount);
        }
        for ((name, _column), rate) in MEMORY_RATES
            .into_iter()
            .chain(TRAFFIC_RATES)
            .zip(self.rates)
        {
            value[name] = json!(rate);
        }
        value
    }

    pub fn bytes(&self) -> usize {
        self.fan_rpm
            .as_ref()
            .map_or(0, |fans| fans.capacity() * size_of::<Option<f64>>())
            + self.system.machine.bytes()
            + self
                .storage
                .as_ref()
                .map_or(0, storage::StoragePoint::bytes)
    }
}

pub(crate) fn capture(
    reading: &crate::store::Reading<'_>,
    previous: Option<&crate::store::Reading<'_>>,
    cpu_interval: &mut Option<crate::cpu_history_sample::CapturedInterval>,
) -> Result<Point> {
    crate::live_raw::validate_machine_tick(reading.tick)?;
    let previous = previous.filter(|reading| reading.tick.wall_ns.is_some());
    let current = observation(reading);
    let before = previous.map(observation);
    super::capture_sources(
        &crate::metric_view::Sources::Machine {
            current: reading,
            previous,
        },
        &current,
        before.as_ref(),
        Some(cpu_interval),
    )
}

pub(crate) fn observation(reading: &crate::store::Reading<'_>) -> Observation {
    Observation {
        window: 0,
        publication: reading.tick.publication,
        run_tick: reading.tick.run_tick,
        wall: reading
            .tick
            .wall_ns
            .unwrap_or_else(|| unreachable!("placed machine reading")),
        continuous: reading.tick.continuous_ns,
    }
}
