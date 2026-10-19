use std::sync::Arc;

use serde_json::json;

use super::{SealedRecording, exact_ranked_request};
use crate::live_app_attribution_history::Metric;

#[test]
fn real_overview_worker_installs_four_distinct_captured_app_histories() {
    // Read 600 sealed Apps through Overview and project its four exact captured cohorts.
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request("overview");

    let snapshot = recording.read(request.clone()).unwrap();
    let read_ms = snapshot.read_ms;
    let presentation = crate::live_acceptance::Presentation::prepare(&request, snapshot).unwrap();

    assert_eq!(
        presentation.snapshot.selection["selected"]["publication"],
        2
    );
    assert_overview_rate(&presentation, Metric::Cpu, 0.000_06);
    assert_overview_rate(&presentation, Metric::Power, 0.000_6);
    assert_overview_rate(&presentation, Metric::Disk, 900.0);
    let resident = presentation.snapshot.resident_history.as_ref().unwrap();
    let panel = resident.panel().unwrap();
    assert_eq!(panel.lines.len(), 9);
    assert_eq!(panel.lines[0].name, "Other");
    assert_eq!(panel.lines[1].name, "App00600");
    assert_eq!(resident.amount(1, panel.selected_index), Some(1_228_800));
    let disk = presentation
        .snapshot
        .attribution_history(Metric::Disk)
        .unwrap();
    assert_eq!(disk.captured_amount(1, disk.index(None)), Some(900_000_000));
    println!(
        "{}",
        json!({"case":"overview_real_worker", "read_ms":read_ms})
    );
}

fn assert_overview_rate(
    presentation: &crate::live_acceptance::Presentation,
    metric: Metric,
    expected: f32,
) {
    let history = presentation.snapshot.attribution_history(metric).unwrap();
    let panel = presentation
        .panels
        .iter()
        .find(|panel| panel.id == metric.panel_id())
        .unwrap();
    assert_eq!(panel.lines.len(), 9);
    assert_eq!(panel.lines[0].name, "Other");
    assert_eq!(panel.lines[1].name, "App00600");
    assert_eq!(panel.selected_index, Some(1));
    assert_eq!(panel.lines[1].values[1], Some(expected));
    assert_eq!(history.index(None), Some(1));
}

#[test]
fn real_overview_preview_keeps_context_and_reads_only_exact_app_totals() {
    // Preview the earlier observation without querying any history again.
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request("overview");
    let accepted = recording.read(request.clone()).unwrap();
    let target =
        crate::live_hover::Target::exact(1000, &json!({"run":[1,2],"boot":[3,4],"publication":1}))
            .unwrap();
    let preview = crate::live_hover::Preview::resident(&accepted, &request, target);
    assert!(!preview.request.load_app_attribution_history);
    assert!(!preview.request.load_resident_history);
    assert!(Arc::ptr_eq(
        accepted.overview_app_histories.as_ref().unwrap(),
        preview.reading.overview_app_histories.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        accepted.resident_history.as_ref().unwrap(),
        preview.reading.resident_history.as_ref().unwrap()
    ));

    let exact = recording.read(preview.request).unwrap();

    assert_eq!(exact.selection["selected"]["publication"], 1);
    assert!(exact.overview_app_histories.is_none());
    assert!(exact.resident_history.is_none());
    assert!(exact.rankings.is_some());
}

#[test]
fn real_overview_source_gap_never_borrows_an_app_history() {
    // A missing observation has no App context or rankings from its neighboring tick.
    let recording = SealedRecording::with_ranked_apps();
    let mut request = exact_ranked_request("overview");
    request.second = Some(1002);
    request.identity = None;

    let snapshot = recording.read(request).unwrap();

    assert!(snapshot.selection["selected"].is_null());
    assert!(snapshot.overview_app_histories.is_none());
    assert!(snapshot.resident_history.is_none());
    assert!(snapshot.rankings.is_none());
}

#[test]
fn overview_exact_history_lookup_reports_its_frontend_cost() {
    // Exclude real sealed setup and preparation from four production exact history lookups.
    let recording = SealedRecording::with_ranked_apps();
    let snapshot = recording.read(exact_ranked_request("overview")).unwrap();
    let target =
        crate::live_hover::Target::exact(1000, &json!({"run":[1,2],"boot":[3,4],"publication":1}))
            .unwrap();
    let started = std::time::Instant::now();

    let (indices, counts) = crate::allocation_tests::measure(|| {
        [
            snapshot
                .attribution_history(Metric::Cpu)
                .unwrap()
                .index(Some(&target)),
            snapshot
                .resident_history
                .as_ref()
                .unwrap()
                .index(Some(&target)),
            snapshot
                .attribution_history(Metric::Disk)
                .unwrap()
                .index(Some(&target)),
            snapshot
                .attribution_history(Metric::Power)
                .unwrap()
                .index(Some(&target)),
        ]
    });

    assert_eq!(indices, [Some(0); 4]);
    println!(
        "{}",
        json!({"case":"overview_exact_history_lookup", "indices":indices,
        "allocation_calls":counts.calls,"requested_bytes":counts.bytes,
        "elapsed_ns":started.elapsed().as_nanos()})
    );
    assert!(counts.calls <= 16, "{counts:?}");
}
