use super::*;
use crate::recording::{CpuRow, FanRow, GpuStateRow, InterfaceRow, MemoryRow, TickRow};
use collector::interfaces::Interface;
use collector::machine::{CPUTicks, Memory};

mod app_machine_tests;
mod battery_tests;
mod storage_tests;
mod system_tests;

macro_rules! unsigned_number_case {
    ($name:ident, $value:expr, $expected:expr) => {
        #[test]
        fn $name() {
            // Goal: unsigned counter conversion retains the floating-point range at word edges.
            // Method: compare a boundary input with its independently specified f64 result.
            let expected: f64 = $expected;

            assert_eq!(number($value).to_bits(), expected.to_bits());
        }
    };
}

unsigned_number_case!(converts_unsigned_zero, 0, 0.0);
unsigned_number_case!(
    converts_unsigned_low_word_limit,
    4_294_967_295,
    4_294_967_295.0
);
unsigned_number_case!(
    converts_unsigned_high_word_start,
    4_294_967_296,
    4_294_967_296.0
);
unsigned_number_case!(
    converts_unsigned_counter_limit,
    u64::MAX,
    18_446_744_073_709_551_616.0
);

struct Fixture {
    minutes: BTreeMap<u64, MachineMinute>,
    previous: Observation,
    current: Observation,
}

impl Fixture {
    fn new() -> Self {
        Self {
            minutes: BTreeMap::from([(1, minute(1, false)), (2, minute(2, true))]),
            previous: Observation {
                window: 0,
                publication: 1,
                run_tick: 1,
                wall: 1_000_000_000_000,
                continuous: Some(1_000_000_000),
            },
            current: Observation {
                window: 0,
                publication: 2,
                run_tick: 2,
                wall: 1_001_000_000_000,
                continuous: Some(3_000_000_000),
            },
        }
    }

    fn point(&self) -> Value {
        self.point_after(Some(&self.previous)).unwrap()
    }

    /// The current observation's point, its rates over the interval since `previous`, if any.
    fn point_after(&self, previous: Option<&Observation>) -> Result<Value> {
        self.point_of(&self.current, previous)
    }

    fn point_of(&self, current: &Observation, previous: Option<&Observation>) -> Result<Value> {
        let reading = self.reading(current.publication);
        let before = previous.map(|_| self.reading(current.publication - 1));

        point_sources(
            &Sources {
                current: &reading,
                previous: before.as_ref(),
            },
            current,
            previous,
            1,
        )
    }

    fn reading(&self, publication: u64) -> Reading<'_> {
        reading_at(&self.minutes[&publication], publication).unwrap()
    }

    fn minute(&mut self, publication: u64) -> &mut MachineMinute {
        self.minutes.get_mut(&publication).unwrap()
    }
}

/// One publication's readings. Only the counters of the current tick moved since the earlier.
fn minute(publication: u64, current: bool) -> MachineMinute {
    MachineMinute {
        ticks: vec![tick(publication, current)],
        cpus: vec![cpu_row(publication, 0, 30, 70)],
        memory: vec![MemoryRow {
            publication,
            memory: memory(current),
        }],
        interfaces: vec![interface(publication, current)],
        gpu_states: gpu_states(
            publication,
            if current {
                &[1_500_000_000, 2_300_000_000]
            } else {
                &[1_000_000_000, 2_000_000_000]
            },
        ),
        core_states: Vec::new(),
        fans: Vec::new(),
    }
}

fn tick(publication: u64, current: bool) -> TickRow {
    TickRow {
        gpu_energy_nj: Some(if current {
            5_000_000_000
        } else {
            1_000_000_000
        }),
        power_watts: Some(0.0),
        soc_celsius: Some(45.0),
        cpu_celsius: Some(if current { 61.5 } else { 60.5 }),
        gpu_celsius: current.then_some(52.25),
        ..TickRow::fixture(publication)
    }
}

fn cpu_row(publication: u64, cpu: u32, user: u32, idle: u32) -> CpuRow {
    CpuRow {
        publication,
        cpu,
        ticks: CPUTicks {
            user,
            system: 0,
            nice: 0,
            idle,
        },
    }
}

