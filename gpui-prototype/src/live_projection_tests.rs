use serde_json::json;

use super::*;

#[test]
fn clicking_the_selected_second_keeps_its_chosen_publication() {
    // Same-second alternatives must retain the selected identity after a chart click.
    let selection = selection();
    assert_eq!(
        inspection_identity(&selection, 1000),
        Some(selection["selected"].clone())
    );
    assert_eq!(
        inspection_identity(&selection, 550).unwrap()["publication"],
        2
    );
    assert!(inspection_identity(&selection, 999).is_none());
}

fn selection() -> Value {
    json!({"second": 1000, "selected": {"publication": 3,
        "run": "00000000000000050000000000000006",
        "boot": "00000000000000030000000000000004"}, "trends": [
        {"run": "00000000000000010000000000000002",
         "boot": "00000000000000030000000000000004", "segment": 1,
         "wall_ns": 101_500_000_000_u64, "publication": 1, "cpu_percent": null},
        {"run": "00000000000000010000000000000002",
         "boot": "00000000000000030000000000000004", "segment": 1,
         "wall_ns": 550_000_000_000_u64, "publication": 2, "cpu_percent": 25.0,
         "gpu_watts": 2.0, "power_watts": 8.0,
         "network_rx_bps": 2048, "network_tx_bps": 1024,
         "memory_active_bytes": 1_073_741_824_u64},
        {"run": "00000000000000050000000000000006",
         "boot": "00000000000000030000000000000004", "segment": 1,
         "wall_ns": 1_000_750_000_000_u64, "publication": 3, "cpu_percent": null}
    ]})
}

#[test]
fn positions_use_wall_time_and_segments_include_run_identity() {
    // Goal: sparse ticks keep real spacing and never connect across collector runs.
    // Method: project fractional, widely spaced observations sharing a segment number.
    let panels = panels("cpu", &selection(), None).unwrap();
    let positions = panels[0].positions.as_ref().unwrap();
    assert!((positions[0] - 0.5 / 900.0).abs() < 0.000_001);
    assert!((positions[1] - 449.0 / 900.0).abs() < 0.000_001);
    assert!((positions[2] - 899.75 / 900.0).abs() < 0.000_001);
    assert_eq!(panels[0].segments.as_ref().unwrap(), &[0, 0, 1]);
    assert_eq!(panels[0].lines[0].values, [None, Some(25.0), None]);
    assert_eq!(panels[0].maximum, Some(100.0));
}

#[test]
fn sends_are_mirrored_without_negating_readouts_or_errors() {
    // Goal: directional traffic follows Swift's mirrored plots while values stay positive.
    // Method: compare traffic, packets and error line metadata and byte conversion.
    let panels = panels("network", &selection(), None).unwrap();
    assert_eq!(panels.len(), 3);
    assert_eq!(panels[0].unit, "KiB/s");
    assert_eq!(panels[0].lines[0].values[1], Some(2.0));
    assert_eq!(panels[0].lines[1].values[1], Some(1.0));
    assert_eq!(panels[0].lines[1].negative, Some(true));
    assert_eq!(panels[1].lines[1].negative, Some(true));
    assert_eq!(panels[1].lines[1].dashed, Some(true));
    assert_eq!(panels[2].lines[1].negative, Some(false));
}

#[test]
fn energy_series_remain_independent_and_memory_categories_only_stack_in_overview() {
    // Goal: avoid inventing total power or a computed used-memory total.
    // Method: inspect source values and stacking on both memory routes.
    let selection = selection();
    let energy = panels("energy", &selection, None).unwrap();
    assert_eq!(energy[0].lines.len(), 2);
    assert_eq!(energy[0].lines[0].values[1], Some(8.0));
    assert_eq!(energy[0].lines[1].values[1], Some(2.0));
    assert_eq!(energy[0].stack, Some(false));
    let overview = panels("overview", &selection, None).unwrap();
    assert_eq!(overview.len(), 8);
    assert_eq!(overview[1].stack, Some(true));
    let memory = panels("memory", &selection, None).unwrap();
    assert_eq!(memory.len(), 5);
    assert_eq!(memory[0].stack, Some(false));
    assert_eq!(memory[0].lines[0].values[1], Some(1.0));
    assert_eq!(memory[0].lines[1].values[1], None);
    assert_eq!(memory[2].lines[1].dashed, Some(true));
}

