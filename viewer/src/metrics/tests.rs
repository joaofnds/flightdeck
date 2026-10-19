use super::*;

mod app_machine_tests;
mod battery_tests;
mod storage_tests;

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
    tables: Tables,
    previous: Observation,
    current: Observation,
}

impl Fixture {
    fn new() -> Self {
        let mut tables = Tables::new();
        let fans = table(&["publication", "fan", "rpm"], &[]);
        tables.insert((0, "fans"), partition(&fans).unwrap());
        for publication in 1..=2 {
            let current = publication == 2;
            tables
                .entry((0, "ticks"))
                .or_default()
                .insert(publication, ticks(current));
            tables
                .entry((0, "memory"))
                .or_default()
                .insert(publication, memory(current));
            tables.entry((0, "cpus")).or_default().insert(
                publication,
                table(
                    &[
                        "publication",
                        "cpu",
                        "user_ticks",
                        "system_ticks",
                        "nice_ticks",
                        "idle_ticks",
                    ],
                    &[&[if current { "2" } else { "1" }, "0", "10", "20", "0", "70"]],
                ),
            );
            tables
                .entry((0, "interfaces"))
                .or_default()
                .insert(publication, interfaces(current));
            tables.entry((0, "gpu_states")).or_default().insert(
                publication,
                table(
                    &["state", "active_ns"],
                    if current {
                        &[&["0", "1500000000"], &["1", "2300000000"]]
                    } else {
                        &[&["0", "1000000000"], &["1", "2000000000"]]
                    },
                ),
            );
        }
        Self {
            tables,
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
        point(&self.tables, &self.current, Some(&self.previous), 1).unwrap()
    }

    fn row(&mut self, name: &'static str, publication: u64) -> &mut Table {
        self.tables
            .get_mut(&(0, name))
            .unwrap()
            .get_mut(&publication)
            .unwrap()
    }
}

fn table(columns: &[&str], rows: &[&[&str]]) -> Table {
    Table {
        columns: columns.iter().map(|value| (*value).to_owned()).collect(),
        rows: rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|value| {
                        if *value == "NULL" {
                            None
                        } else {
                            Some((*value).to_owned())
                        }
                    })
                    .collect()
            })
            .collect(),
    }
}

fn ticks(current: bool) -> Table {
    table(
        &[
            "gpu_energy_nj",
            "power_watts",
            "soc_celsius",
            "ioreport_read",
            "gpu_state_count",
            "publication",
            "smc_read",
        ],
        &[if current {
            &["5000000000", "0", "45", "true", "2", "2", "true"]
        } else {
            &["1000000000", "0", "45", "true", "2", "1", "true"]
        }],
    )
}

const MEMORY_COLUMNS: [&str; 19] = [
    "total_bytes",
    "free_bytes",
    "active_bytes",
    "inactive_bytes",
    "speculative_bytes",
    "wired_bytes",
    "compressed_bytes",
    "anonymous_bytes",
    "file_backed_bytes",
    "purgeable_bytes",
    "swap_total_bytes",
    "swap_used_bytes",
    "swapped_in_bytes",
    "swapped_out_bytes",
    "pageins",
    "pageouts",
    "compressions",
    "decompressions",
    "pressure",
];

fn memory(current: bool) -> Table {
    table(
        &MEMORY_COLUMNS,
        &[if current {
            &[
                "9007199254740993",
                "2",
                "3",
                "4",
                "5",
                "6",
                "7",
                "8",
                "9",
                "10",
                "11",
                "12",
                "1124",
                "2248",
                "7",
                "2",
                "11",
                "6",
                "0",
            ]
        } else {
            &[
                "9007199254740993",
                "2",
                "3",
                "4",
                "5",
                "6",
                "7",
                "8",
                "9",
                "10",
                "11",
                "12",
                "100",
                "200",
                "1",
                "2",
                "3",
                "4",
                "0",
            ]
        }],
    )
}

fn interfaces(current: bool) -> Table {
    table(
        &[
            "interface",
            "name",
            "received_bytes",
            "sent_bytes",
            "received_packets",
            "sent_packets",
            "receive_errors",
            "send_errors",
            "receive_drops",
        ],
        &[if current {
            &["7", "en0", "3048", "3024", "18", "20", "3", "6", "9"]
        } else {
            &["7", "en0", "1000", "2000", "10", "20", "1", "2", "3"]
        }],
    )
}