fn memory(current: bool) -> Memory {
    Memory {
        total_bytes: 9_007_199_254_740_993,
        free_bytes: 2,
        active_bytes: 3,
        inactive_bytes: 4,
        speculative_bytes: 5,
        wired_bytes: 6,
        compressed_bytes: 7,
        anonymous_bytes: 8,
        file_backed_bytes: 9,
        purgeable_bytes: 10,
        swap_total_bytes: 11,
        swap_used_bytes: 12,
        swapped_in_bytes: if current { 1124 } else { 100 },
        swapped_out_bytes: if current { 2248 } else { 200 },
        pageins: if current { 7 } else { 1 },
        pageouts: 2,
        compressions: if current { 11 } else { 3 },
        decompressions: if current { 6 } else { 4 },
        ..Memory::default()
    }
}

/// The current interface's counters, from `received_bytes` through `receive_drops`.
const INTERFACE_CURRENT: [u64; 7] = [3048, 3024, 18, 20, 3, 6, 9];
const INTERFACE_EARLIER: [u64; 7] = [1000, 2000, 10, 20, 1, 2, 3];

fn interface(publication: u64, current: bool) -> InterfaceRow {
    let counters = if current {
        INTERFACE_CURRENT
    } else {
        INTERFACE_EARLIER
    };

    InterfaceRow::fixture(publication, 7, "en0", counters)
}

fn gpu_states(publication: u64, active_ns: &[u64]) -> Vec<GpuStateRow> {
    (0_u32..)
        .zip(active_ns)
        .map(|(state, active_ns)| GpuStateRow {
            publication,
            state,
            active_ns: *active_ns,
        })
        .collect()
}

fn fan_row(fan: u32, rpm: f32) -> FanRow {
    FanRow {
        publication: 2,
        fan,
        rpm,
    }
}

#[test]
fn derives_independent_network_packet_and_error_rates() {
    // Goal: packet and error counters use the measured interval and retain measured zero.
    // Method: adjacent observations contain distinct deltas over two continuous seconds.
    let value = Fixture::new().point();

    assert_eq!(value["network_rx_packets_per_second"], 4.0);
    assert_eq!(value["network_tx_packets_per_second"], 0.0);
    assert_eq!(value["network_rx_errors_per_second"], 1.0);
    assert_eq!(value["network_tx_errors_per_second"], 2.0);
}

fn network_rates(value: &Value) -> Value {
    json!([
        value["network_rx_packets_per_second"],
        value["network_tx_packets_per_second"],
        value["network_rx_errors_per_second"],
        value["network_tx_errors_per_second"]
    ])
}

macro_rules! unavailable_network_counter {
    ($name:ident, $reset:expr, $expected:tt) => {
        #[test]
        fn $name() {
            // Goal: one unavailable counter does not erase another measured rate.
            // Method: reset one current counter and compare all four outputs.
            let mut fixture = Fixture::new();
            $reset(&mut fixture.minute(2).interfaces[0].interface);

            let value = fixture.point();

            assert_eq!(network_rates(&value), json!($expected));
            assert_eq!(value["network_rx_bps"], 1024.0);
            assert_eq!(value["network_tx_bps"], 512.0);
        }
    };
}

unavailable_network_counter!(
    preserves_other_network_rates_when_received_packets_reset,
    |interface: &mut Interface| interface.received_packets = 0,
    [null, 0.0, 1.0, 2.0]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_sent_packets_reset,
    |interface: &mut Interface| interface.sent_packets = 0,
    [4.0, null, 1.0, 2.0]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_receive_errors_reset,
    |interface: &mut Interface| interface.receive_errors = 0,
    [4.0, 0.0, null, 2.0]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_send_errors_reset,
    |interface: &mut Interface| interface.send_errors = 0,
    [4.0, 0.0, 1.0, null]
);

