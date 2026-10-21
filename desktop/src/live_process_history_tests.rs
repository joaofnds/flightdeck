use serde_json::{Value, json};

use crate::live_process_history::ProcessHistory;
use crate::live_source::{AppKey, Request};

#[test]
fn process_history_projects_eight_distinct_captured_group_panels() {
    // The adapter changes identity and scope while preserving the captured App-history quantities.
    let history = ProcessHistory::prepare(payload(), &request(), &selection());

    let panels = history.panels(&selection()).unwrap();

    assert_eq!(
        panels
            .iter()
            .map(|panel| panel.id.as_str())
            .collect::<Vec<_>>(),
        [
            "live-process-group-cpu",
            "live-process-group-resident",
            "live-process-group-footprint",
            "live-process-group-disk",
            "live-process-group-power",
            "live-process-group-wakeups",
            "live-process-group-pageins",
            "live-process-group-threads"
        ]
    );
    assert_eq!(panels[0].lines[0].values, [Some(120.0), Some(240.0)]);
    assert_eq!(panels[1].lines[0].values, [Some(1.0), Some(2.0)]);
    assert_eq!(panels[2].lines[0].values, [Some(2.0), Some(3.0)]);
    assert_eq!(panels[3].lines[0].values, [Some(1.0), Some(2.0)]);
    assert_eq!(panels[3].lines[1].values, [Some(0.5), Some(1.0)]);
    assert_eq!(panels[4].lines[0].values, [Some(1.25), Some(2.5)]);
    assert_eq!(panels[5].lines[0].values, [Some(5.0), Some(11.0)]);
    assert_eq!(panels[6].lines[0].values, [Some(3.0), Some(7.0)]);
    assert_eq!(panels[7].lines[0].values, [Some(8.0), Some(13.0)]);
    assert_eq!(panels[0].selected_index, Some(1));
}

#[test]
fn process_history_rejects_a_different_app_payload() {
    // A well-formed name cannot authorize a history from a different outer App key.
    let mut value = payload();
    value["app_key"]["bytes_hex"] = json!("42");

    let history = ProcessHistory::prepare(value, &request(), &selection());

    assert!(history.panels(&selection()).unwrap().is_empty());
    assert!(history.payload().is_none());
}

#[test]
fn process_history_rejects_a_different_selected_stamp() {
    // Changing the source publication must refuse the group before any chart is installed.
    let mut value = payload();
    value["selected"]["stamp"]["publication"] = json!(3);

    let history = ProcessHistory::prepare(value, &request(), &selection());

    assert!(history.payload().is_none());
    assert!(history.panels(&selection()).unwrap().is_empty());
}

#[test]
fn process_history_rejects_a_different_executable_byte_key() {
    // Display equality cannot substitute a different captured name's bytes.
    let mut value = payload();
    value["executable_name"]["bytes_hex"] = json!("62");

    let history = ProcessHistory::prepare(value, &request(), &selection());

    assert!(history.payload().is_none());
}

#[test]
fn process_history_rejects_all_group_scope_in_a_named_response() {
    // This increment must never relabel App or All-group totals as one named group.
    let mut value = payload();
    value["scope"] = json!("all_process_groups");

    let history = ProcessHistory::prepare(value, &request(), &selection());

    assert!(history.payload().is_none());
}

#[test]
fn process_history_keeps_missing_metrics_and_observation_gaps() {
    // Keep nullable quantities and segment breaks at the exact captured timestamps.
    let mut value = payload();
    value["points"][0]["cpu_percent_one_core"] = Value::Null;
    value["points"][0]["disk_read_bps"] = Value::Null;
    value["points"][1]["segment"] = json!(1);
    let history = ProcessHistory::prepare(value, &request(), &selection());

    let panels = history.panels(&selection()).unwrap();

    assert_eq!(panels[0].lines[0].values, [None, Some(240.0)]);
    assert_eq!(panels[3].lines[0].values, [None, Some(2.0)]);
    assert!(panels[0].positions.as_ref().unwrap()[0] < panels[0].positions.as_ref().unwrap()[1]);
    assert_ne!(
        panels[0].segments.as_ref().unwrap()[0],
        panels[0].segments.as_ref().unwrap()[1]
    );
}

