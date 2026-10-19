use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::{App, History, KeyKind, Point, Target};
use super::{CompactMeasure, Key, Measure, capture_key, measure, observation};
use crate::Cancellation;

#[test]
fn shares_repeated_keys_without_counting_their_allocation_per_observation() {
    // One thousand observations retain one long raw key and separate escaped-byte identities.
    let mut history = History::new();
    let long_key = super::bundle_key(0);
    let keys = [
        (KeyKind::Bundle, long_key.as_slice()),
        (KeyKind::Executable, b"\\xff".as_slice()),
        (KeyKind::Executable, b"\xff".as_slice()),
    ];

    retain(&mut history, &keys, 1000);
    let raw = response(&history, KeyKind::Executable, b"\xff");
    let escaped = response(&history, KeyKind::Executable, b"\\xff");

    assert_eq!(raw["points"].as_array().unwrap().len(), 1000);
    assert_eq!(raw["points"][999]["members"], 3);
    assert_eq!(escaped["points"][999]["members"], 2);
    assert_eq!(raw["coverage"]["retention_limited"], false);
    assert!(raw["coverage"]["cache_bytes"].as_u64().unwrap() < 1024 * 1024);
    assert_eq!(history.bytes, accounted_bytes(&history));
}

#[test]
fn keeps_shared_keys_accounted_while_expired_observations_are_replaced() {
    // The incoming point still owns the key when every previous reference is evicted.
    let mut history = History::new();
    let keys = [(KeyKind::Executable, b"same".as_slice())];
    retain(&mut history, &keys, 4);
    let mut incoming = point(&history, 5, &keys);
    incoming.observation.wall_ns = Some(1_001_000_000_000);

    history.push(incoming, 1_001_000_000_000);
    let response = response(&history, KeyKind::Executable, b"same");

    assert_eq!(response["points"].as_array().unwrap().len(), 1);
    assert_eq!(
        response["points"][0]["observation"]["stamp"]["publication"],
        "5"
    );
    assert_eq!(response["coverage"]["retention_limited"], false);
    assert_eq!(history.bytes, accounted_bytes(&history));
}

#[test]
fn preserves_app_absence_while_sorted_membership_changes() {
    // Sorted membership changes retain the overlap and emit gaps for absent Apps.
    let mut history = History::new();
    let first = [
        (KeyKind::Executable, b"A".as_slice()),
        (KeyKind::Executable, b"B"),
    ];
    let second = [
        (KeyKind::Executable, b"B".as_slice()),
        (KeyKind::Executable, b"C"),
    ];
    let third = [(KeyKind::Executable, b"C".as_slice())];
    history.push(point(&history, 1, &first), 0);
    history.push(point(&history, 2, &second), 0);
    history.push(point(&history, 3, &third), 0);

    let absent = response(&history, KeyKind::Executable, b"A");
    let retained = response(&history, KeyKind::Executable, b"B");
    let arriving = response(&history, KeyKind::Executable, b"C");

    assert_eq!(absent["points"][0]["members"], 1);
    assert_eq!(absent["points"][1]["members"], Value::Null);
    assert_eq!(absent["points"][2]["members"], Value::Null);
    assert_eq!(retained["points"][0]["members"], 2);
    assert_eq!(retained["points"][1]["members"], 1);
    assert_eq!(arriving["points"][0]["members"], Value::Null);
    assert_eq!(arriving["points"][2]["members"], 1);
    assert_eq!(history.bytes, accounted_bytes(&history));
}

#[test]
fn reclaims_churned_keys_when_their_last_retained_point_expires() {
    // Removing two old points releases departed keys while preserving the incoming shared key.
    let mut history = History::new();
    let first = [
        (KeyKind::Executable, b"A".as_slice()),
        (KeyKind::Executable, b"B"),
    ];
    let second = [
        (KeyKind::Executable, b"B".as_slice()),
        (KeyKind::Executable, b"C"),
    ];
    let third = [(KeyKind::Executable, b"C".as_slice())];
    history.push(point(&history, 1, &first), 0);
    history.push(point(&history, 2, &second), 0);
    let mut incoming = point(&history, 3, &third);
    incoming.observation.wall_ns = Some(1_001_000_000_000);
    let mut reference = History::new();
    reference.push(point(&reference, 3, &third), 0);

    history.push(incoming, 1_001_000_000_000);
    let response = response(&history, KeyKind::Executable, b"C");

    assert_eq!(response["coverage"]["observations_retained"], 1);
    assert_eq!(history.bytes, accounted_bytes(&history));
    assert_eq!(history.bytes, reference.bytes);
}