fn set(table: &mut Table, row: usize, name: &str, value: Option<&str>) {
    let index = table
        .columns
        .iter()
        .position(|column| column == name)
        .unwrap();
    table.rows[row][index] = value.map(str::to_owned);
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
    ($name:ident, $column:literal, $counter:expr, $expected:tt) => {
        #[test]
        fn $name() {
            // Goal: one unavailable counter does not erase another measured rate.
            // Method: remove or reset one current counter and compare all four outputs.
            let mut fixture = Fixture::new();
            set(fixture.row("interfaces", 2), 0, $column, $counter);

            let value = fixture.point();

            assert_eq!(network_rates(&value), json!($expected));
            assert_eq!(value["network_rx_bps"], 1024.0);
            assert_eq!(value["network_tx_bps"], 512.0);
        }
    };
}

unavailable_network_counter!(
    preserves_other_network_rates_when_received_packets_reset,
    "received_packets",
    Some("0"),
    [null, 0.0, 1.0, 2.0]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_sent_packets_reset,
    "sent_packets",
    Some("0"),
    [4.0, null, 1.0, 2.0]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_receive_errors_reset,
    "receive_errors",
    Some("0"),
    [4.0, 0.0, null, 2.0]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_send_errors_reset,
    "send_errors",
    Some("0"),
    [4.0, 0.0, 1.0, null]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_received_packets_are_missing,
    "received_packets",
    None,
    [null, 0.0, 1.0, 2.0]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_sent_packets_are_missing,
    "sent_packets",
    None,
    [4.0, null, 1.0, 2.0]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_receive_errors_are_missing,
    "receive_errors",
    None,
    [4.0, 0.0, null, 2.0]
);
unavailable_network_counter!(
    preserves_other_network_rates_when_send_errors_are_missing,
    "send_errors",
    None,
    [4.0, 0.0, 1.0, null]
);

macro_rules! changed_network_identity {
    ($name:ident, $column:literal, $changed:literal) => {
        #[test]
        fn $name() {
            // Goal: reused interface indices or renamed interfaces cannot authorize rates.
            // Method: change one part of the current interface key, keeping the counters.
            let mut fixture = Fixture::new();
            set(fixture.row("interfaces", 2), 0, $column, Some($changed));

            assert_eq!(
                network_rates(&fixture.point()),
                json!([null, null, null, null])
            );
        }
    };
}

changed_network_identity!(
    leaves_network_rates_unavailable_after_interface_renaming,
    "name",
    "utun0"
);
changed_network_identity!(
    leaves_network_rates_unavailable_after_index_reuse,
    "interface",
    "8"
);

