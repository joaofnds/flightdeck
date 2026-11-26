//! Seconds the store holds and the trend points the viewer writes for them, which the tests of
//! the charts drawn from the store compare with the charts drawn from the trend points.

use std::sync::Arc;

use collector::battery::BatteryReading;
use collector::machine::Pressure;
use collector::recording::identity_text;
use collector::ring::Identity;
use num_traits::ToPrimitive;
use serde_json::{Value, json};
use viewer::store::{
    AppsSecond, MachineSecond, MemoryRate, MemorySecond, NetworkRate, StorageRate, Traffic, View,
};

use crate::live_inventory::{PerformanceLevel, performance_levels};

pub const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

const RUN: &str = "00000000000000010000000000000002";
const BOOT: &str = "00000000000000030000000000000004";

/// The Overview's machine charts of `view`, as the page draws them from the store's View.
/// `levels` names the performance levels the CPU chart draws.
pub fn overview_panels(
    view: &View,
    levels: Option<&[PerformanceLevel; 2]>,
) -> Result<Vec<std::rc::Rc<crate::data::Panel>>, String> {
    crate::live_projection::view_panels("overview", view, levels)
}

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
        boot: [3, 4],
        run: [1, 2],
        publication: wall_second,
        run_tick: wall_second,
        wall_ns: wall_second * NANOSECONDS_PER_SECOND + 250_000_000,
        continuous_ns: Some(wall_second * NANOSECONDS_PER_SECOND),
        awake_ns: Some(wall_second * NANOSECONDS_PER_SECOND),
        elapsed_ns: Some(1_000_000_000),
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
        complete: viewer::store::CompleteMachineState::Unread,
        cpu_refused: None,
    }
}

/// The View of a store that holds `earlier` before the live span `seconds`, whose Apps it is
/// still loading, though it names each second, as the store names every second it takes in.
pub fn view(version: u64, earlier: &[MachineSecond], seconds: &[MachineSecond]) -> View {
    View {
        version,
        seconds: seconds.iter().cloned().map(Arc::new).collect(),
        apps: seconds
            .iter()
            .map(|second| {
                Arc::new(AppsSecond {
                    observation: Some(view_observation(second)),
                    ..AppsSecond::default()
                })
            })
            .collect(),
        earlier: earlier.iter().cloned().map(Arc::new).collect(),
        stretches: Vec::new(),
        sampled: Vec::new(),
        apps_loaded: false,
        failure: None,
        left_out: None,
        ring_complete: false,
        full_detail_evicted_through_ns: None,
    }
}

/// The identity the store names `second` by among the Apps of its View.
fn view_observation(second: &MachineSecond) -> viewer::live_app_response::Observation {
    viewer::live_app_response::Observation {
        recording: [5, 6],
        stamp: viewer::apps::Stamp {
            boot: second.boot,
            run: second.run,
            publication: second.publication,
            continuous_ns: second.continuous_ns,
            awake_ns: second.awake_ns,
        },
        run_tick: second.run_tick,
        wall_ns: Some(second.wall_ns),
    }
}

/// The selection that follows the newest of `trends`, its full-detail trend points, with
/// `earlier` as its older tail.
pub fn selection(earlier: &[MachineSecond], trends: &[MachineSecond]) -> Value {
    selection_of_points(earlier, trends, trend_point)
}

/// The selection of `selection` whose trend points are what the complete reading of each second
/// summarizes, as the viewer writes the point of a second it holds such a reading for.
pub fn summary_selection(earlier: &[MachineSecond], trends: &[MachineSecond]) -> Value {
    selection_of_points(earlier, trends, summary_point)
}

/// The selection of `selection` whose trend point of each second `point` writes.
fn selection_of_points(
    earlier: &[MachineSecond],
    trends: &[MachineSecond],
    point: impl Fn(&MachineSecond) -> Value,
) -> Value {
    let newest = trends.last().unwrap();
    let earlier: Vec<Value> = earlier.iter().map(&point).collect();
    let trends: Vec<Value> = trends.iter().map(&point).collect();
    let last = newest.wall_ns / NANOSECONDS_PER_SECOND;
    // The newest point names the run it was read in, which the selection follows.
    let selected = trends.last().unwrap();

    json!({"second": last, "context_last_second": last,
        "selected": {"run": selected["run"], "boot": selected["boot"],
            "publication": newest.publication},
        "trends": trends, "earlier_trends": earlier, "earlier_stride_seconds": 10})
}

/// The selection of `value`, a fixture's selection JSON, split by the conversion a saved
/// selection takes as it enters a snapshot.
pub fn selection_of_json(value: &Value) -> viewer::Selection {
    viewer::Selection::from_json(value.clone())
}