#[test]
fn process_history_rejects_more_than_4096_observations_before_projection() {
    // The typed boundary cannot admit an unbounded process-group response.
    let mut value = payload();
    value["points"] = json!(vec![value["points"][0].clone(); 4097]);

    let history = ProcessHistory::prepare(value, &request(), &selection());

    assert!(history.payload().is_none());
}

#[test]
fn process_history_preserves_non_utf8_name_bytes() {
    // Two names with the same rendered replacement character remain distinct identities.
    let mut value = payload();
    value["executable_name"] = json!({"bytes_hex":"ff","display":"�"});
    let mut request = request();
    request.executable = Some("hex:ff".into());

    let history = ProcessHistory::prepare(value, &request, &selection());

    assert!(history.payload().is_some());
    assert!(history.matches(&request));
    request.executable = Some("hex:fe".into());
    assert!(!history.matches(&request));
}

#[test]
fn process_history_empty_and_unknown_filters_have_distinct_local_reasons() {
    // Empty synthetic filters cannot query ProgramBytes, and unavailable names remain separate.
    assert_eq!(
        ProcessHistory::filter_reason(Some("hex:")),
        Some("Empty executable names have no captured named history")
    );
    assert_eq!(
        ProcessHistory::filter_reason(Some("unavailable")),
        Some("Executable identity unavailable; named history unavailable")
    );
    assert!(ProcessHistory::filter_reason(Some("hex:61")).is_none());
}

#[test]
fn all_groups_place_the_readings_after_the_selected_second_inside_their_charts() {
    // Goal: the process group charts of a selected second place the readings its context
    // holds after it within the 960 seconds the charts hold, the context's 900 and the lead-in
    // before them.
    // Method: select 999 in a context that runs to 1000, project every group chart, then read
    // where each places the reading of 1000.
    let mut history = crate::live_source::tests::process_history::all_history();
    history.points.truncate(2);
    history.app_bytes = Box::from(b"A".as_slice());
    history.selected.wall_ns = Some(999_000_000_000);
    history.points[0].observation.wall_ns = history.selected.wall_ns;
    history.points[1].observation.wall_ns = Some(1_000_000_000_000);
    let mut request = request();
    request.executable = None;
    let selection = json!({"second":999,"context_last_second":1000,
        "selected":history.selected.json()});
    let history = ProcessHistory::prepare_all(history, &request, &selection);

    let panels = history.panels(&selection).unwrap();

    assert!(!panels.is_empty());
    for panel in panels {
        let position = panel.positions.as_ref().unwrap()[1];
        assert!((position - 959.0 / 960.0).abs() < f32::EPSILON);
    }
}

#[test]
fn all_groups_place_a_lead_in_reading_at_the_left_edge() {
    // Goal: the process group charts hold the lead-in before the context's 900 seconds, so
    // their first points in view average whole windows, and they refuse a reading from before
    // the lead-in.
    // Method: move the first reading of a context that ends at 1000 to 41, the first second
    // the charts hold, project every group chart, then move it to 40 and project again.
    let mut request = request();
    request.executable = None;
    let starting_at = |first_second: u64| {
        let mut history = crate::live_source::tests::process_history::all_history();
        history.points.truncate(2);
        history.app_bytes = Box::from(b"A".as_slice());
        history.selected.wall_ns = Some(1_000_000_000_000);
        history.points[0].observation.wall_ns = Some(first_second * 1_000_000_000);
        history.points[1].observation.wall_ns = history.selected.wall_ns;
        let selection = json!({"second":1000,"context_last_second":1000,
            "selected":history.selected.json()});
        (
            ProcessHistory::prepare_all(history, &request, &selection),
            selection,
        )
    };
    let (history, selection) = starting_at(41);

    let panels = history.panels(&selection).unwrap();

    assert!(!panels.is_empty());
    for panel in panels {
        assert_eq!(panel.window.unwrap().first, 41);
        assert!(panel.positions.as_ref().unwrap()[0].abs() < f32::EPSILON);
    }
    let (early, selection) = starting_at(40);
    assert!(early.panels(&selection).is_err());
}

