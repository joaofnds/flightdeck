use serde_json::json;

use super::*;

#[test]
fn resident_amounts_preserve_exact_bytes_while_readouts_and_plot_use_gib() {
    // Goal: a quantity beyond f64 integer precision stays exact where Apps are ordered, and
    // reads in binary units as the prototype's legend does.
    // Method: prepare one App above 2^53 bytes and read its amount, readout and plot value.
    let value = fixture();
    let history = ResidentHistory::prepare(&value, &selection());
    assert_eq!(history.amount(1, Some(1)), Some(9_007_199_254_740_993));
    assert_eq!(history.reading(1, Some(1)), "8,388,608 GiB");
    let panel = history.panel().unwrap();
    assert_eq!(panel.lines[0].name, "Other");
    assert_eq!(panel.lines[1].name, "Captured App");
    assert_eq!(panel.lines[1].values[0], Some(1.0));
    assert_eq!(panel.stack, Some(true));
}

#[test]
fn resident_partial_zero_overflow_and_missing_measures_remain_distinct() {
    // Change one real captured measure between all availability cases and inspect exact text.
    let mut value = fixture();
    value["points"][1]["apps"][0] = measure(0, 1, 2, false);
    let history = ResidentHistory::prepare(&value, &selection());
    assert_eq!(
        history.reading(1, Some(1)),
        "0 B known subtotal · 1 measured · 2 unavailable"
    );
    assert_eq!(history.panel().unwrap().lines[1].values[1], Some(0.0));

    value["points"][1]["apps"][0] = measure(100, 1, 0, true);
    let history = ResidentHistory::prepare(&value, &selection());
    assert_eq!(
        history.reading(1, Some(1)),
        "Unavailable · byte sum overflow"
    );
    assert_eq!(history.panel().unwrap().lines[1].values[1], None);

    value["points"][1]["apps"][0] = Value::Null;
    let history = ResidentHistory::prepare(&value, &selection());
    assert_eq!(history.reading(1, Some(1)), "Unavailable");
}

#[test]
fn empty_other_is_zero_only_when_source_and_membership_are_complete() {
    // An empty complete capture measures an empty set; incomplete capture leaves it unknown.
    let mut value = fixture();
    value["points"][1]["other"] = json!({"app_count":0,
        "resident_bytes":measure(0, 0, 0, false)});
    let history = ResidentHistory::prepare(&value, &selection());
    assert_eq!(history.reading(0, Some(1)), "0 B");
    assert_eq!(history.panel().unwrap().lines[0].values[1], Some(0.0));

    for field in ["ungrouped_members", "usage_refused", "usage_gone"] {
        let mut incomplete = value.clone();
        incomplete["points"][1][field] = json!(1);
        let history = ResidentHistory::prepare(&incomplete, &selection());
        assert_eq!(history.panel().unwrap().lines[0].values[1], None);
    }
    value["points"][1]["membership_complete"] = json!(false);
    let history = ResidentHistory::prepare(&value, &selection());
    assert_eq!(history.panel().unwrap().lines[0].values[1], None);
}

#[test]
fn absent_sources_optional_clocks_and_exact_siblings_do_not_borrow_values() {
    // Preserve a source gap and an unplaceable clock, retaining exact sibling identity lookup.
    let mut value = fixture();
    value["points"][0]["source_available"] = json!(false);
    for field in [
        "membership_complete",
        "ungrouped_members",
        "usage_refused",
        "usage_gone",
        "other",
    ] {
        value["points"][0][field] = Value::Null;
    }
    value["points"][0]["apps"][0] = Value::Null;
    value["points"][0]["observation"]["wall_ns"] = Value::Null;
    let history = ResidentHistory::prepare(&value, &selection());
    assert_eq!(history.panel().unwrap().lines[1].values[0], None);
    assert!(history.coverage(Some(0)).contains("source unavailable"));
    let target = Target::exact(
        1000,
        &json!({"run":[1,2],"boot":[3,4],
        "publication":2}),
    )
    .unwrap();
    assert_eq!(history.index(Some(&target)), Some(1));
    let missing = Target::exact(
        999,
        &json!({"run":[1,2],"boot":[3,4],
        "publication":3}),
    )
    .unwrap();
    assert_eq!(history.index(Some(&missing)), None);
    assert_eq!(history.reading(1, None), "Unavailable");
}

#[test]
fn decoder_refuses_wrong_selection_duplicate_keys_and_coverage_bounds() {
    // Each malformed response becomes a chart-local refusal with no projected panel.
    let mut wrong_selection = selection();
    wrong_selection["selected"]["publication"] = json!(3);
    assert!(
        ResidentHistory::prepare(&fixture(), &wrong_selection)
            .panel()
            .is_none()
    );
    let mut duplicate = fixture();
    let entry = duplicate["cohort"][0].clone();
    duplicate["cohort"].as_array_mut().unwrap().push(entry);
    assert!(
        ResidentHistory::prepare(&duplicate, &selection())
            .panel()
            .is_none()
    );

    for (field, bound) in [
        ("context_app_count", 24_000_u64),
        ("context_key_bytes", 8_388_608),
        ("cache_bytes", 67_108_864),
    ] {
        let mut value = fixture();
        value["coverage"][field] = json!(bound + 1);
        assert!(
            ResidentHistory::prepare(&value, &selection())
                .panel()
                .is_none()
        );
    }
}