macro_rules! changed_network_identity {
    ($name:ident, $index:literal, $interface_name:literal) => {
        #[test]
        fn $name() {
            // Goal: reused interface indices or renamed interfaces cannot authorize rates.
            // Method: change one part of the current interface key, keeping the counters.
            let mut fixture = Fixture::new();
            fixture.minute(2).interfaces[0] =
                InterfaceRow::fixture(2, $index, $interface_name, INTERFACE_CURRENT);

            assert_eq!(
                network_rates(&fixture.point()),
                json!([null, null, null, null])
            );
        }
    };
}

changed_network_identity!(
    leaves_network_rates_unavailable_after_interface_renaming,
    7,
    "utun0"
);
changed_network_identity!(leaves_network_rates_unavailable_after_index_reuse, 8, "en0");

#[test]
fn leaves_network_rates_unavailable_after_interface_membership_changes() {
    // Goal: changing the interface set cannot make a partial aggregate look complete.
    // Method: remove the current interface while retaining the earlier measurement.
    let mut fixture = Fixture::new();
    fixture.minute(2).interfaces.clear();

    assert_eq!(
        network_rates(&fixture.point()),
        json!([null, null, null, null])
    );
}

#[test]
fn sums_network_counters_by_interface_identity_instead_of_row_order() {
    // Goal: multiple external interfaces contribute once despite a changed row order.
    // Method: add a second interface with different counters before the current first row.
    let mut fixture = Fixture::new();
    fixture
        .minute(1)
        .interfaces
        .push(InterfaceRow::fixture(1, 8, "utun0", [0, 0, 2, 3, 4, 5, 0]));
    fixture.minute(2).interfaces.insert(
        0,
        InterfaceRow::fixture(2, 8, "utun0", [0, 0, 8, 13, 12, 17, 0]),
    );

    assert_eq!(network_rates(&fixture.point()), json!([7.0, 5.0, 5.0, 8.0]));
}

#[test]
fn measures_resource_projection_for_900_counter_intervals() {
    // Goal: measure the same bounded projection and serialization before and after expansion.
    // Method: project 900 identical real counter intervals and serialize their public output.
    let fixture = Fixture::new();
    let started = std::time::Instant::now();
    let points: Vec<_> = (0..900)
        .map(|_| public_point(&fixture.point()).unwrap())
        .collect();
    let projected = started.elapsed().as_nanos();
    let bytes = serde_json::to_vec(&points).unwrap();

    assert_eq!(points.len(), 900);
    assert_eq!(points[899]["network_rx_bps"], 1024.0);
    assert!(bytes.len() <= 8 * 1024 * 1024);
    println!(
        "{}",
        json!({"intervals": points.len(), "projection_ns": projected,
            "projection_and_serialize_ns": started.elapsed().as_nanos(),
            "response_bytes": bytes.len()})
    );
}

#[test]
fn derives_resource_scalars_with_the_recorded_continuous_interval() {
    // Goal: physical source units produce exact scalar rates without synthetic labels.
    // Method: adjacent wall seconds carry an explicit two-second continuous interval.
    let value = Fixture::new().point();

    assert_eq!(value["gpu_active_percent"], 40.0);
    assert_eq!(value["gpu_watts"], 2.0);
    assert_eq!(value["memory_total_bytes"], 9_007_199_254_740_993_u64);
    assert_eq!(value["memory_free_bytes"], 2);
    assert_eq!(value["memory_inactive_bytes"], 4);
    assert_eq!(value["memory_speculative_bytes"], 5);
    assert_eq!(value["memory_anonymous_bytes"], 8);
    assert_eq!(value["memory_file_backed_bytes"], 9);
    assert_eq!(value["memory_purgeable_bytes"], 10);
    assert_eq!(value["swap_total_bytes"], 11);
    assert_eq!(value["memory_pageins_per_second"], 3.0);
    assert_eq!(value["memory_pageouts_per_second"], 0.0);
    assert_eq!(value["memory_compressions_per_second"], 4.0);
    assert_eq!(value["memory_decompressions_per_second"], 1.0);
    assert_eq!(value["swap_in_bytes_per_second"], 512.0);
    assert_eq!(value["swap_out_bytes_per_second"], 1024.0);
}

