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
         "network_rx_bps": 2_000_000, "network_tx_bps": 1_000_000,
         "memory_active_bytes": 1_073_741_824_u64,
         "memory_anonymous_bytes": 1_610_612_736_u64,
         "memory_purgeable_bytes": 536_870_912_u64},
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
    // Goal: directional traffic follows mirrored plots while values stay positive.
    // Method: compare traffic, packets and error line metadata and byte conversion.
    let panels = panels("network", &selection(), None).unwrap();
    assert_eq!(panels.len(), 3);
    assert_eq!(panels[0].unit, "MB/s");
    assert_eq!(panels[0].lines[0].values[1], Some(2.0));
    assert_eq!(panels[0].lines[1].values[1], Some(1.0));
    assert_eq!(panels[0].lines[1].negative, Some(true));
    assert_eq!(panels[1].lines[1].negative, Some(true));
    assert_eq!(panels[1].unit, "pps");
    assert_eq!(panels[1].lines[1].dashed, Some(false));
    assert_eq!(panels[2].lines[1].negative, Some(false));
}

#[test]
fn overview_network_combines_positive_readouts_and_mirrors_only_send_geometry() {
    // Project the primary network panel with measured traffic between unavailable observations.
    let projected = panels("overview", &selection(), None).unwrap();
    let network = &projected[5];

    assert_eq!(network.id, "live-network");
    assert_eq!(network.unit, "MB/s");
    assert_eq!(network.lines[0].name, "Receive");
    assert_eq!(network.lines[0].values, [None, Some(2.0), None]);
    assert_eq!(network.lines[0].negative, Some(false));
    assert_eq!(network.lines[1].name, "Send");
    assert_eq!(network.lines[1].values, [None, Some(1.0), None]);
    assert_eq!(network.lines[1].negative, Some(true));
}

#[test]
fn overview_stacks_captured_memory_categories_below_an_independent_installed_guide() {
    // Project measured capacity, zero and missing values without adding capacity to the stack.
    let selection = memory_composition_selection();

    let overview = panels("overview", &selection, None).unwrap();

    assert_eq!(overview.len(), 8);
    assert_memory_composition(&overview[2]);
}

#[test]
fn memory_stacks_captured_categories_below_an_independent_installed_guide() {
    // Project the Memory route with the same category and capacity contract as Overview.
    let selection = memory_composition_selection();

    let memory = panels("memory", &selection, None).unwrap();

    assert_eq!(memory.len(), 5);
    assert_memory_composition(&memory[0]);
    assert_eq!(memory[2].lines[1].dashed, Some(true));
}

fn memory_composition_selection() -> Value {
    let mut selection = selection();
    selection["trends"][0]["memory_total_bytes"] = json!(0);
    selection["trends"][0]["memory_active_bytes"] = json!(0);
    selection["trends"][0]["memory_anonymous_bytes"] = json!(0);
    selection["trends"][0]["memory_purgeable_bytes"] = json!(0);
    selection["trends"][0]["memory_wired_bytes"] = json!(0);
    selection["trends"][0]["memory_compressed_bytes"] = json!(0);
    selection["trends"][1]["memory_total_bytes"] = json!(51_539_607_552_u64);
    selection["trends"][1]["memory_wired_bytes"] = json!(2_147_483_648_u64);
    selection["trends"][1]["memory_compressed_bytes"] = json!(3_221_225_472_u64);

    selection
}

fn assert_memory_composition(memory: &Panel) {
    assert_eq!(memory.unit, "GiB");
    assert_eq!(memory.stack, Some(true));
    assert_eq!(memory.fill, Some(0.45));
    assert_eq!(
        memory
            .lines
            .iter()
            .map(|line| line.name.as_str())
            .collect::<Vec<_>>(),
        ["App memory", "Wired", "Compressed", "Installed"]
    );
    assert_eq!(memory.lines[0].values, [Some(0.0), Some(1.0), None]);
    assert_eq!(memory.lines[1].values, [Some(0.0), Some(2.0), None]);
    assert_eq!(memory.lines[2].values, [Some(0.0), Some(3.0), None]);
    assert_ne!(memory.lines[0].stack, Some(false));
    assert_ne!(memory.lines[1].stack, Some(false));
    assert_ne!(memory.lines[2].stack, Some(false));
    assert_eq!(memory.lines[3].values, [Some(0.0), Some(48.0), None]);
    assert_eq!(memory.lines[3].unit.as_deref(), Some("GiB"));
    assert_eq!(memory.lines[3].stack, Some(false));
    assert_eq!(memory.lines[3].dashed, Some(true));
    assert_eq!(memory.lines[3].fill, Some(0.0));
}