/// `second` with a complete reading of the memory it holds and of memory's rates, as the store
/// holds a second it read. Each rate differs from the other rates and from the rates of every
/// other second.
pub fn with_memory_reading(second: MachineSecond) -> MachineSecond {
    let rates = memory_rates(&second);
    with_memory_rates(second, rates)
}

/// The rates `with_memory_reading` gives `second`, in the order of `MemoryRate::ALL`.
pub fn memory_rates(second: &MachineSecond) -> [Option<f64>; MemoryRate::ALL.len()] {
    let wall_second = f64::from(u32::try_from(second.wall_ns / NANOSECONDS_PER_SECOND).unwrap());

    [
        Some(wall_second + 0.25),
        Some(wall_second * 2.0 + 0.5),
        Some(wall_second * 3.0 + 0.75),
        Some(wall_second * 4_096.0),
        Some(wall_second * 8_192.0),
    ]
}

/// `second` with a complete reading of the memory it holds and of memory's rates `rates`.
pub fn with_memory_rates(
    mut second: MachineSecond,
    rates: [Option<f64>; MemoryRate::ALL.len()],
) -> MachineSecond {
    let budget = Arc::new(viewer::store::ByteBudget::new(1_048_576));
    let complete = viewer::store::CompleteMachine::of_memory(&second, rates, &budget);
    second.complete = viewer::store::CompleteMachineState::Available(complete);
    second
}

/// `second` with a complete reading of the traffic it holds and of the network's packet and
/// error rates, as the store holds a second it read. Each rate differs from the other rates and
/// from the rates of every other second.
pub fn with_network_reading(second: MachineSecond) -> MachineSecond {
    let rates = network_rates(&second);
    with_network_rates(second, rates)
}

/// The rates `with_network_reading` gives `second`, in the order of `NetworkRate::ALL`.
pub fn network_rates(second: &MachineSecond) -> [Option<f64>; NetworkRate::ALL.len()] {
    let wall_second = f64::from(u32::try_from(second.wall_ns / NANOSECONDS_PER_SECOND).unwrap());

    [
        Some(wall_second * 3.0 + 0.25),
        Some(wall_second * 2.0 + 0.5),
        Some(wall_second / 4.0 + 0.75),
        Some(wall_second / 8.0),
    ]
}

/// `second` with a complete reading of the traffic it holds and of the network's packet and
/// error rates `rates`.
pub fn with_network_rates(
    mut second: MachineSecond,
    rates: [Option<f64>; NetworkRate::ALL.len()],
) -> MachineSecond {
    let budget = Arc::new(viewer::store::ByteBudget::new(1_048_576));
    let complete = viewer::store::CompleteMachine::of_network(&second, rates, &budget);
    second.complete = viewer::store::CompleteMachineState::Available(complete);
    second
}

/// `second` with a complete reading of the bytes its disks read and wrote and of the disks'
/// operation, error and mean service time rates, as the store holds a second it read. Each rate
/// differs from the other rates and from the rates of every other second.
pub fn with_storage_reading(second: MachineSecond) -> MachineSecond {
    let rates = storage_rates(&second);
    with_storage_rates(second, rates)
}

/// The rates `with_storage_reading` gives `second`, in the order of `StorageRate::ALL`.
pub fn storage_rates(second: &MachineSecond) -> [Option<f64>; StorageRate::ALL.len()] {
    let wall_second = f64::from(u32::try_from(second.wall_ns / NANOSECONDS_PER_SECOND).unwrap());

    [
        Some(wall_second * 5.0 + 0.25),
        Some(wall_second * 3.0 + 0.5),
        Some(wall_second / 2.0 + 0.75),
        Some(wall_second / 16.0),
        Some(wall_second / 100.0 + 0.125),
        Some(wall_second / 50.0 + 0.375),
    ]
}

/// `second` with a complete reading of the bytes its disks read and wrote and of the disks'
/// rates `rates`.
pub fn with_storage_rates(
    mut second: MachineSecond,
    rates: [Option<f64>; StorageRate::ALL.len()],
) -> MachineSecond {
    let budget = Arc::new(viewer::store::ByteBudget::new(1_048_576));
    let complete = viewer::store::CompleteMachine::of_storage(&second, rates, &budget);
    second.complete = viewer::store::CompleteMachineState::Available(complete);
    second
}

/// `second` with the GPU sensor `gpu_celsius` gives it, and a complete reading of the GPU
/// activity and sensor it then holds and of the GPU's power and memory, as the store holds a
/// second it read. Each reading differs from those of every other second.
pub fn with_gpu_reading(mut second: MachineSecond) -> MachineSecond {
    second.gpu_celsius = Some(gpu_celsius(&second));
    let watts = Some(gpu_watts(&second));
    let memory_bytes = gpu_memory_bytes(&second);

    with_gpu_readings(second, watts, memory_bytes)
}

