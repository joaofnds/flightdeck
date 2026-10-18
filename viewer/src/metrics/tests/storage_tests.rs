use super::*;
use collector::disks::{DiskReading, DiskSnapshot, ScanState};

#[test]
fn projects_per_device_rates_and_weighted_completed_request_time() {
    // Goal: a selected point shows real device deltas and duration weighted machine means.
    // Method: project two complete snapshots through the metric point used by live and saved views.
    let mut fixture = fixture();
    capture(
        &mut fixture,
        1,
        &snapshot(
            1,
            &[
                device(1, 100, 10, 10_000_000),
                device(2, 100, 20, 20_000_000),
            ],
        ),
    );
    capture(
        &mut fixture,
        2,
        &snapshot(
            2,
            &[
                device(1, 300, 12, 16_000_000),
                device(2, 700, 26, 50_000_000),
            ],
        ),
    );

    let point = point(
        &fixture.tables,
        &fixture.current,
        Some(&fixture.previous),
        1,
    )
    .unwrap();
    let storage = &point["storage"];

    assert_eq!(storage["state"], "complete");
    assert_eq!(storage["read_bps"], 800.0);
    assert_eq!(storage["read_ops_per_second"], 8.0);
    assert_eq!(storage["read_mean_ms"], 4.5);
    assert_eq!(storage["devices"][0]["read_mean_ms"], 3.0);
    assert_eq!(storage["devices"][1]["read_mean_ms"], 5.0);
}

fn snapshot(second: u64, devices: &[DiskReading]) -> DiskSnapshot {
    let mut snapshot = DiskSnapshot {
        source_continuous_ns: second * 1_000_000_000,
        count: devices.len(),
        state: ScanState::Complete,
        ..DiskSnapshot::default()
    };
    snapshot.records[..devices.len()].copy_from_slice(devices);
    snapshot
}

fn device(identity: u64, bytes: u64, operations: u64, duration: u64) -> DiskReading {
    let mut name = [0; 32];
    name[..5].copy_from_slice(b"disk0");
    DiskReading {
        driver_id: identity,
        media_id: identity + 100,
        name,
        read_bytes: Some(bytes),
        write_bytes: Some(bytes),
        read_operations: Some(operations),
        write_operations: Some(operations),
        read_errors: Some(0),
        write_errors: Some(0),
        read_time_ns: Some(duration),
        write_time_ns: Some(duration),
        ..DiskReading::default()
    }
}

fn capture(fixture: &mut Fixture, publication: u64, snapshot: &DiskSnapshot) {
    let table = fixture
        .tables
        .get_mut(&(0, "ticks"))
        .unwrap()
        .get_mut(&publication)
        .unwrap();
    table.columns.push("physical_disks".into());
    table.rows[0].push(Some(persistence::disks::encode(snapshot).unwrap()));
}

fn fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.current.continuous = Some(2_000_000_000);
    fixture
}

#[test]
fn keeps_current_devices_visible_without_a_baseline() {
    // Goal: a first reading retains identity while each derived value stays unavailable.
    // Method: project a genuine current snapshot without a preceding disk cell.
    let mut fixture = fixture();
    capture(&mut fixture, 2, &snapshot(2, &[device(1, 100, 10, 1000)]));

    let storage = fixture.point()["storage"].clone();

    assert_eq!(storage["devices"][0]["driver_id"], 1);
    assert!(storage["devices"][0]["read_bps"].is_null());
    assert!(storage["read_bps"].is_null());
}

#[test]
fn refuses_totals_for_replacement_even_when_device_counts_match() {
    // Goal: matched-device rates never masquerade as complete machine totals after replacement.
    // Method: replace B with C between two complete inventories of equal cardinality.
    let mut fixture = fixture();
    capture(
        &mut fixture,
        1,
        &snapshot(1, &[device(1, 100, 10, 1000), device(2, 100, 10, 1000)]),
    );
    capture(
        &mut fixture,
        2,
        &snapshot(2, &[device(1, 200, 12, 2000), device(3, 300, 20, 3000)]),
    );

    let storage = fixture.point()["storage"].clone();

    assert_eq!(storage["devices"][0]["read_bps"], 100.0);
    assert!(storage["devices"][1]["read_bps"].is_null());
    assert!(storage["read_bps"].is_null());
}