#[test]
fn memory_keeps_all_page_rates_and_unavailable_intervals() {
    // Project measured page/compression rates beside absent and reset-produced null rates.
    let mut selection = selection();
    selection["trends"][1]["memory_pageins_per_second"] = json!(1.0);
    selection["trends"][1]["memory_pageouts_per_second"] = json!(4.0);
    selection["trends"][1]["memory_compressions_per_second"] = json!(2.0);
    selection["trends"][1]["memory_decompressions_per_second"] = json!(3.0);
    selection["trends"][2]["memory_pageins_per_second"] = json!(0.0);
    selection["trends"][2]["memory_pageouts_per_second"] = json!(0.0);
    selection["trends"][2]["memory_compressions_per_second"] = json!(0.0);
    selection["trends"][2]["memory_decompressions_per_second"] = json!(0.0);
    let mut reset = selection["trends"][2].clone();
    reset["publication"] = json!(4);
    reset["memory_pageins_per_second"] = Value::Null;
    reset["memory_pageouts_per_second"] = Value::Null;
    reset["memory_compressions_per_second"] = Value::Null;
    reset["memory_decompressions_per_second"] = Value::Null;
    selection["trends"].as_array_mut().unwrap().push(reset);

    let memory = panels("memory", &selection, None).unwrap();
    let paging = memory
        .iter()
        .find(|panel| panel.id == "live-paging")
        .unwrap();

    assert_eq!(paging.lines.len(), 4);
    assert_eq!(paging.lines[0].name, "Page-ins");
    assert_eq!(paging.lines[0].unit.as_deref(), Some("pages/s"));
    assert_eq!(paging.lines[0].values, [None, Some(1.0), Some(0.0), None]);
    assert_eq!(paging.lines[1].name, "Page-outs");
    assert_eq!(paging.lines[1].unit.as_deref(), Some("pages/s"));
    assert_eq!(paging.lines[1].values, [None, Some(4.0), Some(0.0), None]);
    assert_eq!(paging.lines[2].name, "Compressed");
    assert_eq!(paging.lines[2].unit.as_deref(), Some("pages/s"));
    assert_eq!(paging.lines[2].values, [None, Some(2.0), Some(0.0), None]);
    assert_eq!(paging.lines[3].name, "Decompressed");
    assert_eq!(paging.lines[3].unit.as_deref(), Some("pages/s"));
    assert_eq!(paging.lines[3].values, [None, Some(3.0), Some(0.0), None]);
}

#[test]
fn energy_power_panels_preserve_independent_sources_and_missing_intervals() {
    // Separate captured system and GPU power while preserving each source's measured zero and gaps.
    let mut selection = selection();
    selection["trends"][0]["gpu_watts"] = json!(0.0);
    selection["trends"][1]["gpu_watts"] = Value::Null;
    selection["trends"][2]["gpu_watts"] = json!(2.5);

    let energy = panels("energy", &selection, None).unwrap();

    assert_eq!(
        energy
            .iter()
            .map(|panel| panel.id.as_str())
            .collect::<Vec<_>>(),
        [
            "live-energy",
            "live-gpu-power",
            "live-power-breakdown",
            "live-soc-temperature",
            "live-fans"
        ]
    );
    assert_eq!(energy[0].title, "System power");
    assert_eq!(energy[1].title, "GPU power");
    assert_eq!(energy[0].lines.len(), 1);
    assert_eq!(energy[1].lines.len(), 1);
    assert_eq!(energy[0].lines[0].values, [None, Some(8.0), None]);
    assert_eq!(energy[1].lines[0].values, [Some(0.0), None, Some(2.5)]);
    assert_eq!(energy[0].unit, "W");
    assert_eq!(energy[1].unit, "W");
    assert_eq!(energy[0].stack, Some(false));
    assert_eq!(energy[1].stack, Some(false));
    assert_eq!(energy[0].positions, energy[1].positions);
    assert_eq!(energy[0].segments, energy[1].segments);
    assert_eq!(energy[0].selected_index, energy[1].selected_index);
}

