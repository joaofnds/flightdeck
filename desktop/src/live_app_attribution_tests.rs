use serde_json::{Value, json};

use super::{AppAttributionHistory, Metric};
use crate::live_hover::Target;

#[test]
fn cpu_rates_preserve_multicore_values_and_captured_positions() {
    // Decode captured intervals through the public model and inspect the renderer's panel.
    let history = AppAttributionHistory::prepare(Metric::Cpu, &fixture(Metric::Cpu), &selection());
    let panel = history.panel().unwrap();

    assert_eq!(panel.id, "live-cpu-apps");
    assert_eq!(panel.lines[1].values, vec![Some(120.0), Some(240.0)]);
    assert_eq!(panel.maximum, None);
    assert_eq!(panel.stack, Some(true));
    assert_eq!(history.reading(1, Some(0)), "120.00 %");
    assert_eq!(history.headline(Some(1)), "250.00 % captured");
    assert!(panel.positions.as_ref().unwrap()[0] < panel.positions.as_ref().unwrap()[1]);
}

#[test]
fn power_rates_preserve_fractional_watts() {
    // Keep the captured process interval's fractional power separate from machine power.
    let history =
        AppAttributionHistory::prepare(Metric::Power, &fixture(Metric::Power), &selection());

    assert_eq!(history.reading(1, Some(0)), "1.25 W");
    assert_eq!(history.panel().unwrap().lines[1].values[1], Some(2.5));
    assert_eq!(history.headline(Some(1)), "2.75 W captured");
}

#[test]
fn measured_zero_and_partial_rates_retain_their_coverage() {
    // A measured zero is drawable while an incomplete measure carries an explicit subtotal label.
    let mut value = fixture(Metric::Cpu);
    value["points"][0]["apps"][0]["cpu_percent_one_core"] = json!(0.0);
    value["points"][0]["apps"][0]["cpu_ns"]["known"] = json!("0");
    value["points"][1]["apps"][0]["cpu_ns"]["unavailable_members"] = json!(2);
    let history = AppAttributionHistory::prepare(Metric::Cpu, &value, &selection());

    assert_eq!(history.panel().unwrap().lines[1].values[0], Some(0.0));
    assert_eq!(history.reading(1, Some(0)), "0.00 %");
    assert_eq!(
        history.reading(1, Some(1)),
        "240.00 % known subtotal · 1 measured · 2 unavailable"
    );
    assert_eq!(history.headline(Some(1)), "250.00 % known subtotal");
}

#[test]
fn unmeasured_members_remain_unavailable_instead_of_zero() {
    // Preserve a failed measure and its member metadata even though no rate can be drawn.
    let mut value = fixture(Metric::Power);
    value["points"][1]["apps"][0]["energy_watts"] = Value::Null;
    value["points"][1]["apps"][0]["energy_nj"]["measured_members"] = json!(0);
    value["points"][1]["apps"][0]["energy_nj"]["unavailable_members"] = json!(1);
    let history = AppAttributionHistory::prepare(Metric::Power, &value, &selection());

    assert_eq!(history.panel().unwrap().lines[1].values[1], None);
    assert_eq!(history.reading(1, Some(1)), "Unavailable · 1 unavailable");
}

#[test]
fn missing_source_and_app_slots_remain_gaps() {
    // Source absence has no interval or coverage; a missing App slot stays distinct from zero.
    let mut value = fixture(Metric::Cpu);
    value["points"][0] = json!({"observation":observation(1),"segment":"1",
        "elapsed_ns":null,"source_available":false,"membership_complete":null,
        "ungrouped_members":null,"usage_refused":null,"usage_gone":null,
        "apps":[null],"other":null});
    value["points"][1]["apps"][0] = Value::Null;
    value["points"][1]["segment"] = json!("2");
    let history = AppAttributionHistory::prepare(Metric::Cpu, &value, &selection());

    assert_eq!(history.panel().unwrap().lines[1].values, vec![None, None]);
    assert_eq!(history.panel().unwrap().segments, Some(vec![1, 2]));
    assert_eq!(history.headline(Some(0)), "Unavailable");
    assert_eq!(history.reading(1, Some(1)), "Unavailable");
}

#[test]
fn empty_other_is_a_measured_zero_only_with_complete_capture() {
    // The backend may establish an empty set only from a complete source and a valid interval.
    let mut value = fixture(Metric::Cpu);
    value["points"][1]["other"] = json!({"app_count":0,"cpu_ns":{
        "known":"0","measured_members":0,"unavailable_members":0,"overflowed":false},
        "cpu_percent_one_core":0.0,"membership_complete":true});
    let history = AppAttributionHistory::prepare(Metric::Cpu, &value, &selection());

    assert_eq!(history.reading(0, Some(1)), "0.00 %");
    assert_eq!(history.panel().unwrap().lines[0].values[1], Some(0.0));
}

#[test]
fn same_second_siblings_and_boot_changes_keep_their_exact_identity() {
    // Two publications share a second, but lookups must never borrow the other boot's measure.
    let mut value = fixture(Metric::Cpu);
    value["points"][0]["observation"]["wall_ns"] = json!("1000000000000");
    value["points"][0]["observation"]["stamp"]["boot"] = json!(["2", "4"]);
    let history = AppAttributionHistory::prepare(Metric::Cpu, &value, &selection());
    let earlier = Target::exact(1000, &json!({"run":[1,2],"boot":[2,4],"publication":1})).unwrap();
    let selected = Target::exact(1000, &selection()["selected"]).unwrap();

    assert_eq!(history.index(Some(&earlier)), Some(0));
    assert_eq!(history.index(Some(&selected)), Some(1));
    assert_eq!(
        history.reading(1, history.index(Some(&earlier))),
        "120.00 %"
    );
}