#[test]
fn keeps_coverage_and_full_unsigned_amounts_exact_after_compaction() {
    // Compact member counts do not compact or round any u64 metric amount.
    let history = History::new();
    let keys = [(KeyKind::Executable, b"exact".as_slice())];
    let mut point = point(&history, 1, &keys);
    let cpu = Measure {
        known: u64::MAX,
        measured_members: 24_000,
        unavailable_members: 0,
        overflowed: true,
    };
    let resident = Measure {
        known: 0,
        measured_members: 0,
        unavailable_members: 24_000,
        overflowed: false,
    };
    point.apps[0].members = 24_000;
    point.apps[0].cpu_ns = CompactMeasure::capture(&cpu);
    point.apps[0].resident_bytes = CompactMeasure::capture(&resident);

    let value = point.json(&Target {
        kind: KeyKind::Executable,
        bytes: b"exact",
    });

    assert_eq!(value["cpu_ns"], cpu.json());
    assert_eq!(value["resident_bytes"], resident.json());
    assert_eq!(value["members"], 24_000);
    assert_eq!(value["cpu_percent_one_core"], Value::Null);
    assert_eq!(value["membership_complete"], false);
}

#[test]
fn preserves_independent_disk_coverage_and_amounts_after_compaction() {
    // Goal: compaction keeps exact disk sums and independent directional/combined coverage.
    // Method: serialize an overflowed read, measured-zero write and unavailable joint total.
    let history = History::new();
    let keys = [(KeyKind::Executable, b"disk".as_slice())];
    let mut point = point(&history, 1, &keys);
    let read = Measure {
        known: u64::MAX,
        measured_members: 1,
        unavailable_members: 1,
        overflowed: true,
    };
    let written = Measure {
        known: 0,
        measured_members: 2,
        unavailable_members: 0,
        overflowed: false,
    };
    let combined = Measure {
        known: 0,
        measured_members: 0,
        unavailable_members: 2,
        overflowed: false,
    };
    point.apps[0].members = 2;
    point.apps[0].disk_read_bytes = CompactMeasure::capture(&read);
    point.apps[0].disk_written_bytes = CompactMeasure::capture(&written);
    point.apps[0].disk_io_bytes = CompactMeasure::capture(&combined);

    let value = point.json(&Target {
        kind: KeyKind::Executable,
        bytes: b"disk",
    });

    assert_eq!(value["disk_read_bytes"], read.json());
    assert_eq!(value["disk_written_bytes"], written.json());
    assert_eq!(value["disk_io_bytes"], combined.json());
    assert_eq!(value["disk_read_bps"], Value::Null);
    assert_eq!(value["disk_written_bps"], 0.0);
    assert_eq!(value["disk_io_bps"], Value::Null);
}

macro_rules! compact_measure_round_trip {
    ($name:ident, $known:expr, $measured:expr, $unavailable:expr, $overflowed:expr) => {
        #[test]
        fn $name() {
            // Goal: preserve amounts and independent coverage states in compact history.
            // Method: compare the compact output with the original public Measure contract.
            let source = Measure {
                known: $known,
                measured_members: $measured,
                unavailable_members: $unavailable,
                overflowed: $overflowed,
            };

            let compact = CompactMeasure::capture(&source);

            assert_eq!(compact.json(), source.json());
            assert_eq!(compact.measured(), source.measured());
        }
    };
}

compact_measure_round_trip!(preserves_empty_coverage_after_compaction, 0, 0, 0, false);
compact_measure_round_trip!(preserves_measured_zero_after_compaction, 0, 1, 0, false);
compact_measure_round_trip!(
    preserves_maximum_measured_count_after_compaction,
    u64::MAX,
    24_000,
    0,
    false
);
compact_measure_round_trip!(
    preserves_maximum_unavailable_count_after_compaction,
    0,
    0,
    24_000,
    false
);
compact_measure_round_trip!(
    preserves_mixed_coverage_above_float_precision_after_compaction,
    9_007_199_254_740_993,
    11_999,
    12_001,
    false
);
compact_measure_round_trip!(
    preserves_amount_byte_order_after_compaction,
    0x0123_4567_89ab_cdef,
    1,
    1,
    false
);
compact_measure_round_trip!(
    preserves_maximum_amount_with_overflow_after_compaction,
    u64::MAX,
    24_000,
    0,
    true
);
compact_measure_round_trip!(
    preserves_overflow_without_measured_members_after_compaction,
    0,
    0,
    24_000,
    true
);