#[test]
fn leaves_counter_rates_unavailable_across_wall_gaps() {
    // Goal: elapsed clocks never authorize interpolation across a missed wall second.
    // Method: preserve counters but move the later wall timestamp across a two-second gap.
    let mut fixture = Fixture::new();
    fixture.current.wall += 1_000_000_000;

    let value = fixture.point();

    assert!(value["gpu_active_percent"].is_null());
    assert!(value["gpu_watts"].is_null());
    assert!(value["memory_pageins_per_second"].is_null());
    assert!(value["network_rx_bps"].is_null());
    assert_eq!(network_rates(&value), json!([null, null, null, null]));
    assert_eq!(value["memory_free_bytes"], 2);
}

#[test]
fn resumes_gpu_rates_only_after_two_successful_readings_follow_a_source_failure() {
    // Goal: the first recovered counter is a baseline, even with continuous tick identities.
    // Method: project four adjacent observations whose optional sources fail only on the second.
    let fixture = optional_failure_fixture();
    let observations: [Observation; 4] = std::array::from_fn(|index| {
        let publication = u64::try_from(index).unwrap() + 1;
        Observation {
            window: 0,
            publication,
            run_tick: publication,
            wall: (999 + publication) * 1_000_000_000,
            continuous: Some((publication * 2 - 1) * 1_000_000_000),
        }
    });

    let points: [Value; 4] = std::array::from_fn(|index| {
        fixture
            .point_of(
                &observations[index],
                index.checked_sub(1).map(|index| &observations[index]),
            )
            .unwrap()
    });

    assert_eq!(
        points.each_ref().map(|point| &point["gpu_watts"]),
        [&Value::Null, &Value::Null, &Value::Null, &json!(1.0)]
    );
    assert_eq!(
        points.each_ref().map(|point| &point["gpu_active_percent"]),
        [&Value::Null, &Value::Null, &Value::Null, &json!(0.0)]
    );
    assert_eq!(
        points.each_ref().map(|point| &point["power_watts"]),
        [&json!(0.0), &Value::Null, &json!(0.0), &json!(0.0)]
    );
    assert!(points.iter().all(|point| point["memory_free_bytes"] == 2));
}

fn optional_failure_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.minutes.insert(3, minute(3, true));
    fixture.minutes.insert(4, minute(4, true));
    let failed = fixture.minute(2);
    failed.gpu_states.clear();
    let tick = &mut failed.ticks[0];
    tick.ioreport_read = false;
    tick.smc_read = false;
    tick.gpu_energy_nj = None;
    tick.power_watts = None;
    tick.soc_celsius = None;
    tick.cpu_celsius = None;
    tick.gpu_celsius = None;
    fixture.minute(4).ticks[0].gpu_energy_nj = Some(7_000_000_000);
    fixture
}

#[test]
fn rejects_gpu_residency_with_a_refused_previous_sample() {
    // Goal: refusal cannot become measured GPU work or power.
    // Method: keep numeric counters but mark the earlier IOReport read unsuccessful.
    let mut fixture = Fixture::new();
    fixture.minute(1).ticks[0].ioreport_read = false;

    let value = fixture.point();

    assert!(value["gpu_active_percent"].is_null());
    assert!(value["gpu_watts"].is_null());
}