#[test]
fn overview_includes_gpu_activity_history_with_independent_missing_observations() {
    // The default history must expose source GPU activity and preserve its missing observations.
    let mut selection = selection();
    selection["trends"][1]["gpu_active_percent"] = json!(37.5);

    let overview = panels("overview", &selection, None).unwrap();

    let activity = overview
        .iter()
        .find(|panel| panel.id == "live-gpu-activity")
        .unwrap();
    assert_eq!(activity.lines[0].values, [None, Some(37.5), None]);
    assert_eq!(activity.selected_index, Some(2));
    assert_eq!(activity.unit, "%");
}

#[test]
fn empty_or_unavailable_observations_never_fabricate_cpu_activity() {
    // Goal: no-data and unavailable-counter states remain distinguishable from zero.
    // Method: project empty and null-only payloads, then reject an oversized context.
    assert!(
        panels("cpu", &json!({"trends": []}), None)
            .unwrap()
            .is_empty()
    );
    let mut selection = selection();
    selection["trends"][1]["cpu_percent"] = Value::Null;
    let cpu = panels("cpu", &selection, None).unwrap();
    assert!(cpu[0].lines[0].values.iter().all(Option::is_none));
    selection["trends"] = json!(vec![selection["trends"][0].clone(); OBSERVATIONS_MAX + 1]);
    assert!(panels("cpu", &selection, None).is_err());
}

#[test]
fn quantities_follow_binary_memory_and_adaptive_power_units() {
    // Goal: scalar readouts use the same source units as Swift and preserve missing data.
    // Method: format numeric, string, zero and unavailable values.
    let values = json!({"memory": "536870912", "power": 0.25, "zero": 0, "missing": null});
    assert_eq!(quantity(&values, "memory", "GiB", GIBIBYTE), "512.0 MiB");
    assert_eq!(quantity(&values, "power", "W", 1.0), "250.0 mW");
    assert_eq!(quantity(&values, "zero", "W", 1.0), "0.0 W");
    assert_eq!(quantity(&values, "missing", "%", 1.0), "Unavailable");
    assert_eq!(quantity(&values, "absent", "%", 1.0), "Unavailable");
    assert_eq!(quantity(&values, "power", "W", 0.0), "Unavailable");
}

#[test]
fn history_projection_reads_decimal_stamps_and_partial_measurements() {
    // Goal: App histories accept exact decimal identity words and refuse unknown totals.
    // Method: project measured and unavailable memory gauges with no counter-rate baseline.
    let observation = json!({"wall_ns": "1000750000000", "stamp": {
        "run": ["1", "2"], "boot": ["3", "4"], "publication": "3"}});
    let history = json!({"points": [
        {"observation": observation, "segment": "1", "cpu_percent_one_core": null,
         "resident_bytes": {"known": "536870912", "measured_members": 1,
            "unavailable_members": 2, "overflowed": false},
         "footprint_bytes": {"known": "0", "measured_members": 0,
            "unavailable_members": 3, "overflowed": false}, "energy_watts": "0.25"}
    ]});
    let apps = panels("apps", &selection(), Some(&history)).unwrap();
    assert_eq!(apps.len(), 4);
    assert_eq!(apps[0].lines[0].values, [None]);
    assert_eq!(apps[1].lines[0].values, [Some(0.5)]);
    assert_eq!(apps[2].lines[0].values, [None]);
    assert_eq!(apps[3].lines[0].values, [Some(0.25)]);
}