pub(crate) fn request() -> Request {
    Request {
        route: "apps".into(),
        second: None,
        identity: None,
        table: "ticks".into(),
        offset: 0,
        app: Some(AppKey {
            kind: "bundle".into(),
            bytes_hex: "41".into(),
        }),
        apps_offset: 0,
        members_offset: 0,
        cpu_offset: 0,
        executable: Some("hex:61".into()),
        load_process_history: true,
        earlier: crate::live_source::Earlier::Skip,
        load_resident_history: true,
        load_app_attribution_history: true,
    }
}

pub(crate) fn selection() -> Value {
    json!({"second":1000,"context_last_second":1000,
        "selected":{"run":[1,2],"boot":[3,4],"publication":2}})
}

pub(crate) fn payload() -> Value {
    json!({"selected":observation(2),"context_seconds":900,
        "app_key":{"kind":"bundle","bytes_hex":"41","display":"Example App"},
        "executable_name":{"bytes_hex":"61","display":"Accepted worker"},
        "scope":"selected_named_process_group","points":[point(1),point(2)]})
}

fn observation(publication: u64) -> Value {
    json!({"recording":[5,6],"stamp":{"run":[1,2],"boot":[3,4],"publication":publication,
        "continuous_ns":publication*1_000_000_000,"awake_ns":publication*1_000_000_000},
        "run_tick":publication,"wall_ns":(publication+998)*1_000_000_000})
}

fn point(publication: u64) -> Value {
    let factor = if publication == 1 { 1.0 } else { 2.0 };
    let wakeups = 6 * publication - 1;
    let pageins = 4 * publication - 1;
    let threads = 5 * publication + 3;
    json!({"observation":observation(publication),"segment":0,"cpu_percent_one_core":120.0*factor,
        "resident_bytes":(publication*1_073_741_824).to_string(),
        "footprint_bytes":((publication+1)*1_073_741_824).to_string(),"energy_watts":1.25*factor,
        "disk_read_bps":1_000_000.0*factor,"disk_written_bps":500_000.0*factor,
        "idle_wakeups_per_second":f64::from(u32::try_from(wakeups).unwrap()),
        "pageins_per_second":f64::from(u32::try_from(pageins).unwrap()),
        "idle_wakeups":{"known":wakeups.to_string(),"measured_members":1,
            "unavailable_members":0,"overflowed":false},
        "pageins":{"known":pageins.to_string(),"measured_members":1,
            "unavailable_members":0,"overflowed":false},
        "threads":{"known":threads.to_string(),"measured_members":1,
            "unavailable_members":0,"overflowed":false}})
}

#[test]
fn captured_event_rates_use_human_units_and_preserve_small_nonzero_values() {
    // Large rates choose a readable prefix while a small rate cannot appear as zero.
    assert_eq!(
        crate::live_projection::quantity_text(1500.0, "/s"),
        "1.5 k /s"
    );
    assert_eq!(
        crate::live_projection::quantity_text(0.001, "/s"),
        "0.001 /s"
    );
    assert_eq!(
        crate::live_projection::quantity_text(2_000_000.0, "pages/s"),
        "2,000,000 pages/s"
    );
}

#[test]
fn named_thread_reading_shows_one_exact_quantity() {
    // Read a named captured gauge directly so its exact quantity appears only once.
    let payload = json!({"points":[{"threads":{"known":"8","measured_members":1,
        "unavailable_members":0,"overflowed":false}}]});

    let reading = crate::live_process_history::captured_reading(
        &payload,
        "live-process-group-threads",
        Some(0),
    );

    assert_eq!(reading.as_deref(), Some("8 threads"));
}

#[test]
fn named_thread_reading_preserves_partial_coverage() {
    // Read a partial named gauge without repeating its known subtotal.
    let payload = json!({"points":[{"threads":{"known":"8","measured_members":1,
        "unavailable_members":2,"overflowed":false}}]});

    let reading = crate::live_process_history::captured_reading(
        &payload,
        "live-process-group-threads",
        Some(0),
    );

    assert_eq!(
        reading.as_deref(),
        Some("8 threads · known subtotal · 1 measured · 2 unavailable")
    );
}