#[test]
fn leaves_network_rates_unavailable_after_interface_membership_changes() {
    // Goal: changing the interface set cannot make a partial aggregate look complete.
    // Method: remove the current interface while retaining the earlier measurement.
    let mut fixture = Fixture::new();
    fixture.row("interfaces", 2).rows.clear();

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
    fixture.row("interfaces", 1).rows.push(
        ["8", "utun0", "0", "0", "2", "3", "4", "5", "0"]
            .map(|value| Some(value.to_owned()))
            .to_vec(),
    );
    fixture.row("interfaces", 2).rows.insert(
        0,
        ["8", "utun0", "0", "0", "8", "13", "12", "17", "0"]
            .map(|value| Some(value.to_owned()))
            .to_vec(),
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
fn rejects_gpu_residency_with_a_refused_previous_sample() {
    // Goal: refusal cannot become measured GPU work or power.
    // Method: keep numeric counters but mark the earlier IOReport read unsuccessful.
    let mut fixture = Fixture::new();
    set(fixture.row("ticks", 1), 0, "ioreport_read", Some("false"));

    let value = fixture.point();

    assert!(value["gpu_active_percent"].is_null());
    assert!(value["gpu_watts"].is_null());
}

#[test]
fn rejects_gpu_residency_with_a_changed_declared_state_count() {
    // Goal: a changing state domain cannot produce an aggregate residency rate.
    // Method: change only the earlier declared count while keeping both state tables.
    let mut fixture = Fixture::new();
    set(fixture.row("ticks", 1), 0, "gpu_state_count", Some("3"));

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_with_zero_declared_states() {
    // Goal: no active-state domain means unavailable rather than measured idle.
    // Method: declare zero states and provide no state rows at either endpoint.
    let mut fixture = Fixture::new();
    set(fixture.row("ticks", 1), 0, "gpu_state_count", Some("0"));
    set(fixture.row("ticks", 2), 0, "gpu_state_count", Some("0"));
    fixture.row("gpu_states", 1).rows.clear();
    fixture.row("gpu_states", 2).rows.clear();

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_with_a_missing_state_row() {
    // Goal: partial residency is never reported as a complete GPU percentage.
    // Method: remove one of two declared rows from the current endpoint.
    let mut fixture = Fixture::new();
    fixture.row("gpu_states", 2).rows.pop().unwrap();

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_with_duplicate_state_rows() {
    // Goal: duplicated rows cannot disguise a missing state at the declared row count.
    // Method: give the two current rows the same state index.
    let mut fixture = Fixture::new();
    set(fixture.row("gpu_states", 2), 1, "state", Some("0"));

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_with_an_out_of_range_state() {
    // Goal: a matching row count cannot substitute a different state domain.
    // Method: replace state 1 with state 2 in a two-state sample.
    let mut fixture = Fixture::new();
    set(fixture.row("gpu_states", 2), 1, "state", Some("2"));

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_after_a_counter_reset() {
    // Goal: a reset is unavailable, never a negative or wrapped activity reading.
    // Method: move one current state counter below its previous value.
    let mut fixture = Fixture::new();
    set(fixture.row("gpu_states", 2), 0, "active_ns", Some("0"));

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn rejects_gpu_residency_with_unavailable_state_data() {
    // Goal: SQL NULL residency remains unavailable.
    // Method: keep the state identity but remove its counter.
    let mut fixture = Fixture::new();
    set(fixture.row("gpu_states", 2), 1, "active_ns", None);

    assert!(fixture.point()["gpu_active_percent"].is_null());
}

#[test]
fn preserves_measured_idle_and_never_clamps_gpu_activity() {
    // Goal: measured zero differs from missing data, and source counters are not clamped.
    // Method: reuse the earlier counters, then increase one state beyond a full interval.
    let mut fixture = Fixture::new();
    fixture.row("gpu_states", 2).rows = fixture.row("gpu_states", 1).rows.clone();
    assert_eq!(fixture.point()["gpu_active_percent"], 0.0);

    set(
        fixture.row("gpu_states", 2),
        0,
        "active_ns",
        Some("4000000000"),
    );
    assert_eq!(fixture.point()["gpu_active_percent"], 150.0);
}

#[test]
fn preserves_independent_memory_and_network_counter_availability() {
    // Goal: one reset cannot create a false rate or erase other measured rates.
    // Method: reset one VM counter and remove one network counter independently.
    let mut fixture = Fixture::new();
    set(fixture.row("memory", 2), 0, "compressions", Some("0"));
    set(fixture.row("interfaces", 2), 0, "received_bytes", None);

    let value = fixture.point();

    assert!(value["memory_compressions_per_second"].is_null());
    assert!(value["network_rx_bps"].is_null());
    assert_eq!(value["memory_pageins_per_second"], 3.0);
    assert_eq!(value["network_tx_bps"], 512.0);
}

const POINT_FIELDS: [&str; 44] = [
    "battery",
    "boot",
    "continuous_ns",
    "cpu_level_ghz",
    "cpu_level_percent",
    "cpu_percent",
    "cpu_percent_one_core",
    "fan_rpm",
    "gpu_active_percent",
    "gpu_watts",
    "interval_unavailable",
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
    assert_eq!(fields.values().filter(|value| value.is_f64()).count(), 17);
    assert_eq!(fields.values().filter(|value| value.is_u64()).count(), 18);
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
fn preserves_measured_empty_interface_domains_after_saved_partitioning() {
    // Goal: partitioning an empty measured interface table cannot turn zero into unavailable.
    // Method: compare live empty row sets with the saved partition's absent publication rows.
    let mut fixture = Fixture::new();
    fixture.row("interfaces", 1).rows.clear();
    fixture.row("interfaces", 2).rows.clear();
    let live = fixture.point();
    fixture.tables.get_mut(&(0, "interfaces")).unwrap().clear();

    assert_eq!(fixture.point(), live);
    assert_eq!(live["network_rx_bps"], 0.0);
    assert_eq!(network_rates(&live), json!([0.0, 0.0, 0.0, 0.0]));
    fixture.tables.remove(&(0, "interfaces")).unwrap();
    assert!(fixture.point()["network_rx_bps"].is_null());
    assert_eq!(
        network_rates(&fixture.point()),
        json!([null, null, null, null])
    );
}

#[test]
fn preserves_zero_gpu_energy_and_refuses_energy_resets() {
    // Goal: measured zero power remains distinct from a reset energy counter.
    // Method: set equal energy endpoints, then make the later energy smaller.
    let mut fixture = Fixture::new();
    set(
        fixture.row("ticks", 2),
        0,
        "gpu_energy_nj",
        Some("1000000000"),
    );
    assert_eq!(fixture.point()["gpu_watts"], 0.0);

    set(fixture.row("ticks", 2), 0, "gpu_energy_nj", Some("0"));
    assert!(fixture.point()["gpu_watts"].is_null());
    assert_eq!(fixture.point()["gpu_active_percent"], 40.0);
}

#[test]
fn leaves_rates_unavailable_without_a_compatible_predecessor() {
    // Goal: no source counter interval is fabricated at startup or a run-tick gap.
    // Method: omit the predecessor, then skip a run tick with numeric tables unchanged.
    let mut fixture = Fixture::new();
    let first = point(&fixture.tables, &fixture.current, None, 1).unwrap();
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
    fixture.row("interfaces", 2).rows = fixture.row("interfaces", 1).rows.clone();

    assert_eq!(network_rates(&fixture.point()), json!([0.0, 0.0, 0.0, 0.0]));
}

#[test]
fn refuses_a_missing_fan_source_in_common_projection() {
    // Goal: a missing source cannot turn into a measured empty array or an unread gap.
    // Method: remove the loaded source map from an otherwise complete scalar observation.
    let mut fixture = Fixture::new();
    assert!(fixture.tables.remove(&(0, "fans")).is_some());

    let result = point(&fixture.tables, &fixture.current, None, 1);

    assert!(matches!(
        result,
        Err(Error::Bound("missing required recorded fan source"))
    ));
}

macro_rules! malformed_common_fan {
    ($name:ident, $change:expr, $reason:literal) => {
        #[test]
        fn $name() {
            // Goal: common scalar projection refuses malformed fan associations.
            // Method: corrupt one fact of a measured, complete two-channel source.
            let mut fixture = Fixture::new();
            set(fixture.row("ticks", 2), 0, "smc_read", Some("true"));
            fixture.tables.get_mut(&(0, "fans")).unwrap().insert(
                2,
                table(
                    &["publication", "fan", "rpm"],
                    &[&["2", "0", "12"], &["2", "1", "0"]],
                ),
            );
            $change(&mut fixture);

            let result = point(&fixture.tables, &fixture.current, None, 1);

            assert!(matches!(result, Err(Error::Bound($reason))), "{result:?}");
        }
    };
}
malformed_common_fan!(
    refuses_a_changed_fan_publication_in_common_projection,
    |fixture: &mut Fixture| set(fixture.row("fans", 2), 0, "publication", Some("3")),
    "fan row publication"
);
malformed_common_fan!(
    refuses_duplicate_fan_channels_in_common_projection,
    |fixture: &mut Fixture| set(fixture.row("fans", 2), 1, "fan", Some("0")),
    "fan channel order"
);
malformed_common_fan!(
    refuses_missing_fan_columns_in_common_projection,
    |fixture: &mut Fixture| fixture.row("fans", 2).columns.clear(),
    "missing schema column"
);
malformed_common_fan!(
    refuses_too_many_fan_channels_in_common_projection,
    |fixture: &mut Fixture| fixture.row("fans", 2).rows.resize(11, vec![None; 3]),
    "fan channels"
);

#[test]
fn retains_nullable_fan_channels_in_common_projection() {
    // Goal: SQL NULL remains a channel gap beside stopped zero in common scalar projection.
    // Method: project a real-shaped nullable fan table through the same point builder.
    let mut fixture = Fixture::new();
    set(fixture.row("ticks", 2), 0, "smc_read", Some("true"));
    fixture.tables.get_mut(&(0, "fans")).unwrap().insert(
        2,
        table(
            &["publication", "fan", "rpm"],
            &[&["2", "0", "NULL"], &["2", "1", "0"]],
        ),
    );

    assert_eq!(fixture.point()["fan_rpm"], json!([null, 0.0]));
}