#[test]
fn clicks_use_the_exact_nine_hundred_second_domain() {
    // Goal: selecting a chart second respects the elapsed window including its endpoints.
    // Method: probe both endpoints, its midpoint, a short initial window and invalid inputs.
    assert_eq!(sample_second(&selection(), 0.0), Some(101));
    assert_eq!(sample_second(&selection(), 0.5), Some(551));
    assert_eq!(sample_second(&selection(), 1.0), Some(1000));
    assert_eq!(sample_second(&json!({"second": 4}), 0.5), Some(2));
    assert_eq!(sample_second(&selection(), f32::NAN), None);
    assert_eq!(sample_second(&selection(), -0.1), None);
    assert_eq!(sample_second(&Value::Null, 0.5), None);
}

#[test]
fn missing_selected_second_keeps_context_but_has_no_current_readout() {
    // Goal: a recording gap retains neighboring history without borrowing its latest value.
    // Method: clear the selection and summary while keeping the backend's actual trend points.
    let mut selection = selection();
    selection["selected"] = Value::Null;
    selection["summary"] = Value::Null;
    let cpu = panels("cpu", &selection, None).unwrap();
    assert_eq!(cpu[0].lines[0].values, [None, Some(25.0), None]);
    assert_eq!(cpu[0].selected_index, None);
    assert_eq!(
        quantity(&selection["summary"], "cpu_percent", "%", 1.0),
        "Unavailable"
    );
}

#[test]
fn invalid_clocks_and_nonfinite_values_are_reported_instead_of_plotted() {
    // Goal: corrupt input cannot place samples outside the plot or create nonfinite geometry.
    // Method: move an observation beyond the domain, then substitute a nonfinite string reading.
    let mut selection = selection();
    selection["trends"][0]["wall_ns"] = json!(100_000_000_000_u64);
    assert!(panels("cpu", &selection, None).is_err());
    selection["trends"][0]["wall_ns"] = json!(101_000_000_000_u64);
    selection["trends"][0]["cpu_percent"] = json!("NaN");
    assert!(panels("cpu", &selection, None).is_err());
}

#[test]
fn cursor_identifies_the_exact_selected_alternative_instead_of_the_latest_point() {
    // Goal: paused same-second alternatives keep their own cursor and legend reading.
    // Method: select an earlier observation, then an absent identity, without changing trends.
    let mut selection = selection();
    selection["trends"][1]["wall_ns"] = json!(1_000_500_000_000_u64);
    selection["selected"] = selection["trends"][1].clone();
    let cpu = panels("cpu", &selection, None).unwrap();
    assert_eq!(cpu[0].selected_index, Some(1));
    selection["selected"]["publication"] = json!(9);
    let cpu = panels("cpu", &selection, None).unwrap();
    assert_eq!(cpu[0].selected_index, None);
}

#[test]
fn app_cursor_matches_decimal_stamp_identity_to_its_selected_observation() {
    // Goal: App history uses its own exact selected stamp instead of the last retained point.
    // Method: select the first of two observations with different publications.
    let first = json!({"wall_ns": "1000000000000", "stamp": {
        "run": ["1", "2"], "boot": ["3", "4"], "publication": "1"}});
    let second = json!({"wall_ns": "1000500000000", "stamp": {
        "run": ["1", "2"], "boot": ["3", "4"], "publication": "2"}});
    let history = json!({"selected": first, "points": [
        {"observation": first, "segment": "1", "cpu_percent_one_core": 10},
        {"observation": second, "segment": "1", "cpu_percent_one_core": 20}
    ]});
    let apps = panels("apps", &selection(), Some(&history)).unwrap();
    assert_eq!(apps[0].selected_index, Some(0));
}

#[test]
fn uppercase_hexadecimal_identities_match_selected_points() {
    // Goal: hexadecimal case cannot move the cursor or merge distinct unsigned identities.
    // Method: project matching upper/lowercase runs beside one differing in its highest bit.
    let mut selection = selection();
    selection["trends"][0]["run"] = json!("7fffffffffffffffabcdef0123456789");
    selection["trends"][1]["run"] = json!("FFFFFFFFFFFFFFFFABCDEF0123456789");
    selection["trends"][2]["run"] = json!("ffffffffffffffffabcdef0123456789");
    selection["selected"]["run"] = json!("FFFFFFFFFFFFFFFFABCDEF0123456789");

    let cpu = panels("cpu", &selection, None).unwrap();

    assert_eq!(cpu[0].selected_index, Some(2));
    assert_eq!(cpu[0].segments.as_ref().unwrap(), &[0, 1, 1]);
}