#[test]
fn energy_breakdown_keeps_measured_zero_and_negative_remainders_without_clamping() {
    // Subtract only aligned readings, including a GPU interval above the system sensor.
    let mut selection = selection();
    for (point, (system, gpu)) in selection["trends"].as_array_mut().unwrap().iter_mut().zip([
        (8.0, 2.0),
        (0.0, 0.0),
        (2.0, 5.0),
    ]) {
        point["power_watts"] = json!(system);
        point["gpu_watts"] = json!(gpu);
    }

    let projected = panels("energy", &selection, None).unwrap();
    let power = projected
        .iter()
        .find(|panel| panel.id == "live-power-breakdown")
        .unwrap();

    assert_eq!(power.title, "Power");
    assert_eq!(power.unit, "W");
    assert_eq!(power.stack, Some(true));
    assert_eq!(power.lines.len(), 2);
    assert_eq!(power.lines[0].name, "SoC and rest of system");
    assert_eq!(power.lines[0].values, [Some(6.0), Some(0.0), Some(-3.0)]);
    assert_eq!(power.lines[1].name, "GPU");
    assert_eq!(power.lines[1].values, [Some(2.0), Some(0.0), Some(5.0)]);
    assert_eq!(power.selected_index, Some(2));
    assert_eq!(power.positions, projected[0].positions);
    assert_eq!(power.segments, projected[0].segments);
    assert!(
        power
            .help
            .as_ref()
            .unwrap()
            .contains("not independently measured")
    );
}

#[test]
fn energy_breakdown_requires_both_sources_for_the_remainder_and_keeps_gpu_independent() {
    // Missing system or GPU readings must not be replaced by a measured zero remainder.
    let mut selection = selection();
    selection["trends"][0]["gpu_watts"] = json!(2.0);
    selection["trends"][2]["power_watts"] = json!(4.0);

    let projected = panels("energy", &selection, None).unwrap();
    let power = projected
        .iter()
        .find(|panel| panel.id == "live-power-breakdown")
        .unwrap();

    assert_eq!(power.lines[0].values, [None, Some(6.0), None]);
    assert_eq!(power.lines[1].values, [Some(2.0), Some(2.0), None]);
}

