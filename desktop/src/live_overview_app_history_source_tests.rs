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
    let cpu = presentation
        .snapshot
        .attribution_history(Metric::Cpu)
        .unwrap();
    assert_eq!(cpu.panel().unwrap().lines[9].name, "All CPU");
    assert_eq!(cpu.panel().unwrap().lines[9].values[1], Some(10.0));
    assert_eq!(cpu.amount(9, Some(1)), Some(10.0));
    assert_eq!(
        cpu.reading(9, Some(1)),
        "10.00 % · all recorded logical CPUs"
    );
    assert_eq!(disk.panel().unwrap().lines[9].name, "All disk I/O");
    assert_eq!(disk.panel().unwrap().lines[9].values[1], Some(6.0));
    assert_eq!(disk.amount(9, Some(1)), Some(6_000_000.0));
    assert_eq!(disk.captured_amount(9, Some(1)), None);
    assert_eq!(
        disk.reading(9, Some(1)),
        "6.0 MB/s · whole-device read + write"
    );
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
    let expected_lines = if metric == Metric::Power { 9 } else { 10 };
    assert_eq!(panel.lines.len(), expected_lines);
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

#[test]
fn real_overview_warm_reader_prepares_one_attribution_and_one_resident_context() {
    // A real warm Reader must gather two saved contexts even though it installs four histories.
    let recording = SealedRecording::with_ranked_apps();
    let mut reader = super::super::SnapshotReader::new();
    reader.command.request = exact_ranked_request("overview");
    assert!(reader.read(&recording).unwrap().is_some());
    let before = reader
        .reader
        .session
        .as_ref()
        .unwrap()
        .saved_history_context_preparations();
    reader.command.generation += 1;

    let snapshot = reader.read(&recording).unwrap().unwrap();

    let after = reader
        .reader
        .session
        .as_ref()
        .unwrap()
        .saved_history_context_preparations();
    assert_eq!(after - before, 2);
    assert_healthy_cpu_and_disk(&snapshot);
    let power = snapshot.attribution_history(Metric::Power).unwrap();
    assert_eq!(power.captured_amount(1, power.index(None)), Some(600_000));
    let resident = snapshot.resident_history.as_ref().unwrap();
    assert_eq!(resident.amount(1, resident.index(None)), Some(1_228_800));
}

#[test]
fn overview_batch_common_failure_replaces_stale_histories_and_preserves_other_readings() {
    // Feed the production adapter a common error after accepting real CPU, Disk and Resident data.
    let mut fixture = BatchResultFixture::new();
    let preserved: Box<PreservedReadings> = fixture.snapshot.as_ref().into();
    let failure = persistence::Error::Io(std::io::Error::other("x".repeat(2048)));

    crate::live_source::reader_overview_app_histories_result(Err(failure), &mut fixture.snapshot);

    assert_unavailable(&fixture.snapshot, Metric::Cpu);
    assert_unavailable(&fixture.snapshot, Metric::Power);
    assert_unavailable(&fixture.snapshot, Metric::Disk);
    preserved.assert_unchanged(&fixture.snapshot);
}

#[test]
fn overview_batch_power_failure_preserves_literal_cpu_disk_and_other_readings() {
    // A real batch's metric-local refusal cannot poison its two successful siblings.
    let mut fixture = BatchResultFixture::new();
    let preserved: Box<PreservedReadings> = fixture.snapshot.as_ref().into();
    fixture.histories.power = Err(persistence::Error::Bound("power test refusal"));

    crate::live_source::reader_overview_app_histories_result(
        Ok(fixture.histories),
        &mut fixture.snapshot,
    );

    assert_unavailable(&fixture.snapshot, Metric::Power);
    assert_healthy_cpu_and_disk(&fixture.snapshot);
    preserved.assert_unchanged(&fixture.snapshot);
}

#[test]
fn overview_batch_wrong_power_selection_withholds_only_power() {
    // Changing one actual payload's selected publication must not admit a neighboring identity.
    let mut fixture = BatchResultFixture::new();
    let preserved: Box<PreservedReadings> = fixture.snapshot.as_ref().into();
    fixture.histories.power.as_mut().unwrap()["selected"]["stamp"]["publication"] = json!("1");

    crate::live_source::reader_overview_app_histories_result(
        Ok(fixture.histories),
        &mut fixture.snapshot,
    );

    assert_unavailable(&fixture.snapshot, Metric::Power);
    assert_healthy_cpu_and_disk(&fixture.snapshot);
    preserved.assert_unchanged(&fixture.snapshot);
}

struct BatchResultFixture {
    snapshot: Box<crate::live_source::Snapshot>,
    histories: Box<viewer::AppAttributionHistories>,
}

impl BatchResultFixture {
    fn new() -> Self {
        let recording = SealedRecording::with_ranked_apps();
        let mut reader = super::super::SnapshotReader::new();
        reader.command.request = exact_ranked_request("overview");
        let snapshot = reader.read_complete(&recording);
        let histories = reader
            .reader
            .session
            .as_ref()
            .unwrap()
            .app_attribution_histories()
            .unwrap();

        Self {
            snapshot,
            histories,
        }
    }
}

struct PreservedReadings {
    selection: serde_json::Value,
    pages: std::collections::BTreeMap<String, serde_json::Value>,
    resident: Arc<crate::live_resident_history::ResidentHistory>,
    rankings: Arc<crate::live_rankings::Rankings>,
}

impl From<&crate::live_source::Snapshot> for Box<PreservedReadings> {
    fn from(snapshot: &crate::live_source::Snapshot) -> Self {
        Box::new(PreservedReadings {
            selection: snapshot.selection.clone(),
            pages: snapshot.pages.clone(),
            resident: Arc::clone(snapshot.resident_history.as_ref().unwrap()),
            rankings: Arc::clone(snapshot.rankings.as_ref().unwrap()),
        })
    }
}

impl PreservedReadings {
    fn assert_unchanged(&self, snapshot: &crate::live_source::Snapshot) {
        assert_eq!(snapshot.selection, self.selection);
        assert_eq!(snapshot.pages, self.pages);
        assert!(Arc::ptr_eq(
            snapshot.resident_history.as_ref().unwrap(),
            &self.resident
        ));
        assert!(Arc::ptr_eq(
            snapshot.rankings.as_ref().unwrap(),
            &self.rankings
        ));
    }
}

fn assert_healthy_cpu_and_disk(snapshot: &crate::live_source::Snapshot) {
    let cpu = snapshot.attribution_history(Metric::Cpu).unwrap();
    assert_eq!(cpu.captured_amount(1, cpu.index(None)), Some(600));
    assert_eq!(cpu.panel().unwrap().lines[1].values[1], Some(0.000_06));
    let disk = snapshot.attribution_history(Metric::Disk).unwrap();
    assert_eq!(disk.captured_amount(1, disk.index(None)), Some(900_000_000));
    assert_eq!(disk.amount(1, disk.index(None)), Some(900_000_000.0));
}

fn assert_unavailable(snapshot: &crate::live_source::Snapshot, metric: Metric) {
    let crate::live_app_attribution_history::AppAttributionHistory::Unavailable(message) =
        snapshot.attribution_history(metric).unwrap()
    else {
        panic!("{metric:?} retained a history after its batch failed");
    };
    assert!(!message.is_empty());
    assert!(message.chars().count() <= 513);
}
