use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use super::{fresh_request, selected_copy};
use crate::live_dashboard::{LiveDashboard, Row};
use crate::live_source::{AppKey, Snapshot, Worker};

fn snapshot() -> Box<Snapshot> {
    Box::new(Snapshot {
        generation: 1,
        catalog: json!({}),
        selection: json!({"second":1000,"selected":{"publication":7},
            "summary":{"cpu_percent":12.5}}),
        pages: BTreeMap::from([(
            "ticks".into(),
            json!({"columns":["known","missing"],
            "rows":[["9007199254740993",null]]}),
        )]),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        read_ms: 0.0,
    })
}

fn missing_recording() -> PathBuf {
    std::env::temp_dir().join(format!("flightdeck-missing-native-{}", std::process::id()))
}

fn dashboard(route: &str) -> LiveDashboard {
    LiveDashboard::new(
        Worker::start(missing_recording()).unwrap(),
        route.into(),
        None,
    )
}

#[test]
fn opening_an_unreadable_recording_preserves_the_current_selection() {
    // Worker startup can succeed before its real query fails; keep the accepted reading then.
    let mut dashboard = dashboard("raw");
    dashboard.snapshot = Some(snapshot());
    dashboard.request.second = Some(1000);
    dashboard.request.identity = Some(json!({"publication":7}));
    let previous_request = dashboard.request.clone();
    let previous_generation = dashboard.generation;
    dashboard.begin_recording(missing_recording());
    for _attempt in 0..1000 {
        if dashboard.recording_step() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(dashboard.native.candidate.is_none());
    assert!(
        dashboard
            .native
            .notice
            .as_ref()
            .unwrap()
            .starts_with("Recording unchanged:")
    );
    assert_eq!(dashboard.request, previous_request);
    assert_eq!(dashboard.generation, previous_generation);
    assert_eq!(
        dashboard.snapshot.as_ref().unwrap().selection["selected"]["publication"],
        7
    );
}

#[test]
fn opening_a_missing_recording_from_computer_keeps_the_previous_recording() {
    // Computer inventory is available without a recording; opening still needs a real query.
    let mut dashboard = dashboard("computer");
    dashboard.snapshot = Some(snapshot());
    dashboard.request.second = Some(1000);
    let previous_generation = dashboard.generation;
    dashboard.begin_recording(missing_recording());
    for _attempt in 0..1000 {
        if dashboard.recording_step() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(dashboard.native.candidate.is_none());
    assert!(
        dashboard
            .native
            .notice
            .as_ref()
            .unwrap()
            .starts_with("Recording unchanged:")
    );
    assert_eq!(dashboard.request.route, "computer");
    assert_eq!(dashboard.request.second, Some(1000));
    assert_eq!(dashboard.generation, previous_generation);
    assert_eq!(
        dashboard.snapshot.as_ref().unwrap().selection["selected"]["publication"],
        7
    );
}

#[test]
fn canceling_a_pending_switch_cannot_publish_its_result_into_the_old_recording() {
    // A canceled candidate has a separate mailbox; a later step cannot accept its update.
    let mut dashboard = dashboard("raw");
    dashboard.snapshot = Some(snapshot());
    dashboard.request.second = Some(1000);
    dashboard.begin_recording(missing_recording());
    dashboard.cancel_recording("Recording unchanged");
    assert!(!dashboard.recording_step());
    assert_eq!(dashboard.request.second, Some(1000));
    assert_eq!(
        dashboard.snapshot.as_ref().unwrap().selection["selected"]["publication"],
        7
    );
}

#[test]
fn overview_defers_artwork_until_a_reading_is_available() {
    // Cold row planning must not load the artwork only to push it below arriving charts.
    let mut dashboard = dashboard("overview");
    assert!(
        !dashboard
            .rows
            .iter()
            .any(|row| matches!(row, Row::Hardware))
    );
    assert!(
        dashboard
            .rows
            .iter()
            .any(|row| matches!(row, Row::OverviewStatus))
    );
    dashboard.snapshot = Some(snapshot());
    dashboard.rebuild();
    assert!(
        dashboard
            .rows
            .iter()
            .any(|row| matches!(row, Row::Hardware))
    );
    dashboard.request.route = "raw".into();
    dashboard.rebuild();
    assert!(
        !dashboard
            .rows
            .iter()
            .any(|row| matches!(row, Row::Hardware))
    );
}

#[test]
fn copying_an_expanded_reading_preserves_exact_counters_nulls_and_observation() {
    // Clipboard interchange must retain values too large for a floating point integer.
    let snapshot = snapshot();
    let output = selected_copy(
        Some(&snapshot),
        &fresh_request("raw"),
        Some(&("ticks".into(), 0)),
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["values"][0], "9007199254740993");
    assert!(value["values"][1].is_null());
    assert_eq!(value["columns"], json!(["known", "missing"]));
    assert_eq!(value["observation"]["publication"], 7);
}

#[test]
fn copying_during_app_switch_refuses_retained_details_from_the_previous_app() {
    // Selecting B cannot copy retained A details while B's query is pending.
    let mut snapshot = snapshot();
    snapshot.detail = Some(json!({"app":{"key":{"kind":"bundle","bytes_hex":"41"}}}));
    let mut request = fresh_request("apps");
    request.app = Some(AppKey {
        kind: "bundle".into(),
        bytes_hex: "42".into(),
    });
    assert!(selected_copy(Some(&snapshot), &request, None).is_err());
}

#[test]
fn copying_an_expanded_process_keeps_its_creation_identity_and_exact_counters() {
    // A selected process is copied as that process, rather than the entire App or a raw page.
    let mut snapshot = snapshot();
    snapshot.detail = Some(json!({"app":{"key":{"kind":"bundle","bytes_hex":"41"}},
        "processes":[{"pid":123,"creation":{"unique_id":"987","pid_version":4},
            "cpu_ns":{"known":"9007199254740993"}}]}));
    let mut request = fresh_request("apps");
    request.app = Some(AppKey {
        kind: "bundle".into(),
        bytes_hex: "41".into(),
    });
    let output = selected_copy(
        Some(&snapshot),
        &request,
        Some(&("app_processes".into(), 0)),
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["detail"]["pid"], 123);
    assert_eq!(value["detail"]["creation"]["unique_id"], "987");
    assert_eq!(value["detail"]["cpu_ns"]["known"], "9007199254740993");
}

#[test]
fn retiring_a_real_reader_signals_shutdown_without_consuming_its_join_handle() {
    // The UI retains the stopped worker handle and only joins once it reports completion.
    let worker = Worker::start(missing_recording()).unwrap();
    let handle = worker.retire().unwrap();
    for _attempt in 0..1000 {
        if handle.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(handle.is_finished());
    assert!(handle.join().is_ok());
}
