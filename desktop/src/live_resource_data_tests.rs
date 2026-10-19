use serde_json::{Value, json};

use super::{Field, devices, notice};

#[test]
fn storage_partial_inventory_retains_measured_device_rates_and_warns_about_totals() {
    // Goal: a partial physical-disk scan exposes per-device readings without implying completeness.
    // Method: format a selected partial response with one device and null aggregate rates.
    let selection = selection(&json!({"storage": {"state": "partial", "devices": [
        {"driver_id": 91, "media_id": 3, "name": "disk0", "read_bps": 2_000_000}
    ]}}));

    assert_eq!(devices(&selection).len(), 1);
    assert_eq!(
        notice("storage", &selection).as_deref(),
        Some("Some physical-disk readings are unavailable. Totals are incomplete.")
    );
}

#[test]
fn physical_device_fields_keep_units_zero_and_independent_missingness() {
    // Goal: physical device readouts expose every supported rate without aggregating devices.
    // Method: format one source-shaped device with measured, zero and null counters.
    let device = json!({"read_bps": 2_000_000, "write_bps": 0,
        "read_ops_per_second": 4, "write_ops_per_second": 2,
        "read_mean_ms": 1.5, "write_mean_ms": null,
        "read_errors_per_second": 0, "write_errors_per_second": 0.5});

    let fields = super::device_fields(&device);

    assert_eq!(
        fields,
        vec![
            field("Read", "2.0 MB/s"),
            field("Write", "0.0 MB/s"),
            field("Read operations", "4.0 ops/s"),
            field("Write operations", "2.0 ops/s"),
            field("Read mean", "1.5 ms"),
            field("Write mean", "Unavailable"),
            field("Read errors", "0.0 errors/s"),
            field("Write errors", "0.5 errors/s")
        ]
    );
}

#[test]
fn mismatched_observation_cannot_lend_storage_devices_or_a_battery_notice() {
    // Goal: a sibling publication cannot supply selected resource content.
    // Method: keep measured resources while changing only the summary publication.
    let mut selection = selection(&json!({"battery": {"charge_percent": 70},
        "storage": {"state": "complete", "devices": [{"name": "disk0"}]}}));
    selection["summary"]["publication"] = json!(10);

    assert!(devices(&selection).is_empty());
    assert_eq!(
        notice("battery", &selection).as_deref(),
        Some("No battery reading is available at this observation.")
    );
}

#[test]
fn small_nonzero_device_rates_and_mean_times_remain_distinct_from_zero() {
    // Goal: sub-tenth device readings remain measured nonzero values in selected details.
    // Method: format real-valued rates and mean I/O time below fixed one-decimal precision.
    let device = json!({"read_mean_ms": 0.004, "read_errors_per_second": 0.001});

    let fields = super::device_fields(&device);

    assert_eq!(fields[4], field("Read mean", "0.004 ms"));
    assert_eq!(fields[6], field("Read errors", "0.001 errors/s"));
}

fn field(name: &'static str, reading: &str) -> Field {
    Field {
        name,
        reading: reading.into(),
    }
}

fn selection(resources: &Value) -> Value {
    let mut selected = json!({"run": "run", "boot": "boot", "publication": 9,
        "wall_ns": 1_000_000_000_000_u64});
    let mut summary = selected.clone();
    for (key, value) in resources.as_object().unwrap() {
        summary[key] = value.clone();
    }
    selected["window_losses"] = Value::Null;
    json!({"selected": selected, "summary": summary})
}