macro_rules! invalid_baseline {
    ($name:ident, $change:expr) => {
        #[test]
        fn $name() {
            // Goal: incompatible observations preserve current identities but never borrow rates.
            // Method: break one prerequisite of a valid two-snapshot point.
            let mut fixture = fixture();
            capture(&mut fixture, 1, &snapshot(1, &[device(1, 100, 10, 1000)]));
            capture(&mut fixture, 2, &snapshot(2, &[device(1, 200, 20, 2000)]));
            ($change)(&mut fixture);

            let storage = fixture.point()["storage"].clone();

            assert!(storage["read_bps"].is_null());
            assert!(storage["devices"][0]["read_bps"].is_null());
            assert_eq!(storage["devices"][0]["driver_id"], 1);
        }
    };
}

invalid_baseline!(
    rejects_a_missed_publication_baseline,
    |fixture: &mut Fixture| fixture.previous.publication = 0
);
invalid_baseline!(rejects_a_missed_tick_baseline, |fixture: &mut Fixture| {
    fixture.previous.run_tick = 0;
});
invalid_baseline!(
    rejects_a_missed_wall_second_baseline,
    |fixture: &mut Fixture| fixture.previous.wall -= 2_000_000_000
);

macro_rules! unavailable_source {
    ($name:ident, $source_time:expr) => {
        #[test]
        fn $name() {
            // Goal: future and stale disk snapshots never appear at the selected timestamp.
            // Method: encode a valid cell whose source timestamp differs from this tick.
            let mut fixture = fixture();
            capture(
                &mut fixture,
                2,
                &snapshot($source_time, &[device(1, 1, 1, 1)]),
            );

            assert!(fixture.point()["storage"].is_null());
        }
    };
}

unavailable_source!(rejects_future_storage_source_time, 3);
unavailable_source!(rejects_stale_storage_source_time, 1);

#[test]
fn keeps_independent_rates_when_one_counter_resets() {
    // Goal: a read-byte reset invalidates read throughput alone.
    // Method: retain increasing write/operation/error/duration counters in the same device pair.
    let mut fixture = fixture();
    let mut current = device(1, 200, 20, 20_000_000);
    current.read_bytes = Some(1);
    capture(
        &mut fixture,
        1,
        &snapshot(1, &[device(1, 100, 10, 10_000_000)]),
    );
    capture(&mut fixture, 2, &snapshot(2, &[current]));

    let storage = fixture.point()["storage"].clone();

    assert!(storage["read_bps"].is_null());
    assert_eq!(storage["write_bps"], 100.0);
    assert_eq!(storage["read_ops_per_second"], 10.0);
    assert_eq!(storage["read_errors_per_second"], 0.0);
    assert_eq!(storage["read_mean_ms"], 1.0);
}

#[test]
fn refuses_machine_totals_for_partial_inventories() {
    // Goal: a partial inventory may show a device rate but cannot claim a machine total.
    // Method: retain one compatible disk and mark its current enumeration partial.
    let mut fixture = fixture();
    let mut current = snapshot(2, &[device(1, 200, 20, 2000)]);
    current.state = ScanState::Partial;
    capture(&mut fixture, 1, &snapshot(1, &[device(1, 100, 10, 1000)]));
    capture(&mut fixture, 2, &current);

    let storage = fixture.point()["storage"].clone();

    assert_eq!(storage["state"], "partial");
    assert_eq!(storage["devices"][0]["read_bps"], 100.0);
    assert!(storage["read_bps"].is_null());
}

#[test]
fn keeps_mean_unavailable_when_a_device_reports_no_completed_time() {
    // Goal: an unsupported zero duration on a busy disk never becomes measured machine latency.
    // Method: combine one busy zero-duration disk with another reporting positive completed time.
    let mut fixture = fixture();
    capture(
        &mut fixture,
        1,
        &snapshot(1, &[device(1, 100, 10, 0), device(2, 100, 10, 10_000_000)]),
    );
    capture(
        &mut fixture,
        2,
        &snapshot(2, &[device(1, 200, 20, 0), device(2, 200, 20, 20_000_000)]),
    );

    let storage = fixture.point()["storage"].clone();

    assert!(storage["read_mean_ms"].is_null());
    assert!(storage["devices"][0]["read_mean_ms"].is_null());
    assert_eq!(storage["devices"][1]["read_mean_ms"], 1.0);
    assert_eq!(storage["read_ops_per_second"], 20.0);
}