#[test]
fn decoder_refuses_more_than_4096_distinct_observations() {
    // Keep every observation valid and the selection present on both sides of the point bound.
    let mut value = fixture();
    let points: Vec<_> = (1..=u64::try_from(POINTS_MAX + 1).unwrap())
        .map(|publication| {
            let mut value = point(publication, 42);
            value["observation"]["wall_ns"] = json!(1000 * NS);
            value
        })
        .collect();
    value["selected"] = points[1]["observation"].clone();
    value["points"] = json!(points[..POINTS_MAX]);
    assert!(matches!(
        ResidentHistory::prepare(&value, &selection()),
        ResidentHistory::Ready(_)
    ));

    value["points"] = json!(points);

    assert!(matches!(
        ResidentHistory::prepare(&value, &selection()),
        ResidentHistory::Unavailable(_)
    ));
}

#[test]
fn decoder_refuses_more_than_eight_distinct_leaders() {
    // Unique keys and aligned measures leave only the leader count invalid at the boundary.
    let mut value = fixture();
    let entries: Vec<_> = (0..=COHORT_MAX)
        .map(|index| {
            let mut entry = value["cohort"][0].clone();
            entry["key"]["bytes_hex"] = json!(format!("{index:02x}"));
            entry["key"]["display"] = json!(format!("App{index}"));
            entry
        })
        .collect();
    value["cohort"] = json!(entries[..COHORT_MAX]);
    for point in value["points"].as_array_mut().unwrap() {
        point["apps"] = json!(vec![measure(42, 1, 0, false); COHORT_MAX]);
    }
    assert!(matches!(
        ResidentHistory::prepare(&value, &selection()),
        ResidentHistory::Ready(_)
    ));

    value["cohort"] = json!(entries);
    for point in value["points"].as_array_mut().unwrap() {
        point["apps"] = json!(vec![measure(42, 1, 0, false); COHORT_MAX + 1]);
    }

    assert!(matches!(
        ResidentHistory::prepare(&value, &selection()),
        ResidentHistory::Unavailable(_)
    ));
}

#[test]
fn resident_decode_failure_preserves_machine_panels_and_selected_app_rankings() {
    // Prepare a Memory snapshot containing malformed attribution through the real acceptance path.
    let mut selected = selection();
    selected["trends"] = json!([{"run":[1,2],"boot":[3,4],"publication":2,
        "wall_ns":1_000_000_000_000_u64,"segment":0,"pressure":0}]);
    let rankings = std::sync::Arc::new(crate::live_rankings::Rankings::Unavailable(
        "Fixture ranking diagnostic".into(),
    ));
    let snapshot = Box::new(crate::live_source::Snapshot {
        generation: 1,
        catalog: Value::Null,
        selection: selected,
        pages: std::collections::BTreeMap::new(),
        apps: None,
        detail: None,
        history: Some(json!({"context_seconds":900})),
        cpu_detail: None,
        cpu_history: None,
        resident_history: None,
        app_attribution_history: None,
        overview_app_histories: None,
        histories_pending: false,
        process_history: None,
        rankings: Some(std::sync::Arc::clone(&rankings)),
        read_ms: 0.0,
    });
    let request = crate::live_source::Request {
        route: "memory".into(),
        second: None,
        identity: None,
        table: "ticks".into(),
        offset: 0,
        app: None,
        apps_offset: 0,
        members_offset: 0,
        cpu_offset: 0,
        load_resident_history: true,
        load_app_attribution_history: true,
        load_process_history: true,
        executable: None,
    };

    let presentation = crate::live_acceptance::Presentation::prepare(&request, snapshot).unwrap();

    assert!(!presentation.panels.is_empty());
    assert!(presentation.snapshot.history.is_none());
    assert!(std::sync::Arc::ptr_eq(
        &rankings,
        presentation.snapshot.rankings.as_ref().unwrap()
    ));
    assert!(matches!(
        presentation.snapshot.resident_history.as_deref(),
        Some(ResidentHistory::Unavailable(_))
    ));
}

fn fixture() -> Value {
    json!({"selected":observation(2),"context_seconds":900,
        "cohort":[{"key":{"kind":"bundle","bytes_hex":"41","display":"Captured App"},
            "sum_bytes":"9007200328482817","samples":2}],
        "points":[point(1, 1_073_741_824), point(2, 9_007_199_254_740_993)],
        "coverage":{"retention_limited":false,"context_app_count":2,
            "context_key_bytes":2,"cache_bytes":0}})
}

fn selection() -> Value {
    json!({"selected":{"run":[1,2],"boot":[3,4],"publication":2},"second":1000})
}

fn observation(publication: u64) -> Value {
    let wall_ns = (publication + 998) * NS;
    json!({"recording":[5,6],"stamp":{"run":[1,2],"boot":[3,4],
        "publication":publication,"continuous_ns":wall_ns,"awake_ns":wall_ns},
        "run_tick":publication,"wall_ns":wall_ns})
}

fn point(publication: u64, known: u64) -> Value {
    json!({"observation":observation(publication),"segment":0,"source_available":true,
        "membership_complete":true,"ungrouped_members":0,"usage_refused":0,"usage_gone":0,
        "apps":[measure(known, 1, 0, false)],
        "other":{"app_count":1,"resident_bytes":measure(20, 1, 0, false)}})
}

fn measure(known: u64, measured: u32, unavailable: u32, overflowed: bool) -> Value {
    json!({"known":known.to_string(),"measured_members":measured,
        "unavailable_members":unavailable,"overflowed":overflowed})
}