#[test]
fn rejects_gpu_residency_with_a_changed_state_count() {
    // Goal: a changing state domain cannot produce an aggregate residency rate.
    // Method: give only the earlier reading a third state.
    let mut fixture = Fixture::new();
    fixture.minute(1).gpu_states = gpu_states(1, &[1_000_000_000, 2_000_000_000, 0]);

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_with_no_states() {
    // Goal: no active-state domain means unavailable rather than measured idle.
    // Method: give neither endpoint a state row.
    let mut fixture = Fixture::new();
    fixture.minute(1).gpu_states.clear();
    fixture.minute(2).gpu_states.clear();

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_with_a_missing_state_row() {
    // Goal: partial residency is never reported as a complete GPU percentage.
    // Method: remove one of two rows from the current endpoint.
    let mut fixture = Fixture::new();
    fixture.minute(2).gpu_states.pop().unwrap();

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_with_duplicate_state_rows() {
    // Goal: duplicated rows cannot disguise a missing state at the same row count.
    // Method: give the two current rows the same state index.
    let mut fixture = Fixture::new();
    fixture.minute(2).gpu_states[1].state = 0;

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_with_an_out_of_range_state() {
    // Goal: a matching row count cannot substitute a different state domain.
    // Method: replace state 1 with state 2 in a two-state sample.
    let mut fixture = Fixture::new();
    fixture.minute(2).gpu_states[1].state = 2;

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_after_a_counter_reset() {
    // Goal: a reset is unavailable, never a negative or wrapped activity reading.
    // Method: move one current state counter below its previous value.
    let mut fixture = Fixture::new();
    fixture.minute(2).gpu_states[0].active_ns = 0;

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn preserves_measured_idle_and_never_clamps_gpu_activity() {
    // Goal: measured zero differs from missing data, and source counters are not clamped.
    // Method: reuse the earlier counters, then increase one state beyond a full interval.
    let mut fixture = Fixture::new();
    fixture.minute(2).gpu_states = gpu_states(2, &[1_000_000_000, 2_000_000_000]);
    assert_eq!(fixture.point()["gpu_active_percent"], 0.0);

    fixture.minute(2).gpu_states[0].active_ns = 4_000_000_000;
    assert_eq!(fixture.point()["gpu_active_percent"], 150.0);
}

#[test]
fn preserves_independent_memory_and_network_counter_availability() {
    // Goal: one reset cannot create a false rate or erase other measured rates.
    // Method: reset one VM counter and one network counter independently.
    let mut fixture = Fixture::new();
    fixture.minute(2).memory[0].memory.compressions = 0;
    fixture.minute(2).interfaces[0].interface.received_bytes = 0;

    let value = fixture.point();

    assert!(value["memory_compressions_per_second"].is_null());
    assert!(value["network_rx_bps"].is_null());
    assert_eq!(value["memory_pageins_per_second"], 3.0);
    assert_eq!(value["network_tx_bps"], 512.0);
}

#[test]
fn reports_the_recorded_cpu_and_gpu_temperatures() {
    // Goal: a point carries the temperature of each die beside the chip's, as recorded.
    // Method: project a tick that recorded both, then one whose GPU sensor gave no reading.
    let mut fixture = Fixture::new();
    let both = fixture.point();
    fixture.minute(2).ticks[0].gpu_celsius = None;
    let cpu_only = fixture.point();

    assert_eq!(both["cpu_celsius"], 61.5);
    assert_eq!(both["gpu_celsius"], 52.25);
    assert_eq!(cpu_only["cpu_celsius"], 61.5);
    assert!(cpu_only["gpu_celsius"].is_null());
}

const POINT_FIELDS: [&str; 62] = [
    "battery",
    "boot",
    "continuous_ns",
    "cpu_celsius",
    "cpu_level_ghz",
    "cpu_level_percent",
    "cpu_percent",
    "cpu_percent_one_core",
    "fan_rpm",
    "gpu_active_percent",
    "gpu_celsius",
    "gpu_memory_allocated_bytes",
    "gpu_memory_in_use_bytes",
    "gpu_watts",
    "interval_unavailable",
    "load_average",
    "logical_cpu_count",
    "machine_architecture",
    "machine_chip",
    "machine_cpu_cores",
    "machine_gpu_cores",
    "machine_memory_bytes",
    "machine_model",
    "memory_active_bytes",
    "memory_anonymous_bytes",
    "memory_compressed_bytes",
    "memory_compressions_per_second",
    "memory_decompressions_per_second",
    "memory_file_backed_bytes",
    "memory_free_bytes",
    "memory_inactive_bytes",
    "memory_pageins_per_second",
    "memory_pageouts_per_second",
    "memory_purgeable_bytes",
    "memory_speculative_bytes",
    "memory_total_bytes",
    "memory_wired_bytes",
    "network_rx_bps",
    "network_rx_errors_per_second",
    "network_rx_packets_per_second",
    "network_tx_bps",
    "network_tx_errors_per_second",
    "network_tx_packets_per_second",
    "open_files",
    "open_files_limit",
    "open_files_percent",
    "performance_levels",
    "power_watts",
    "pressure",
    "publication",
    "run",
    "run_tick",
    "segment",
    "soc_celsius",
    "storage",
    "swap_in_bytes_per_second",
    "swap_out_bytes_per_second",
    "swap_total_bytes",
    "swap_used_bytes",
    "thermal_pressure",
    "uptime_seconds",
    "wall_ns",
];

fn widest_point() -> Value {
    let mut point = Fixture::new().point();
    point["cpu_percent"] = json!(0.000_012_345_678_901_234_568);
    for value in point.as_object_mut().unwrap().values_mut() {
        if value.is_number() {
            *value = if value.is_f64() {
                json!(0.000_012_345_678_901_234_568)
            } else {
                json!(u64::MAX)
            };
        }
    }
    point["fan_rpm"] = json!(vec![0.000_012_345_678_901_234_568; 10]);
    point["cpu_level_percent"] = json!(vec![0.000_012_345_678_901_234_568; 2]);
    point["cpu_level_ghz"] = point["cpu_level_percent"].clone();
    widest_point_system(&mut point);
    point["battery"] = json!({"source_continuous_ns": u64::MAX,
        "age_seconds": 0.000_012_345_678_901_234_568,
        "charge_percent": 0.000_012_345_678_901_234_568,
        "health_percent": 0.000_012_345_678_901_234_568,
        "cycle_count": u64::MAX, "charging": false, "external_power": false,
        "power_watts": -0.000_012_345_678_901_234_568, "adapter_watts": u64::MAX});
    assert_eq!(point["cpu_percent"].to_string().len(), 23);
    point["power_watts"] = json!(-0.000_012_345_678_901_234_568);
    assert_eq!(point["power_watts"].to_string().len(), 24);
    point["soc_celsius"] = point["power_watts"].clone();
    point["run"] = json!("ffffffffffffffffffffffffffffffff");
    point["boot"] = point["run"].clone();
    point["observation"] = json!({"run": point["run"], "boot": point["boot"],
        "publication": u64::MAX, "run_tick": u64::MAX, "wall_ns": u64::MAX,
        "window_losses": null, "reader_losses": {"publications": u64::MAX,
            "events": u64::MAX, "unknown": false}});
    point
}

/// Fills the system readings and the machine's facts at their widest, which the fixture's
/// ticks, holding no system group, leave null.
fn widest_point_system(point: &mut Value) {
    let fraction = 0.000_012_345_678_901_234_568;
    let text = "x".repeat(collector::system::FACT_BYTES_MAX);
    point["load_average"] = json!(vec![fraction; 3]);
    point["open_files_percent"] = json!(fraction);
    for name in [
        "open_files",
        "open_files_limit",
        "thermal_pressure",
        "gpu_memory_in_use_bytes",
        "gpu_memory_allocated_bytes",
        "uptime_seconds",
        "machine_cpu_cores",
        "machine_gpu_cores",
        "machine_memory_bytes",
    ] {
        point[name] = json!(u64::MAX);
    }
    for name in ["machine_model", "machine_chip", "machine_architecture"] {
        point[name] = json!(text);
    }
    let level = json!({"name": text, "logical_cpus": u64::MAX});
    point["performance_levels"] = json!([level, level]);
}

fn selection_envelope(point: &Value, identity: &Value, count: usize) -> Value {
    let response = json!({"second": u64::MAX, "selected": identity,
        "alternatives": vec![identity.clone(); if count == 900 { 1 } else { count }],
        "summary": point, "trends": vec![point.clone(); count], "context_seconds": 900});
    json!({"value": response, "error": null})
}

#[test]
fn measures_scalar_selection_response_sizes_at_both_context_limits() {
    // Goal: widest scalar envelopes expose both the supported and refused byte-bound cases.
    // Method: retain every field and ten widest fan gauges at both context count limits.
    let internal = widest_point();
    let point = public_point(&internal).unwrap();
    let fields = point.as_object().unwrap();
    assert_eq!(
        fields.keys().map(String::as_str).collect::<Vec<_>>(),
        POINT_FIELDS
    );
    assert_eq!(fields.values().filter(|value| value.is_f64()).count(), 20);
    assert_eq!(fields.values().filter(|value| value.is_u64()).count(), 28);
    assert_eq!(u64::MAX.to_string().len(), 20);
    let identity = &internal["observation"];
    let mut saved_identity = identity.clone();
    let losses = saved_identity
        .as_object_mut()
        .unwrap()
        .remove("reader_losses")
        .unwrap();
    saved_identity["window_losses"] = losses;
    assert!(saved_identity.to_string().len() < identity.to_string().len());
    assert!(internal["observation"].is_object());
    assert!(point.get("observation").is_none());
    for (name, value) in point.as_object().unwrap() {
        assert_eq!(internal[name], *value);
    }
    for count in [900, OBSERVATIONS_MAX] {
        let start = std::time::Instant::now();
        let before_bytes = serde_json::to_vec(&selection_envelope(&internal, identity, count))
            .unwrap()
            .len();
        let bytes = serde_json::to_vec(&selection_envelope(&point, identity, count))
            .unwrap()
            .len();
        let saved_bytes = serde_json::to_vec(&selection_envelope(&point, &saved_identity, count))
            .unwrap()
            .len();
        println!(
            "{}",
            json!({"observations": count, "response_bytes": bytes,
            "saved_response_bytes": saved_bytes, "before_response_bytes": before_bytes,
            "removed_bytes": before_bytes - bytes,
            "headroom_bytes": (8_usize * 1024 * 1024).checked_sub(bytes),
            "serialize_ns": start.elapsed().as_nanos(), "bridge_bytes_max": 8 * 1024 * 1024,
            "point_fields": point.as_object().unwrap().len(),
            "reader_loss_object_bytes": identity["reader_losses"].to_string().len(),
            "window_loss_object_bytes": saved_identity["window_losses"].to_string().len(),
            "live_alternative_bytes": identity.to_string().len(),
            "saved_alternative_bytes": saved_identity.to_string().len()})
        );
        assert_eq!(bytes <= 8 * 1024 * 1024, count == 900);
        assert_eq!(saved_bytes <= 8 * 1024 * 1024, count == 900);
        assert!(saved_bytes <= bytes);
    }
}

#[test]
fn preserves_measured_empty_interface_domains() {
    // Goal: an empty measured interface table cannot turn zero into unavailable.
    // Method: project two readings that hold no interface row.
    let mut fixture = Fixture::new();
    fixture.minute(1).interfaces.clear();
    fixture.minute(2).interfaces.clear();

    let point = fixture.point();

    assert_eq!(point["network_rx_bps"], 0.0);
    assert_eq!(network_rates(&point), json!([0.0, 0.0, 0.0, 0.0]));
}

#[test]
fn preserves_zero_gpu_energy_and_refuses_energy_resets() {
    // Goal: measured zero power remains distinct from a reset energy counter.
    // Method: set equal energy endpoints, then make the later energy smaller.
    let mut fixture = Fixture::new();
    fixture.minute(2).ticks[0].gpu_energy_nj = Some(1_000_000_000);
    assert_eq!(fixture.point()["gpu_watts"], 0.0);

    fixture.minute(2).ticks[0].gpu_energy_nj = Some(0);
    assert!(fixture.point()["gpu_watts"].is_null());
    assert_eq!(fixture.point()["gpu_active_percent"], 40.0);
}

#[test]
fn leaves_rates_unavailable_without_a_compatible_predecessor() {
    // Goal: no source counter interval is fabricated at startup or a run-tick gap.
    // Method: omit the predecessor, then skip a run tick with numeric rows unchanged.
    let mut fixture = Fixture::new();
    let first = fixture.point_after(None).unwrap();
    assert!(first["gpu_active_percent"].is_null());
    assert!(first["memory_pageins_per_second"].is_null());
    assert_eq!(network_rates(&first), json!([null, null, null, null]));

    fixture.current.run_tick += 1;
    assert!(fixture.point()["gpu_active_percent"].is_null());
    assert!(fixture.point()["network_rx_bps"].is_null());
    assert_eq!(
        network_rates(&fixture.point()),
        json!([null, null, null, null])
    );
}

#[test]
fn preserves_measured_network_zeroes_without_traffic() {
    // Goal: unchanged measured counters are zero, rather than absent observations.
    // Method: keep the same interface and counters at both endpoints of a valid interval.
    let mut fixture = Fixture::new();
    fixture.minute(2).interfaces = vec![interface(2, false)];

    assert_eq!(network_rates(&fixture.point()), json!([0.0, 0.0, 0.0, 0.0]));
}

macro_rules! malformed_common_fan {
    ($name:ident, $change:expr, $reason:literal) => {
        #[test]
        fn $name() {
            // Goal: common scalar projection refuses malformed fan associations.
            // Method: corrupt one fact of a measured, complete two-channel source.
            let mut fixture = Fixture::new();
            fixture.minute(2).fans = vec![fan_row(0, 12.0), fan_row(1, 0.0)];
            $change(&mut fixture.minute(2).fans);

            let result = fixture.point_after(None);

            assert!(matches!(result, Err(Error::Bound($reason))), "{result:?}");
        }
    };
}
malformed_common_fan!(
    refuses_duplicate_fan_channels_in_common_projection,
    |fans: &mut Vec<FanRow>| fans[1].fan = 0,
    "fan channel order"
);
malformed_common_fan!(
    refuses_too_many_fan_channels_in_common_projection,
    |fans: &mut Vec<FanRow>| fans.resize(11, fan_row(0, 0.0)),
    "fan channels"
);

#[test]
fn retains_an_unread_fan_channel_in_common_projection() {
    // Goal: a channel with no reading remains a gap beside stopped zero in common projection.
    // Method: project a channel the collector read as NaN through the same point builder.
    let mut fixture = Fixture::new();
    fixture.minute(2).fans = vec![fan_row(0, f32::NAN), fan_row(1, 0.0)];

    assert_eq!(fixture.point()["fan_rpm"], json!([null, 0.0]));
}

#[test]
fn preserves_machine_rate_multiplication_before_division() {
    // Goal: sharing exact deltas cannot change the machine's public floating-point rate.
    // Method: one byte over 1,000,000,001 ns distinguishes the two operation orders.
    let expected: f64 = 0.999_999_999;

    assert_eq!(
        per_second(Some(1), 1_000_000_001).unwrap().to_bits(),
        expected.to_bits()
    );
}

/// A publication's interfaces, each its index and its received bytes.
fn received_minute(publication: u64, interfaces: &[(u16, u64)]) -> MachineMinute {
    MachineMinute {
        ticks: vec![TickRow::fixture(publication)],
        interfaces: interfaces
            .iter()
            .map(|(index, received_bytes)| {
                InterfaceRow::fixture(
                    publication,
                    *index,
                    "en0",
                    [*received_bytes, 0, 0, 0, 0, 0, 0],
                )
            })
            .collect(),
        ..MachineMinute::default()
    }
}

#[test]
fn retains_decimal_component_order_before_a_later_reset() {
    // Goal: typed pairing retains the saved reduction's overflow before a later reset.
    // Method: keys 1, 10, 2 overflow in decimal order but reset first in numeric order.
    let current = received_minute(2, &[(1, u64::MAX), (2, 0), (10, 1)]);
    let previous = received_minute(1, &[(1, 0), (2, 1), (10, 0)]);
    let current = reading_at(&current, 2).unwrap();
    let previous = reading_at(&previous, 1).unwrap();

    let result = counter_delta(
        &View(crate::live_raw::View::machine(&current, "interfaces").unwrap()),
        &View(crate::live_raw::View::machine(&previous, "interfaces").unwrap()),
        &["interface"],
        "received_bytes",
    );

    assert!(matches!(result, Err(Error::Bound("counter sum"))));
    let integer = crate::metric_view::Key::Integer(10);
    assert!(integer < crate::metric_view::Key::Integer(2));
    assert!(integer == crate::metric_view::Key::Text("10"));
}