#[test]
fn bounds_app_history_layout_with_all_seven_exact_measures() {
    // Goal: preserve fifteen minutes for ordinary App populations within the fixed cache budget.
    // Method: guard the actual compiler layouts that Point::bytes charges per App and observation.
    assert!(size_of::<CompactMeasure>() <= 12);
    assert!(align_of::<CompactMeasure>() <= align_of::<u32>());
    assert!(size_of::<App>() <= 96);
    eprintln!(
        "compact App history layout: {}",
        json!({"measure_bytes": size_of::<CompactMeasure>(), "app_bytes": size_of::<App>(),
            "point_bytes": size_of::<Point>(),
            "trace_point_bytes": size_of::<super::super::TracePoint>()})
    );
}

#[test]
fn shares_keys_across_runs_without_changing_observation_identity_or_metrics() {
    // An immutable raw key can be shared while each point retains its own run and values.
    let mut history = History::new();
    let keys = [(KeyKind::Executable, b"same".as_slice())];
    history.push(point(&history, 1, &keys), 0);
    let mut incoming = point(&history, 2, &keys);
    incoming.observation.stamp.run = [9, 10];
    incoming.segment = 2;
    incoming.elapsed_ns = None;

    history.push(incoming, 0);
    let response = response(&history, KeyKind::Executable, b"same");

    assert_eq!(
        response["points"][0]["observation"]["stamp"]["run"],
        json!(["5", "6"])
    );
    assert_eq!(
        response["points"][1]["observation"]["stamp"]["run"],
        json!(["9", "10"])
    );
    assert_eq!(response["points"][0]["segment"], "0");
    assert_eq!(response["points"][1]["segment"], "2");
    assert_eq!(response["points"][1]["cpu_percent_one_core"], Value::Null);
    assert_eq!(history.bytes, accounted_bytes(&history));
}

#[test]
#[should_panic(expected = "value <= maximum")]
fn refuses_member_counts_above_the_source_population_limit() {
    // A future producer cannot silently narrow a count outside the validated source bound.
    super::super::compact::count(24_001);
}

fn retain(history: &mut History, keys: &[(KeyKind, &[u8])], count: u64) {
    for publication in 1..=count {
        history.push(point(history, publication, keys), 0);
    }
}

fn point(history: &History, publication: u64, keys: &[(KeyKind, &[u8])]) -> Box<Point> {
    let previous = history.points.back().map_or(&[][..], |point| &point.apps);
    let mut previous_index = 0;
    let apps = keys
        .iter()
        .enumerate()
        .map(|(index, (kind, bytes))| App {
            key: capture_key(
                &Key {
                    kind: *kind,
                    bytes: bytes.to_vec(),
                    display: String::new(),
                },
                previous,
                &mut previous_index,
            ),
            members: u16::try_from(index + 1).unwrap(),
            membership_complete: false,
            cpu_ns: measure(),
            energy_nj: measure(),
            disk_read_bytes: measure(),
            disk_written_bytes: measure(),
            disk_io_bytes: measure(),
            resident_bytes: measure(),
            footprint_bytes: measure(),
        })
        .collect();

    Box::new(Point {
        observation: observation(publication),
        segment: 0,
        elapsed_ns: Some(1_000_000_000),
        apps,
        groups: None,
        membership_complete: false,
        resident_ungrouped_members: 0,
        ungrouped_members: 0,
        usage_refused: 0,
        usage_gone: 0,
    })
}

fn response(history: &History, kind: KeyKind, bytes: &[u8]) -> Value {
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();

    history
        .response(
            &history.points.back().unwrap().observation,
            &Target { kind, bytes },
            &operation,
        )
        .unwrap()
}

fn accounted_bytes(history: &History) -> usize {
    let mut addresses = BTreeSet::new();
    let mut bytes = size_of::<History>()
        + history.points.capacity() * size_of::<Box<Point>>()
        + history.observations.capacity() * size_of::<super::super::Observation>();
    for point in &history.points {
        bytes += point.bytes();
        for app in &point.apps {
            if addresses.insert(Arc::as_ptr(&app.key).addr()) {
                bytes += app.key.bytes();
            }
        }
    }

    bytes
}