#[test]
fn unsigned_decimal_identities_preserve_distinct_segments() {
    // Goal: exact unsigned App stamps retain their selected point and distinct collector runs.
    // Method: project a maximum decimal word with numeric low words differing by one.
    let first = json!({"wall_ns": "1000000000000", "stamp": {
        "run": ["18446744073709551615", 3], "boot": [3, 4], "publication": "1"}});
    let second = json!({"wall_ns": "1000500000000", "stamp": {
        "run": ["18446744073709551615", 4], "boot": [3, 4], "publication": "2"}});
    let history = json!({"selected": {"run": "FFFFFFFFFFFFFFFF0000000000000003",
        "boot": "00000000000000030000000000000004", "publication": 1}, "points": [
        {"observation": first, "segment": "1", "cpu_percent_one_core": 10},
        {"observation": second, "segment": "1", "cpu_percent_one_core": 20}]});

    let apps = panels("apps", &selection(), Some(&history)).unwrap();

    assert_eq!(apps[0].selected_index, Some(0));
    assert_eq!(apps[0].segments.as_ref().unwrap(), &[0, 1]);
}

#[test]
fn mixed_identity_encodings_keep_the_selected_point_and_segment() {
    // Goal: representation and hexadecimal case cannot split a collector run or move its cursor.
    // Method: select a hexadecimal identity matching an App's decimal words.
    let observation = json!({"wall_ns": "1000000000000", "stamp": {
        "run": ["18446744073709551615", "12379813738877118345"],
        "boot": ["3", "4"], "publication": "1"}});
    let history = json!({"selected": {"run": "FFFFFFFFFFFFFFFFABCDEF0123456789",
        "boot": "00000000000000030000000000000004", "publication": 1},
        "points": [{"observation": observation, "segment": "1",
            "cpu_percent_one_core": "-0.0"}]});

    let apps = panels("apps", &selection(), Some(&history)).unwrap();

    assert_eq!(apps[0].selected_index, Some(0));
    assert_eq!(apps[0].segments.as_ref().unwrap(), &[0]);
    assert_eq!(
        apps[0].lines[0].values[0].unwrap().to_bits(),
        (-0.0_f32).to_bits()
    );
}

#[test]
fn negative_run_identity_words_reject_the_projection() {
    // Goal: a negative word cannot wrap into a valid unsigned observation identity.
    // Method: put a negative decimal run word in an actual scalar point.
    let mut selection = selection();
    selection["trends"][1]["run"] = json!(["-1", "2"]);

    assert!(panels("cpu", &selection, None).is_err());
}

#[test]
fn overflowing_boot_identity_words_reject_the_projection() {
    // Goal: values beyond a fixed unsigned word cannot alias an existing identity.
    // Method: exceed u64 by one in an actual scalar point's decimal boot identity.
    let mut selection = selection();
    selection["trends"][1]["boot"] = json!(["18446744073709551616", "2"]);

    assert!(panels("cpu", &selection, None).is_err());
}

#[test]
fn non_ascii_run_identities_reject_the_projection() {
    // Goal: malformed run identities produce a projection error rather than a panic.
    // Method: put a 32-byte non-ASCII run crossing its midpoint in an actual scalar point.
    let mut selection = selection();
    selection["trends"][1]["run"] = json!("000000000000000é000000000000000");

    assert!(panels("cpu", &selection, None).is_err());
}

