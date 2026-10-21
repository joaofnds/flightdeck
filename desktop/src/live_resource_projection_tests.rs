use serde_json::{Value, json};

use super::panels;

#[test]
fn storage_projects_source_rates_in_display_units_without_negating_write_readouts() {
    // Goal: the Storage page exposes four measured histories and keeps write readouts positive.
    // Method: project source-shaped nested values through the production panels API.
    let selection = selection(&json!({"storage": {
        "state": "complete", "read_bps": 2_000_000, "write_bps": 1_000_000,
        "read_ops_per_second": 4.0, "write_ops_per_second": 2.0,
        "read_mean_ms": 1.5, "write_mean_ms": 2.5,
        "read_errors_per_second": 0.0, "write_errors_per_second": null, "devices": []
    }}));

    let projected = panels("storage", &selection, None).unwrap();

    assert_eq!(projected.len(), 5);
    assert_eq!(projected[0].unit, "MB/s");
    assert_eq!(projected[0].lines[0].values, [Some(2.0)]);
    assert_eq!(projected[0].lines[1].values, [Some(1.0)]);
    assert_eq!(projected[0].lines[1].negative, Some(true));
    assert_eq!(projected[1].lines[1].negative, Some(true));
    assert_eq!(projected[2].unit, "ms");
    assert_eq!(projected[2].lines[1].values, [Some(2.5)]);
    assert_eq!(projected[3].lines[0].values, [Some(0.0)]);
    assert_eq!(projected[3].lines[1].values, [None]);
}

#[test]
fn battery_projects_charge_signed_flow_and_binary_charging_history() {
    // Goal: battery discharge keeps its sign and charging false remains a measured state.
    // Method: project nested controller gauges through the production panels API.
    let selection = selection(&json!({"battery": {
        "charge_percent": 67.0, "power_watts": -4.5, "charging": false
    }}));

    let projected = panels("battery", &selection, None).unwrap();

    assert_eq!(projected.len(), 3);
    assert_eq!(projected[0].unit, "%");
    assert_eq!(projected[0].maximum, Some(100.0));
    assert_eq!(projected[0].lines[0].values, [Some(67.0)]);
    assert_eq!(projected[1].lines[0].values, [Some(-4.5)]);
    assert_eq!(projected[1].lines[0].negative, Some(false));
    assert_eq!(projected[2].kind, "timeline");
    assert_eq!(
        projected[2].states.as_ref().unwrap(),
        &["Not charging", "Charging"]
    );
    assert_eq!(projected[2].lines[0].values[959], Some(0.0));
    assert_eq!(projected[2].selected_index, Some(959));
    assert_eq!(
        projected[2].help.as_deref(),
        Some(
            "Battery-controller charging state by observed second. The chosen \
            same-second publication takes priority; otherwise the latest reading is shown. \
            Blank means unavailable or unobserved. This state does not imply external power."
        )
    );
}

#[test]
fn overview_includes_measured_physical_disk_throughput() {
    // Goal: the default page retains the source-backed physical-disk chart.
    // Method: project a measured device total through the normal Overview API.
    let selection = selection(&json!({"storage": {"read_bps": 3_000_000, "write_bps": 0}}));

    let projected = panels("overview", &selection, None).unwrap();

    assert_eq!(projected.len(), 9);
    let disk = projected
        .iter()
        .find(|panel| panel.title == "Disk throughput")
        .unwrap();
    assert_eq!(disk.lines[0].values, [Some(3.0)]);
    assert_eq!(disk.lines[1].values, [Some(0.0)]);
}

#[test]
fn charging_history_keeps_the_exact_selected_alternative_and_unobserved_seconds_blank() {
    // Goal: a later publication cannot replace an explicitly selected charging state.
    // Method: project conflicting observations in one second with the earlier one selected.
    let mut selection = selection(&json!({"battery": {"charging": false}}));
    let mut sibling = selection["trends"][0].clone();
    sibling["publication"] = json!(8);
    sibling["battery"]["charging"] = json!(true);
    selection["trends"].as_array_mut().unwrap().push(sibling);

    let projected = panels("battery", &selection, None).unwrap();

    assert_eq!(projected.len(), 3);
    assert_eq!(projected[2].lines[0].values[958], None);
    assert_eq!(projected[2].lines[0].values[959], Some(0.0));
    assert_eq!(projected[2].selected_index, Some(959));
}

#[test]
fn legacy_sources_keep_storage_and_battery_readings_unavailable() {
    // Goal: an old recording cannot borrow captured reference values for new resource pages.
    // Method: project a real observation with neither optional resource payload.
    let selection = selection(&json!({}));

    let storage = panels("storage", &selection, None).unwrap();
    let battery = panels("battery", &selection, None).unwrap();

    assert_eq!(storage.len(), 5);
    assert_eq!(battery.len(), 3);
    assert!(
        storage
            .iter()
            .flat_map(|panel| &panel.lines)
            .all(|line| line.values.iter().all(Option::is_none))
    );
    assert!(
        battery
            .iter()
            .flat_map(|panel| &panel.lines)
            .all(|line| line.values.iter().all(Option::is_none))
    );
}

#[test]
fn battery_histories_retain_wall_spacing_and_run_breaks() {
    // Goal: battery histories keep the same provenance and elapsed-time coordinates as CPU.
    // Method: project two observations with a gap and a changed run identity.
    let mut selection = selection(&json!({"battery": {"charge_percent": 50.0}}));
    let mut before = selection["trends"][0].clone();
    before["run"] = json!("00000000000000090000000000000002");
    before["wall_ns"] = json!(550_000_000_000_u64);
    before["publication"] = json!(1);
    selection["trends"]
        .as_array_mut()
        .unwrap()
        .insert(0, before);

    let projected = panels("battery", &selection, None).unwrap();

    assert_eq!(projected.len(), 3);
    assert_eq!(projected[0].segments.as_ref().unwrap(), &[0, 1]);
    let positions = projected[0].positions.as_ref().unwrap();
    assert!((positions[0] - 509.0 / 960.0).abs() < 0.000_001);
    assert!((positions[1] - 959.75 / 960.0).abs() < 0.000_001);
    assert_eq!(projected[0].selected_index, Some(1));
}

fn selection(resources: &Value) -> Value {
    let mut point = json!({
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004",
        "publication": 7, "wall_ns": 1_000_750_000_000_u64, "segment": 1
    });
    for (key, value) in resources.as_object().unwrap() {
        point[key] = value.clone();
    }

    json!({"second": 1000, "context_last_second": 1000, "selected": point.clone(),
        "summary": point.clone(), "trends": [point]})
}
