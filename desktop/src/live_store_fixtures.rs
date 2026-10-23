//! Seconds the store holds and the trend points the viewer writes for them, which the tests of
//! the charts drawn from the store compare with the charts drawn from the trend points.

use std::sync::Arc;

use collector::machine::Pressure;
use serde_json::{Value, json};
use viewer::store::{AppsSecond, Cohorts, MachineSecond, MemorySecond, Traffic, View};

use crate::live_inventory::{PerformanceLevel, performance_levels};

pub const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const RUN: &str = "00000000000000010000000000000002";
const BOOT: &str = "00000000000000030000000000000004";

/// A second whose readings differ from those of every other second, a quarter of a second
/// into wall second `wall_second`, in segment `segment` of the store's readings.
pub fn reading_at(wall_second: u64, segment: u64) -> MachineSecond {
    let half = |modulus: u64| Some(f64::from(u32::try_from(wall_second % modulus).unwrap()) / 2.0);
    let pressure = match wall_second % 3 {
        0 => Pressure::Normal,
        1 => Pressure::Warning,
        _ => Pressure::Critical,
    };

    MachineSecond {
        publication: wall_second,
        run_tick: wall_second,
        wall_ns: wall_second * NANOSECONDS_PER_SECOND + 250_000_000,
        continuous_ns: Some(wall_second * NANOSECONDS_PER_SECOND),
        awake_ns: Some(wall_second * NANOSECONDS_PER_SECOND),
        follows: true,
        segment,
        cpu_percent: half(200),
        cpu_percent_one_core: half(140).map(|value| value * 18.0),
        cpu_level_percent: [half(190), half(180)],
        gpu_active_percent: half(170),
        memory: Some(MemorySecond {
            total_bytes: 34_359_738_368,
            app_bytes: Some(wall_second * 1_048_576),
            wired_bytes: wall_second * 524_288,
            compressed_bytes: wall_second * 262_144,
            pressure,
            swap_used_bytes: wall_second * 65_536,
            swap_total_bytes: 4_294_967_296,
        }),
        network: Some(Traffic {
            received_bps: f64::from(u32::try_from(wall_second).unwrap()) * 1_000.0,
            sent_bps: f64::from(u32::try_from(wall_second).unwrap()) * 500.0,
        }),
        disk_read_bps: half(160).map(|value| value * 1_000_000.0),
        disk_written_bps: half(150).map(|value| value * 1_000_000.0),
        power_watts: Some(f32::from(u16::try_from(wall_second % 60).unwrap()) / 2.0),
        soc_celsius: Some(40.25),
        cpu_celsius: Some(61.5),
        gpu_celsius: Some(52.25),
        fan_mean_rpm: Some(1_200.5),
        fan_rpm: [Some(1_000.5), Some(1_400.5)],
        battery: None,
        thermal_pressure: Some(u8::try_from(wall_second % 5).unwrap()),
        performance_levels: Some(store_levels()),
    }
}

/// The View of a store that holds `earlier` before the live span `seconds`, whose Apps it has
/// not read.
pub fn view(version: u64, earlier: &[MachineSecond], seconds: &[MachineSecond]) -> View {
    View {
        version,
        seconds: seconds.iter().copied().map(Arc::new).collect(),
        apps: seconds
            .iter()
            .map(|_| Arc::new(AppsSecond::default()))
            .collect(),
        earlier: earlier.iter().copied().map(Arc::new).collect(),
        cohorts: Cohorts::default(),
        failure: None,
        left_out: None,
    }
}

/// The selection that follows the newest of `trends`, its full-detail trend points, with
/// `earlier` as its older tail.
pub fn selection(earlier: &[MachineSecond], trends: &[MachineSecond]) -> Value {
    let newest = trends.last().unwrap();
    let earlier: Vec<Value> = earlier.iter().map(trend_point).collect();
    let trends: Vec<Value> = trends.iter().map(trend_point).collect();
    let last = newest.wall_ns / NANOSECONDS_PER_SECOND;

    json!({"second": last, "context_last_second": last,
        "selected": {"run": RUN, "boot": BOOT, "publication": newest.publication},
        "trends": trends, "earlier_trends": earlier, "earlier_stride_seconds": 10})
}

/// The trend point the viewer writes for `second`.
pub fn trend_point(second: &MachineSecond) -> Value {
    let memory = second.memory.as_ref();
    // The viewer writes anonymous memory with the purgeable part the trend path subtracts.
    let purgeable = second.publication * 131_072;
    let pressure = memory.map(|memory| match memory.pressure {
        Pressure::Normal => 0,
        Pressure::Warning => 1,
        Pressure::Critical => 2,
    });

    json!({"run": RUN, "boot": BOOT, "segment": second.segment, "wall_ns": second.wall_ns,
        "publication": second.publication,
        "cpu_percent": second.cpu_percent,
        "cpu_percent_one_core": second.cpu_percent_one_core,
        "cpu_level_percent": second.cpu_level_percent,
        "gpu_active_percent": second.gpu_active_percent,
        "memory_anonymous_bytes": memory.and_then(|memory| Some(memory.app_bytes? + purgeable)),
        "memory_purgeable_bytes": memory.map(|_| purgeable),
        "memory_wired_bytes": memory.map(|memory| memory.wired_bytes),
        "memory_compressed_bytes": memory.map(|memory| memory.compressed_bytes),
        "memory_total_bytes": memory.map(|memory| memory.total_bytes),
        "swap_used_bytes": memory.map(|memory| memory.swap_used_bytes),
        "swap_total_bytes": memory.map(|memory| memory.swap_total_bytes),
        "pressure": pressure,
        "network_rx_bps": second.network.map(|traffic| traffic.received_bps),
        "network_tx_bps": second.network.map(|traffic| traffic.sent_bps),
        "storage": {"read_bps": second.disk_read_bps, "write_bps": second.disk_written_bps},
        "power_watts": second.power_watts,
        "soc_celsius": second.soc_celsius,
        "cpu_celsius": second.cpu_celsius,
        "gpu_celsius": second.gpu_celsius,
        "fan_rpm": second.fan_rpm,
        "thermal_pressure": second.thermal_pressure,
        "performance_levels": levels_json("Super", "Performance")})
}

/// The performance levels every trend point names.
pub fn levels() -> Option<[PerformanceLevel; 2]> {
    levels_named("Super", "Performance")
}

/// Six logical CPUs of the level named `fastest` and twelve of the level named `slower`, as
/// the desktop reads them from a trend point.
pub fn levels_named(fastest: &str, slower: &str) -> Option<[PerformanceLevel; 2]> {
    performance_levels(&json!({"performance_levels": levels_json(fastest, slower)}))
}

/// The performance levels the store holds for every second, as `levels` names them.
fn store_levels() -> [collector::system::PerformanceLevel; 2] {
    store_levels_named("Super", "Performance")
}

/// The levels of `levels_named`, as the store holds them.
pub fn store_levels_named(fastest: &str, slower: &str) -> [collector::system::PerformanceLevel; 2] {
    let level = |name: &str, logical_cpus| collector::system::PerformanceLevel {
        name: collector::system::FactText::new(name.as_bytes()).unwrap(),
        logical_cpus,
    };

    [level(fastest, 6), level(slower, 12)]
}

fn levels_json(fastest: &str, slower: &str) -> Value {
    json!([{"name": fastest, "logical_cpus": 6}, {"name": slower, "logical_cpus": 12}])
}