/// The GPU sensor `with_gpu_reading` gives `second`, a quarter degree apart from one second
/// to the next over 160 seconds.
fn gpu_celsius(second: &MachineSecond) -> f32 {
    let quarter_degrees = u16::try_from(second.wall_ns / NANOSECONDS_PER_SECOND % 160).unwrap();

    f32::from(quarter_degrees) / 4.0 + 30.0
}

/// The GPU power `with_gpu_reading` gives `second`.
pub fn gpu_watts(second: &MachineSecond) -> f64 {
    let wall_second = f64::from(u32::try_from(second.wall_ns / NANOSECONDS_PER_SECOND).unwrap());

    wall_second / 10.0 + 0.25
}

/// The GPU memory in use and allocated `with_gpu_reading` gives `second`, in that order.
pub fn gpu_memory_bytes(second: &MachineSecond) -> [Option<u64>; 2] {
    let wall_second = second.wall_ns / NANOSECONDS_PER_SECOND;

    [Some(wall_second * 2_097_152), Some(wall_second * 3_145_728)]
}

/// `second` with a complete reading of the GPU activity and sensor it holds, the GPU power
/// `watts`, and the GPU memory `memory_bytes`, in use and then allocated.
pub fn with_gpu_readings(
    mut second: MachineSecond,
    watts: Option<f64>,
    memory_bytes: [Option<u64>; 2],
) -> MachineSecond {
    let budget = Arc::new(viewer::store::ByteBudget::new(1_048_576));
    let complete = viewer::store::CompleteMachine::of_gpu(&second, watts, memory_bytes, &budget);
    second.complete = viewer::store::CompleteMachineState::Available(complete);
    second
}

/// `second` with a complete reading of the power, `soc_celsius` and thermal state it holds, the GPU
/// power `with_gpu_reading` gives it and the three fan channels `fan_rpm` gives it, as the store
/// holds a second it read. Its GPU power and fans differ from those of every other second, while
/// its power and `soc_celsius` are those `reading_at` gives it, which repeat.
pub fn with_energy_reading(second: MachineSecond) -> MachineSecond {
    let watts = Some(gpu_watts(&second));
    let fans = fan_rpm(&second);

    with_energy_readings(second, watts, fans)
}

/// The three fan channels `with_energy_reading` gives `second`, past the two its record holds.
pub fn fan_rpm(second: &MachineSecond) -> Vec<Option<f64>> {
    let wall_second = f64::from(u32::try_from(second.wall_ns / NANOSECONDS_PER_SECOND).unwrap());

    vec![
        Some(wall_second + 1_000.5),
        Some(wall_second + 1_400.5),
        Some(wall_second + 1_800.5),
    ]
}

/// `second` with a complete reading of the power, `soc_celsius` and thermal state it holds, the GPU
/// power `gpu_watts` and the fan channels `fan_rpm`, the first two of which its record then holds.
pub fn with_energy_readings(
    mut second: MachineSecond,
    gpu_watts: Option<f64>,
    fan_rpm: Vec<Option<f64>>,
) -> MachineSecond {
    let channel = |index: usize| {
        fan_rpm
            .get(index)
            .copied()
            .flatten()
            .and_then(|rpm| rpm.to_f32())
    };
    second.fan_rpm = [channel(0), channel(1)];
    let budget = Arc::new(viewer::store::ByteBudget::new(1_048_576));
    let complete = viewer::store::CompleteMachine::of_energy(&second, gpu_watts, fan_rpm, &budget);
    second.complete = viewer::store::CompleteMachineState::Available(complete);
    second
}

/// `second` with the battery `battery_reading` gives it, in its record and in a complete reading
/// of it, as the store holds a second it read.
pub fn with_battery_reading(second: MachineSecond) -> MachineSecond {
    let battery = battery_reading(&second);

    with_battery_readings(second, Some(battery))
}

/// What the battery `battery_reading` gives holds when full, in the controller's units, which
/// its charge at a second, the second's wall clock, is a share of.
pub const BATTERY_CHARGE_MAXIMUM: u16 = 20_000;

/// The battery `with_battery_reading` gives `second`, fresh at the second. Its charge and power
/// differ from those of every other second, and it charges, drawing power in, past 9,000.
pub fn battery_reading(second: &MachineSecond) -> BatteryReading {
    let wall_second = second.wall_ns / NANOSECONDS_PER_SECOND;
    let current_ma = i64::try_from(wall_second).unwrap() - 9_000;

    BatteryReading {
        source_continuous_ns: second.continuous_ns.unwrap(),
        charge_current: Some(wall_second),
        charge_maximum: Some(u64::from(BATTERY_CHARGE_MAXIMUM)),
        full_charge_capacity: Some(4_500),
        design_capacity: Some(5_000),
        cycle_count: Some(321),
        charging: Some(current_ma > 0),
        external_power: Some(current_ma > 0),
        voltage_mv: Some(12_000),
        current_ma: Some(current_ma),
        adapter_watts: Some(96),
    }
}