#[test]
fn refuses_an_oversized_nine_hundred_point_sixteen_disk_response() {
    // Goal: maximum disk cardinality and wide numbers respect the existing response envelope.
    // Method: serialize the full public selection envelope with sixteen 31-byte device labels.
    let mut fixture = fixture();
    let mut before = snapshot(1, &[]);
    let mut after = snapshot(2, &[]);
    before.count = collector::disks::DISKS_MAX;
    after.count = collector::disks::DISKS_MAX;
    for index in 0..collector::disks::DISKS_MAX {
        let identity = u64::MAX - u64::try_from(index).unwrap() * 2 - 100;
        before.records[index] = device(identity, 0, 0, 0);
        after.records[index] = device(identity, u64::MAX, u64::MAX, u64::MAX);
        before.records[index].name[..31].fill(b'x');
        after.records[index].name[..31].fill(b'x');
    }
    capture(&mut fixture, 1, &before);
    capture(&mut fixture, 2, &after);
    let mut widest = widest_point();
    widest["storage"] = fixture.point()["storage"].clone();
    widen_storage_numbers(&mut widest["storage"]);
    let public = public_point(&widest).unwrap();

    let envelope = selection_envelope(&public, &widest["observation"], 900);
    let bytes = serde_json::to_vec(&envelope).unwrap().len();

    assert_eq!(public["storage"]["devices"].as_array().unwrap().len(), 16);
    assert!(bytes > 8 * 1024 * 1024, "{bytes}");
    let response = crate::bridge::tests::response_value(envelope["value"].clone());
    assert_eq!(
        response,
        json!({"value": null, "error": "response exceeds 8 MiB"})
    );
    println!("storage maximum selection response bytes: {bytes}");
}

fn widen_storage_numbers(value: &mut Value) {
    let fields = [
        "read_bps",
        "write_bps",
        "read_ops_per_second",
        "write_ops_per_second",
        "read_errors_per_second",
        "write_errors_per_second",
        "read_mean_ms",
        "write_mean_ms",
    ];
    for name in fields {
        value[name] = json!(0.000_012_345_678_901_234_568);
    }
    for device in value["devices"].as_array_mut().unwrap() {
        for name in fields {
            device[name] = json!(0.000_012_345_678_901_234_568);
        }
    }
}

#[test]
fn refuses_a_baseline_after_media_replacement_under_the_same_driver() {
    // Goal: a reused driver cannot bridge the counters of a replaced physical medium.
    // Method: change only media registry identity between otherwise compatible observations.
    let mut fixture = fixture();
    let mut current = device(1, 200, 20, 2000);
    current.media_id += 1;
    capture(&mut fixture, 1, &snapshot(1, &[device(1, 100, 10, 1000)]));
    capture(&mut fixture, 2, &snapshot(2, &[current]));

    let storage = fixture.point()["storage"].clone();

    assert_eq!(storage["devices"][0]["media_id"], 102);
    assert!(storage["devices"][0]["read_bps"].is_null());
    assert!(storage["read_bps"].is_null());
}

#[test]
fn subtracts_high_unsigned_counters_before_float_conversion() {
    // Goal: the low counter bits above floating-point integer precision remain in the delta.
    // Method: project an exact 37-byte increment near the u64 limit.
    let mut fixture = fixture();
    capture(
        &mut fixture,
        1,
        &snapshot(1, &[device(1, u64::MAX - 37, 10, 1000)]),
    );
    capture(
        &mut fixture,
        2,
        &snapshot(2, &[device(1, u64::MAX, 20, 2000)]),
    );

    assert_eq!(fixture.point()["storage"]["read_bps"], 37.0);
}