#[test]
fn duplicate_basenames_use_full_app_keys_for_independent_labels() {
    // Distinct bundle paths with the same basename remain independent plotted series.
    let mut value = fixture(Metric::Cpu);
    value["cohort"] = json!([
        {"key":{"kind":"bundle","bytes_hex":"41","display":"/A/Foo.app"}},
        {"key":{"kind":"bundle","bytes_hex":"42","display":"/B/Foo.app"}}]);
    for point in value["points"].as_array_mut().unwrap() {
        let app = point["apps"][0].clone();
        point["apps"].as_array_mut().unwrap().push(app);
    }
    let panel = AppAttributionHistory::prepare(Metric::Cpu, &value, &selection())
        .panel()
        .unwrap();

    assert_eq!(panel.lines[1].name, "/A/Foo.app");
    assert_eq!(panel.lines[2].name, "/B/Foo.app");
}

#[test]
fn different_metric_is_a_local_unavailable_history() {
    // A valid Power envelope cannot silently become a CPU chart.
    let history =
        AppAttributionHistory::prepare(Metric::Cpu, &fixture(Metric::Power), &selection());

    assert!(history.panel().is_none());
}

#[test]
fn wrong_selected_identity_is_a_local_unavailable_history() {
    // The selected publication must match the worker's accepted observation.
    let mut selected = selection();
    selected["selected"]["publication"] = json!(3);

    let history = AppAttributionHistory::prepare(Metric::Cpu, &fixture(Metric::Cpu), &selected);

    assert!(history.panel().is_none());
}

#[test]
fn oversized_point_envelope_is_refused_before_projection() {
    // The renderer never receives a history exceeding its fixed point budget.
    let mut value = fixture(Metric::Cpu);
    value["points"] = json!(vec![value["points"][0].clone(); 4097]);

    let history = AppAttributionHistory::prepare(Metric::Cpu, &value, &selection());

    assert!(history.panel().is_none());
}

#[test]
fn oversized_source_cache_metadata_is_refused() {
    // A response cannot enlarge the 64 MiB source cache contract through metadata.
    let mut value = fixture(Metric::Cpu);
    value["coverage"]["cache_bytes"] = json!(67_108_865);

    let history = AppAttributionHistory::prepare(Metric::Cpu, &value, &selection());

    assert!(history.panel().is_none());
}

pub(crate) fn fixture(metric: Metric) -> Value {
    let mut value = json!({"metric":"cpu","selected":observation(2),"context_seconds":900,
        "cohort":[{"key":{"kind":"executable","bytes_hex":"41","display":"App A"}}],
        "points":[point(1),point(2)],"coverage":{"retention_limited":false,
        "context_app_count":2,"context_key_bytes":2,"cache_bytes":0}});
    if metric == Metric::Power {
        value["metric"] = json!("power");
        for (index, point) in value["points"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            let power = [1.25, 2.5][index];
            point["apps"][0] = json!({"members":1,"membership_complete":true,
                "energy_nj":measure(&(power * 1_000_000_000.0).to_string()),"energy_watts":power});
            point["other"] = json!({"app_count":1,"membership_complete":true,
                "energy_nj":measure("250000000"),"energy_watts":0.25});
        }
    }

    value
}

pub(crate) fn selection() -> Value {
    json!({"second":1000,"selected":{"run":[1,2],"boot":[3,4],"publication":2}})
}

fn observation(publication: u64) -> Value {
    let wall_ns = (publication + 998) * 1_000_000_000;
    json!({"recording":["5","6"],"stamp":{"boot":["3","4"],"run":["1","2"],
        "publication":publication.to_string(),"continuous_ns":wall_ns.to_string(),
        "awake_ns":wall_ns.to_string()},"run_tick":publication.to_string(),
        "wall_ns":wall_ns.to_string()})
}

fn point(publication: u64) -> Value {
    let cpu = if publication == 1 { 120.0 } else { 240.0 };
    json!({"observation":observation(publication),"segment":"0","elapsed_ns":"1000000000",
        "source_available":true,"membership_complete":true,"ungrouped_members":0,
        "usage_refused":0,"usage_gone":0,"apps":[{"members":1,"membership_complete":true,
        "cpu_ns":measure(if publication == 1 { "1200000000" }
            else { "2400000000" }),"cpu_percent_one_core":cpu}],"other":{"app_count":1,
        "membership_complete":true,"cpu_ns":measure("100000000"),
        "cpu_percent_one_core":10.0}})
}

fn measure(known: &str) -> Value {
    json!({"known":known,"measured_members":1,"unavailable_members":0,
        "overflowed":false})
}

#[test]
fn another_recordings_points_are_refused() {
    // A valid selected identity must not admit a context point from a different recording.
    let mut value = fixture(Metric::Cpu);
    value["points"][0]["observation"]["recording"] = json!(["9", "10"]);

    let history = AppAttributionHistory::prepare(Metric::Cpu, &value, &selection());

    assert!(history.panel().is_none());
}