#[test]
fn energy_exposes_temperature_and_channel_indexed_fan_history() {
    // Goal: Energy retains source channel identities, stopped zero and independent missingness.
    // Method: project three ticks with partial, unavailable and absent fan channel arrays.
    let mut selection = selection();
    selection["trends"][0]["fan_rpm"] = json!([0.0, null]);
    selection["trends"][1]["fan_rpm"] = Value::Null;
    selection["trends"][2]["fan_rpm"] = json!([1200.0]);
    selection["trends"][1]["soc_celsius"] = json!(42.5);

    let energy = panels("energy", &selection, None).unwrap();

    assert_eq!(energy.len(), 3);
    assert_eq!(energy[1].title, "SoC temperature");
    assert_eq!(energy[1].lines[0].values, [None, Some(42.5), None]);
    assert_eq!(energy[2].unit, "RPM");
    assert_eq!(energy[2].lines[0].name, "Fan 0");
    assert_eq!(energy[2].lines[0].values, [Some(0.0), None, Some(1200.0)]);
    assert_eq!(energy[2].lines[1].name, "Fan 1");
    assert_eq!(energy[2].lines[1].values, [None, None, None]);
}

#[test]
fn energy_rejects_fan_arrays_beyond_the_recording_bound() {
    // Goal: an oversized channel array cannot create unbounded series or lose channels silently.
    // Method: supply eleven channels where the recording permits at most ten.
    let mut selection = selection();
    selection["trends"][1]["fan_rpm"] = json!(vec![0; 11]);

    assert!(panels("energy", &selection, None).is_err());
}

#[test]
fn memory_exposes_mirrored_swap_flow_without_negating_quantities() {
    // Goal: disk swap traffic retains direction and independent missingness.
    // Method: compare source bytes converted to MiB/s and outgoing plot metadata.
    let mut selection = selection();
    selection["trends"][1]["swap_in_bytes_per_second"] = json!(1_048_576);
    selection["trends"][1]["swap_out_bytes_per_second"] = json!(2_097_152);

    let memory = panels("memory", &selection, None).unwrap();
    let swap = memory
        .iter()
        .find(|panel| panel.id == "live-swap-flow")
        .unwrap();

    assert_eq!(swap.unit, "MiB/s");
    assert_eq!(swap.lines[0].values, [None, Some(1.0), None]);
    assert_eq!(swap.lines[1].values, [None, Some(2.0), None]);
    assert_eq!(swap.lines[0].negative, Some(false));
    assert_eq!(swap.lines[1].negative, Some(true));
}

#[test]
fn pressure_timeline_uses_exact_selected_alternatives_and_blanks_unknown_states() {
    // Goal: repeated seconds retain the chosen state while unobserved and future codes stay blank.
    // Method: select the earlier of two same-second observations and add an unknown state.
    let mut selection = selection();
    selection["trends"][0]["pressure"] = json!(8);
    selection["trends"][1]["wall_ns"] = json!(1_000_100_000_000_u64);
    selection["trends"][1]["pressure"] = json!(1);
    selection["trends"][2]["pressure"] = json!(2);
    selection["selected"] = selection["trends"][1].clone();

    let memory = panels("memory", &selection, None).unwrap();
    let pressure = memory
        .iter()
        .find(|panel| panel.id == "live-memory-pressure")
        .unwrap();

    assert_eq!(pressure.kind, "timeline");
    assert_eq!(pressure.lines[0].values.len(), 900);
    assert!(pressure.lines[0].values[..899].iter().all(Option::is_none));
    assert_eq!(pressure.lines[0].values[899], Some(1.0));
    assert_eq!(pressure.selected_index, Some(899));
    assert_eq!(
        pressure.states.as_ref().unwrap(),
        &["Normal", "Warning", "Critical"]
    );
    assert_eq!(
        pressure.help.as_deref(),
        Some(
            "macOS memory pressure by observed second. The chosen same-second \
            publication takes priority; otherwise the latest reading is shown. Blank means \
            unavailable or unrecognized, including gaps."
        )
    );
}

#[test]
fn inspecting_another_repeated_second_selects_its_latest_observation() {
    // Goal: clicking a pressure cell selects the same latest observation that its state shows.
    // Method: retain two ordered observations in a second other than the selected gap.
    let mut selection = selection();
    selection["selected"] = Value::Null;
    selection["trends"][1]["wall_ns"] = json!(999_100_000_000_u64);
    selection["trends"][2]["wall_ns"] = json!(999_750_000_000_u64);

    let identity = inspection_identity(&selection, 999).unwrap();

    assert_eq!(identity["publication"], 3);
}