#[test]
fn preserves_other_device_fields_when_read_bytes_are_missing() {
    // Goal: one absent source field cannot erase independently valid operations and errors.
    // Method: remove read bytes alone from the current disk snapshot.
    let mut fixture = fixture();
    let mut current = device(1, 200, 20, 2000);
    current.read_bytes = None;
    capture(&mut fixture, 1, &snapshot(1, &[device(1, 100, 10, 1000)]));
    capture(&mut fixture, 2, &snapshot(2, &[current]));

    let storage = fixture.point()["storage"].clone();

    assert!(storage["read_bps"].is_null());
    assert_eq!(storage["write_bps"], 100.0);
    assert_eq!(storage["read_ops_per_second"], 10.0);
    assert_eq!(storage["read_errors_per_second"], 0.0);
}

#[test]
fn keeps_mean_unavailable_without_completed_operations() {
    // Goal: a measured idle interval has zero operations and no measured completed-request mean.
    // Method: project an unchanged operations counter with an unchanged positive duration counter.
    let mut fixture = fixture();
    capture(&mut fixture, 1, &snapshot(1, &[device(1, 100, 10, 1000)]));
    capture(&mut fixture, 2, &snapshot(2, &[device(1, 100, 10, 1000)]));

    let storage = fixture.point()["storage"].clone();

    assert_eq!(storage["read_ops_per_second"], 0.0);
    assert!(storage["read_mean_ms"].is_null());
    assert!(storage["devices"][0]["read_mean_ms"].is_null());
}

#[test]
fn preserves_successful_empty_inventories_as_measured_zero_totals() {
    // Goal: complete empty scans remain distinct from unavailable and failed readings.
    // Method: project consecutive successful empty device inventories.
    let mut fixture = fixture();
    capture(&mut fixture, 1, &snapshot(1, &[]));
    capture(&mut fixture, 2, &snapshot(2, &[]));

    let storage = fixture.point()["storage"].clone();

    assert_eq!(storage["state"], "complete");
    assert_eq!(storage["devices"], json!([]));
    assert_eq!(storage["read_bps"], 0.0);
    assert!(storage["read_mean_ms"].is_null());
}

macro_rules! incomplete_scan {
    ($name:ident, $state:expr, $text:literal) => {
        #[test]
        fn $name() {
            // Goal: incomplete empty scans never become complete measured zero machine totals.
            // Method: project two empty snapshots with the specified scan outcome.
            let mut fixture = fixture();
            let mut current = snapshot(2, &[]);
            current.state = $state;
            capture(&mut fixture, 1, &snapshot(1, &[]));
            capture(&mut fixture, 2, &current);

            let storage = fixture.point()["storage"].clone();

            assert_eq!(storage["state"], $text);
            assert_eq!(storage["devices"], json!([]));
            assert!(storage["read_bps"].is_null());
        }
    };
}

incomplete_scan!(
    preserves_failed_scan_availability,
    ScanState::Failed,
    "failed"
);
incomplete_scan!(
    preserves_exhausted_scan_availability,
    ScanState::Exhausted,
    "exhausted"
);
incomplete_scan!(
    preserves_partial_empty_scan_availability,
    ScanState::Partial,
    "partial"
);

#[test]
fn keeps_device_rates_when_only_the_machine_read_total_overflows() {
    // Goal: overflowing one machine field preserves valid per-device and independent write rates.
    // Method: project a two-disk read sum above u64MAX with a measured 100-byte write sum.
    let mut fixture = fixture();
    let mut first = device(1, u64::MAX, 1, 1_000_000);
    let mut other = device(2, 1, 1, 1_000_000);
    first.write_bytes = Some(40);
    other.write_bytes = Some(60);
    capture(
        &mut fixture,
        1,
        &snapshot(1, &[device(1, 0, 0, 0), device(2, 0, 0, 0)]),
    );
    capture(&mut fixture, 2, &snapshot(2, &[first, other]));

    let projected = public_point(&fixture.point()).unwrap();
    let storage = &projected["storage"];

    assert!(storage["read_bps"].is_null());
    assert_eq!(storage["devices"][0]["read_bps"], json!(number(u64::MAX)));
    assert_eq!(storage["devices"][1]["read_bps"], 1.0);
    assert_eq!(storage["write_bps"], 100.0);
    assert_eq!(storage["devices"][0]["write_bps"], 40.0);
    assert_eq!(storage["devices"][1]["write_bps"], 60.0);
}