#[test]
fn energy_breakdown_rejects_a_nonfinite_derived_remainder() {
    // Finite inputs may still overflow their difference; never send infinity to the renderer.
    let mut selection = selection();
    selection["trends"][1]["power_watts"] = json!(3.0e38);
    selection["trends"][1]["gpu_watts"] = json!(-3.0e38);

    match panels("energy", &selection, None) {
        Err(error) => assert!(error.contains("power remainder")),
        Ok(_) => panic!("The derived power remainder must reject overflow"),
    }
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
fn overview_pressure_history_keeps_normal_warning_critical_and_missing_seconds() {
    // Project sparse pressure observations and compare every retained second, including gaps.
    let mut selection = selection();
    selection["trends"][0]["pressure"] = json!(0);
    selection["trends"][1]["pressure"] = json!(1);
    selection["trends"][2]["pressure"] = json!(2);
    let mut expected = vec![None; 900];
    expected[0] = Some(0.0);
    expected[449] = Some(1.0);
    expected[899] = Some(2.0);

    let overview = panels("overview", &selection, None).unwrap();
    let pressure = overview
        .iter()
        .find(|panel| panel.id == "live-memory-pressure")
        .unwrap();

    assert_eq!(pressure.kind, "timeline");
    assert_eq!(pressure.lines[0].values, expected);
    assert_eq!(pressure.selected_index, Some(899));
    assert_eq!(
        pressure.states.as_ref().unwrap(),
        &["Normal", "Warning", "Critical"]
    );
}

#[test]
fn overview_pressure_history_retains_the_selected_same_second_alternative() {
    // Select the earlier same-second warning and keep an unknown pressure observation blank.
    let mut selection = selection();
    selection["trends"][0]["pressure"] = json!(8);
    selection["trends"][1]["wall_ns"] = json!(1_000_100_000_000_u64);
    selection["trends"][1]["pressure"] = json!(1);
    selection["trends"][2]["pressure"] = json!(2);
    selection["selected"] = selection["trends"][1].clone();
    let mut expected = vec![None; 900];
    expected[899] = Some(1.0);

    let overview = panels("overview", &selection, None).unwrap();
    let pressure = overview
        .iter()
        .find(|panel| panel.id == "live-memory-pressure")
        .unwrap();

    assert_eq!(pressure.lines[0].values, expected);
    assert_eq!(pressure.selected_index, Some(899));
    assert_eq!(
        inspection_identity(&selection, 1000),
        Some(selection["selected"].clone())
    );
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
    // Goal: scalar readouts use the recorded source units and preserve missing data.
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
    assert_eq!(apps.len(), 5);
    assert_eq!(apps[0].lines[0].values, [None]);
    assert_eq!(apps[1].lines[0].values, [Some(0.5)]);
    assert_eq!(apps[2].lines[0].values, [None]);
    assert_eq!(apps[3].lines[0].values, [Some(0.25)]);
    assert_eq!(apps[4].lines[0].values, [None]);
    assert_eq!(apps[4].lines[1].values, [None]);
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

    assert_eq!(energy.len(), 5);
    let temperature = energy
        .iter()
        .find(|panel| panel.id == "live-soc-temperature")
        .unwrap();
    let fans = energy.iter().find(|panel| panel.id == "live-fans").unwrap();
    assert_eq!(temperature.title, "SoC temperature");
    assert_eq!(temperature.lines[0].values, [None, Some(42.5), None]);
    assert_eq!(fans.unit, "RPM");
    assert_eq!(fans.lines[0].name, "Fan 0");
    assert_eq!(fans.lines[0].values, [Some(0.0), None, Some(1200.0)]);
    assert_eq!(fans.lines[1].name, "Fan 1");
    assert_eq!(fans.lines[1].values, [None, None, None]);
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
    // Goal: disk swap traffic retains direction and independent missingness, named and
    // scaled as the prototype's Paging chart.
    // Method: compare source bytes converted to MB/s and outgoing plot metadata.
    let mut selection = selection();
    selection["trends"][1]["swap_in_bytes_per_second"] = json!(1_000_000);
    selection["trends"][1]["swap_out_bytes_per_second"] = json!(2_000_000);

    let memory = panels("memory", &selection, None).unwrap();
    let swap = memory
        .iter()
        .find(|panel| panel.id == "live-swap-flow")
        .unwrap();

    assert_eq!(swap.title, "Paging");
    assert_eq!(swap.unit, "MB/s");
    assert_eq!(swap.lines[0].name, "Read from swap");
    assert_eq!(swap.lines[1].name, "Write to swap");
    assert_eq!(swap.lines[0].values, [None, Some(1.0), None]);
    assert_eq!(swap.lines[1].values, [None, Some(2.0), None]);
    assert_eq!(swap.lines[0].negative, Some(false));
    assert_eq!(swap.lines[1].negative, Some(true));
}

#[test]
fn memory_draws_memory_in_use_and_page_ins_as_the_prototype_names_them() {
    // Goal: the Memory page's charts carry the prototype's titles and solid series lines.
    // Method: project the memory route and read each chart's title and series.
    let memory = panels("memory", &selection(), None).unwrap();
    let find = |id: &str| memory.iter().find(|panel| panel.id == id).unwrap();

    let composition = find("live-memory");
    let paging = find("live-paging");

    assert_eq!(composition.title, "Memory in use");
    assert_eq!(paging.title, "Page-ins and compression");
    let names: Vec<_> = paging.lines.iter().map(|line| line.name.as_str()).collect();
    assert_eq!(
        names,
        ["Page-ins", "Page-outs", "Compressed", "Decompressed"]
    );
    assert!(paging.lines.iter().all(|line| line.dashed != Some(true)));
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

#[test]
fn cpu_charts_scheduling_and_open_files_only_where_sampled() {
    // Goal: the load average and open files charts read the app's samples, and a point the
    // app did not sample stays a gap rather than a zero.
    // Method: sample the middle point of three and project the CPU page.
    let mut selection = selection();
    let point = &mut selection["trends"][1];
    point["load_average"] = json!([2.5, 3.0, 3.5]);
    point["logical_cpu_count"] = json!(18);
    point["open_files"] = json!(12_000);
    point["open_files_percent"] = json!(3.0);

    let panels = panels("cpu", &selection, None).unwrap();

    let load = panels
        .iter()
        .find(|panel| panel.id == LOAD_AVERAGE_ID)
        .unwrap();
    let names: Vec<_> = load.lines.iter().map(|line| line.name.as_str()).collect();
    assert_eq!(names, ["1 minute", "5 minutes", "15 minutes", "Cores"]);
    assert_eq!(load.lines[2].values, [None, Some(3.5), None]);
    assert_eq!(load.lines[3].dashed, Some(true));
    let files = panels
        .iter()
        .find(|panel| panel.id == OPEN_FILES_ID)
        .unwrap();
    assert_eq!(files.lines[0].values, [None, Some(12_000.0), None]);
    assert_eq!(files.lines[1].axis.as_deref(), Some("right1"));
    assert_eq!(quantity_text(12_000.0, "files"), "12.0 k files");
}

#[test]
fn cpu_frequency_charts_each_level_where_measured() {
    // Goal: the CPU active frequency chart draws each core level's clock from its own channel,
    // and an interval with no clock reading stays a gap.
    // Method: measure the middle point of three, the slower level at 2.2 GHz and the faster
    // at 4.1 GHz, and project the CPU page of this Mac.
    let mut selection = selection();
    selection["trends"][1]["cpu_level_ghz"] = json!([4.1, 2.2]);

    let panels = panels("cpu", &selection, None).unwrap();

    let frequency = panels
        .iter()
        .find(|panel| panel.id == FREQUENCY_ID)
        .unwrap();
    let values: Vec<_> = frequency.lines.iter().map(|line| &line.values).collect();
    assert_eq!(values, [&[None, Some(2.2), None], &[None, Some(4.1), None]]);
    assert_eq!(quantity_text(2.2, "GHz"), "2.2 GHz");
}

#[test]
fn gpu_memory_charts_in_use_and_dashed_allocation_in_gibibytes() {
    // Goal: the GPU memory chart draws the driver's memory in use solid and its allocation
    // dashed without fill, in GiB, and a second the app did not sample stays a gap.
    // Method: sample only the middle point of three, 1 GiB in use of 6 GiB allocated.
    let mut selection = selection();
    selection["trends"][1]["gpu_memory_in_use_bytes"] = json!(1_u64 << 30);
    selection["trends"][1]["gpu_memory_allocated_bytes"] = json!(6_u64 << 30);

    let panels = panels("gpu", &selection, None).unwrap();

    let memory = panels
        .iter()
        .find(|panel| panel.id == GPU_MEMORY_ID)
        .unwrap();
    let names: Vec<_> = memory.lines.iter().map(|line| line.name.as_str()).collect();
    assert_eq!(names, ["In use", "Allocated"]);
    let values: Vec<_> = memory.lines.iter().map(|line| &line.values).collect();
    assert_eq!(values, [&[None, Some(1.0), None], &[None, Some(6.0), None]]);
    assert_eq!(memory.lines[0].dashed, Some(false));
    assert_eq!(memory.lines[1].dashed, Some(true));
    assert_eq!(memory.unit, "GiB");
}