/// `second` with `battery`, or no battery, in its record and in a complete reading of it, as the
/// store holds a second it read.
pub fn with_battery_readings(
    mut second: MachineSecond,
    battery: Option<BatteryReading>,
) -> MachineSecond {
    second.battery = battery
        .as_ref()
        .map(viewer::store::BatterySecond::of_fresh_reading);
    let budget = Arc::new(viewer::store::ByteBudget::new(1_048_576));
    let complete = viewer::store::CompleteMachine::of_battery(&second, battery, &budget);
    second.complete = viewer::store::CompleteMachineState::Available(complete);
    second
}

/// The most files `cpu_readings` lets the system open, as macOS's limit reads on a Mac.
const OPEN_FILES_LIMIT: u64 = 245_760;

/// The readings of a second's complete reading the CPU page's machine charts draw past those its
/// record holds.
#[derive(Clone, Copy, Debug)]
pub struct CpuReadings {
    /// Each performance level's clock while active, level 0, the fastest, first.
    pub level_ghz: [Option<f64>; 2],
    pub load_average: Option<[f64; 3]>,
    pub open_files: Option<u64>,
    pub open_files_percent: Option<f64>,
}

/// `second` with the CPU sensor `cpu_celsius` gives it, and a complete reading of the CPU
/// activity and sensor it then holds and of the readings `cpu_readings` gives it, as the store
/// holds a second it read. Each reading differs from those of every other second.
pub fn with_cpu_reading(mut second: MachineSecond) -> MachineSecond {
    second.cpu_celsius = Some(cpu_celsius(&second));
    let readings = cpu_readings(&second);

    with_cpu_readings(second, readings)
}

/// The CPU sensor `with_cpu_reading` gives `second`, a quarter degree apart from one second
/// to the next over 160 seconds.
fn cpu_celsius(second: &MachineSecond) -> f32 {
    let quarter_degrees = u16::try_from(second.wall_ns / NANOSECONDS_PER_SECOND % 160).unwrap();

    f32::from(quarter_degrees) / 4.0 + 50.0
}

/// The clocks, load averages and open files `with_cpu_reading` gives `second`, the share of the
/// limit as the collector's reading derives it from the open files.
pub fn cpu_readings(second: &MachineSecond) -> CpuReadings {
    let wall_second = second.wall_ns / NANOSECONDS_PER_SECOND;
    let hundredths =
        |modulus: u64| f64::from(u32::try_from(wall_second % modulus).unwrap()) / 100.0;
    let open_files = wall_second % 7_000 + 20_000;

    CpuReadings {
        level_ghz: [Some(hundredths(130) + 3.0), Some(hundredths(120) + 2.0)],
        load_average: Some([
            hundredths(110) * 10.0 + 1.0,
            hundredths(100) * 10.0 + 1.5,
            hundredths(90) * 10.0 + 2.0,
        ]),
        open_files: Some(open_files),
        open_files_percent: Some(open_files_percent(open_files)),
    }
}

/// The share of `OPEN_FILES_LIMIT` that `open_files` take.
pub fn open_files_percent(open_files: u64) -> f64 {
    let open = f64::from(u32::try_from(open_files).unwrap());
    let limit = f64::from(u32::try_from(OPEN_FILES_LIMIT).unwrap());

    open / limit * 100.0
}

/// `second` with a complete reading of the CPU activity and sensor it holds and of `readings`.
pub fn with_cpu_readings(mut second: MachineSecond, readings: CpuReadings) -> MachineSecond {
    let budget = Arc::new(viewer::store::ByteBudget::new(1_048_576));
    let complete = viewer::store::CompleteMachine::of_cpu(
        &second,
        readings.level_ghz,
        readings.load_average,
        readings.open_files,
        readings.open_files_percent,
        &budget,
    );
    second.complete = viewer::store::CompleteMachineState::Available(complete);
    second
}

/// The trend point the viewer writes for `second` from its complete reading.
pub fn summary_point(second: &MachineSecond) -> Value {
    let identity = |identity| identity_text(Identity(identity)).to_string();
    let mut point = second.complete().unwrap().summary(second);
    point["run"] = json!(identity(second.run));
    point["boot"] = json!(identity(second.boot));
    point
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
        "battery": second.battery.as_ref().map(|battery| json!({
            "charge_percent": battery.charge_percent, "power_watts": battery.power_watts})),
        "thermal_pressure": second.thermal_pressure,
        "logical_cpu_count": second.cpu_counters().map(<[_]>::len),
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
