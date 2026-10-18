use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{JoinHandle, yield_now};
use std::time::{Duration, Instant};

use gpui::{Entity, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::json;

use super::{fresh_request, selected_copy};
use crate::live_dashboard::{LiveDashboard, Row};
use crate::live_source::{AppKey, Snapshot, Worker};

const WAITING: &str = "Finishing recording cleanup. Open will become available when it finishes.";
const DELAYED: &str = "Recording cleanup is taking longer. You can keep using \
    this recording. Close and reopen Flightdeck to open another recording.";
const TEST_DEADLINE: Duration = Duration::from_secs(10);

#[gpui::test]
fn opening_while_a_reader_retires_explains_the_wait_and_preserves_readings(
    cx: &mut TestAppContext,
) {
    // Drive the installed Open action after the production retirement initializer runs.
    let mut harness = RetirementHarness::setup(cx);
    harness.retire_reader();

    harness.driver.open();

    assert!(!harness.driver.picker_is_open());
    assert_eq!(harness.driver.notice().as_deref(), Some(WAITING));
    assert!(harness.driver.wait_is_visible());
    assert!(harness.driver.open_is_unavailable());
    assert_eq!(harness.driver.selected_publication(), 7);
    harness.teardown();
}

#[gpui::test]
fn a_delayed_retirement_preserves_the_recording_and_explains_recovery(cx: &mut TestAppContext) {
    // Check the exact grace boundary without waiting for wall-clock time or constructing state.
    let mut harness = RetirementHarness::setup(cx);
    harness.retire_reader();
    harness
        .driver
        .advance_clock(Duration::from_nanos(4_999_999_999));
    assert!(harness.driver.wait_is_visible());

    harness.driver.advance_clock(Duration::from_secs(5));
    harness.driver.open();
    harness.driver.open();

    assert!(harness.driver.recovery_is_visible());
    assert!(harness.driver.open_is_unavailable());
    assert!(!harness.driver.picker_is_open());
    assert_eq!(harness.driver.selected_publication(), 7);
    harness.teardown();
}

#[gpui::test]
fn a_late_retirement_reenables_open_and_clears_its_delayed_status(cx: &mut TestAppContext) {
    // Release the reader, hold completion separately, then restore the same installed command.
    let mut harness = RetirementHarness::setup(cx);
    harness.retire_reader();
    harness.driver.advance_clock(Duration::from_secs(5));
    harness.driver.open();
    harness.release_reader();
    harness.driver.poll();
    assert!(harness.driver.open_is_unavailable());

    harness.finish_reader();

    assert!(!harness.driver.recovery_is_visible());
    assert!(harness.driver.open_is_available());
    assert_eq!(harness.driver.notice(), None);
    assert_eq!(harness.driver.selected_publication(), 7);
    harness.driver.open();
    assert!(harness.driver.picker_is_open());
    harness.driver.cancel_picker();
    assert_eq!(harness.driver.selected_publication(), 7);
    harness.teardown();
}

#[gpui::test]
fn retiring_a_failed_candidate_preserves_its_error_and_the_active_recording(
    cx: &mut TestAppContext,
) {
    // A real missing-folder query fails B; cleanup must retain its error while A stays selected.
    let mut harness = RetirementHarness::setup(cx);
    harness.driver.fail_candidate();
    let failure = harness.driver.notice().unwrap();
    assert!(failure.starts_with("Recording unchanged:"));

    harness.driver.finish_retirement().unwrap();

    assert_eq!(harness.driver.notice(), Some(failure));
    assert!(harness.driver.open_is_available());
    assert_eq!(harness.driver.selected_publication(), 7);
    harness.teardown();
}

#[gpui::test]
fn retirement_completion_preserves_a_newer_copy_notice(cx: &mut TestAppContext) {
    // Copy while the retired reader is held, then complete cleanup after the newer feedback.
    let mut harness = RetirementHarness::setup(cx);
    harness.retire_reader();
    harness.driver.copy_reading();

    harness.finish_reader();

    assert_eq!(
        harness.driver.notice().as_deref(),
        Some("Selected reading copied")
    );
    assert!(harness.driver.open_is_available());
    assert_eq!(harness.driver.selected_publication(), 7);
    harness.teardown();
}

#[gpui::test]
fn retirement_completion_preserves_a_newer_time_entry_notice(cx: &mut TestAppContext) {
    // Invalid time entry remains actionable after unrelated reader cleanup finishes.
    let mut harness = RetirementHarness::setup(cx);
    harness.retire_reader();
    harness.driver.enter_invalid_time();
    let notice = harness.driver.notice().unwrap();

    harness.finish_reader();

    assert_eq!(harness.driver.notice(), Some(notice));
    assert!(harness.driver.open_is_available());
    assert_eq!(harness.driver.selected_publication(), 7);
    harness.teardown();
}

#[gpui::test]
fn an_absent_reader_does_not_block_open(cx: &mut TestAppContext) {
    // A worker with no join handle cannot create a waiting retirement slot.
    let mut harness = RetirementHarness::setup(cx);

    harness.driver.retire_absent_reader();
    harness.driver.open();

    assert!(harness.driver.picker_is_open());
    assert!(harness.driver.open_is_available());
    assert!(!harness.driver.wait_is_visible());
    harness.driver.cancel_picker();
    harness.teardown();
}

struct RetirementHarness {
    driver: RetirementDriver,
    blocked: Option<BlockedReader>,
    cleaned: bool,
}

impl RetirementHarness {
    fn setup(cx: &mut TestAppContext) -> Self {
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                super::create_dashboard(
                    Worker::start(missing_recording()).unwrap(),
                    "raw".into(),
                    None,
                    cx,
                )
            })
            .unwrap()
        });
        let entity = window.root(cx).unwrap();
        let mut context = VisualTestContext::from_window(window.into(), cx);
        entity.update(&mut context, |dashboard, cx| {
            dashboard.snapshot = Some(snapshot());
            dashboard.request.second = Some(1000);
            dashboard.rebuild();
            cx.notify();
        });
        context.run_until_parked();

        Self {
            driver: RetirementDriver {
                entity,
                context,
                started: Instant::now(),
            },
            blocked: None,
            cleaned: false,
        }
    }

    fn retire_reader(&mut self) {
        assert!(self.blocked.is_none());
        let mut blocked = BlockedReader::new();
        self.driver.begin_retirement(blocked.thread.take());
        self.blocked = Some(blocked);
    }

    fn release_reader(&mut self) {
        self.blocked.as_mut().unwrap().release().unwrap();
    }

    fn finish_reader(&mut self) {
        self.blocked.as_mut().unwrap().finish().unwrap();
        self.driver.finish_retirement().unwrap();
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if self.cleaned {
            return Ok(());
        }

        self.driver.cancel_pending_open();
        if let Some(blocked) = &mut self.blocked {
            blocked.finish()?;
        }
        self.driver.finish_retirement()?;
        self.driver
            .entity
            .update(&mut self.driver.context, |dashboard, _| {
                dashboard.worker.stop();
            });
        self.driver
            .context
            .update(|window, _| window.remove_window());
        self.cleaned = true;

        Ok(())
    }

    fn teardown(mut self) {
        self.cleanup().unwrap();
    }
}

impl Drop for RetirementHarness {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("Retirement harness cleanup: {error}");
        }
    }
}

struct RetirementDriver {
    entity: Entity<LiveDashboard>,
    context: VisualTestContext,
    started: Instant,
}

impl RetirementDriver {
    fn begin_retirement(&mut self, thread: Option<JoinHandle<()>>) {
        self.entity.update(&mut self.context, |dashboard, cx| {
            dashboard.native.retire(thread, self.started);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn retire_absent_reader(&mut self) {
        self.begin_retirement(None);
    }

    fn open(&mut self) {
        self.context.dispatch_action(super::OpenRecording);
    }

    fn poll(&mut self) {
        self.entity.update(&mut self.context, |dashboard, cx| {
            if dashboard.native.poll_retirement().unwrap() {
                cx.notify();
            }
        });
        self.context.run_until_parked();
    }

    fn advance_clock(&mut self, elapsed: Duration) {
        let now = self.started.checked_add(elapsed).unwrap();
        self.entity.update(&mut self.context, |dashboard, cx| {
            if dashboard.native.poll_retirement_at(now).unwrap() {
                cx.notify();
            }
        });
        self.context.run_until_parked();
    }

    fn finish_retirement(&mut self) -> Result<(), String> {
        await_condition(|| {
            self.entity.update(&mut self.context, |dashboard, cx| {
                if dashboard.native.poll_retirement().unwrap() {
                    cx.notify();
                }
                dashboard.native.retirement_status().is_none()
            })
        })?;
        self.context.run_until_parked();

        Ok(())
    }

    fn fail_candidate(&mut self) {
        self.entity.update(&mut self.context, |dashboard, _| {
            dashboard.begin_recording(missing_recording());
        });
        await_condition(|| {
            self.entity.update(&mut self.context, |dashboard, _| {
                dashboard.recording_step();
                !dashboard.native.opening()
            })
        })
        .unwrap();
    }

    fn copy_reading(&mut self) {
        self.entity
            .update(&mut self.context, LiveDashboard::copy_reading);
    }

    fn cancel_pending_open(&mut self) {
        self.entity.update(&mut self.context, |dashboard, _| {
            dashboard.cancel_recording("Test finished");
        });
    }

    fn enter_invalid_time(&mut self) {
        self.entity.update(&mut self.context, |dashboard, cx| {
            dashboard.inspect_text("not a time", cx);
        });
    }

    fn cancel_picker(&mut self) {
        self.context.simulate_path_prompt_response(|_| None);
        self.entity
            .update(&mut self.context, LiveDashboard::native_tick);
    }

    fn picker_is_open(&self) -> bool {
        self.context.did_prompt_for_paths()
    }

    fn open_is_available(&mut self) -> bool {
        self.context
            .debug_bounds("open-recording-available")
            .is_some()
    }

    fn open_is_unavailable(&mut self) -> bool {
        self.context
            .debug_bounds("open-recording-waiting")
            .is_some()
    }

    fn wait_is_visible(&mut self) -> bool {
        self.context.debug_bounds(WAITING).is_some()
    }

    fn recovery_is_visible(&mut self) -> bool {
        self.context.debug_bounds(DELAYED).is_some()
    }

    fn notice(&self) -> Option<String> {
        self.entity.read_with(&self.context, |dashboard, _| {
            dashboard.native.notice.clone()
        })
    }

    fn selected_publication(&self) -> u64 {
        self.entity.read_with(&self.context, |dashboard, _| {
            dashboard.snapshot.as_ref().unwrap().selection["selected"]["publication"]
                .as_u64()
                .unwrap()
        })
    }
}

struct BlockedReader {
    release: Option<SyncSender<()>>,
    released: Option<Receiver<()>>,
    complete: Option<SyncSender<()>>,
    completed: Option<Receiver<()>>,
    thread: Option<JoinHandle<()>>,
}

impl BlockedReader {
    fn new() -> Self {
        let (release, waiting) = sync_channel(1);
        let (released, saw_release) = sync_channel(1);
        let (complete, waiting_completion) = sync_channel(1);
        let (completed, saw_completion) = sync_channel(1);
        let (ready, started) = sync_channel(1);
        let thread = std::thread::spawn(move || {
            ready.send(()).unwrap();
            waiting.recv().unwrap();
            released.send(()).unwrap();
            waiting_completion.recv().unwrap();
            completed.send(()).unwrap();
        });
        started.recv_timeout(TEST_DEADLINE).unwrap();

        Self {
            release: Some(release),
            released: Some(saw_release),
            complete: Some(complete),
            completed: Some(saw_completion),
            thread: Some(thread),
        }
    }

    fn release(&mut self) -> Result<(), String> {
        if let Some(release) = self.release.take() {
            release.send(()).map_err(|error| error.to_string())?;
        }
        if let Some(released) = self.released.take() {
            released
                .recv_timeout(TEST_DEADLINE)
                .map_err(|error| error.to_string())?;
        }

        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        self.release()?;
        if let Some(complete) = self.complete.take() {
            complete.send(()).map_err(|error| error.to_string())?;
        }
        if let Some(completed) = self.completed.take() {
            completed
                .recv_timeout(TEST_DEADLINE)
                .map_err(|error| error.to_string())?;
        }

        Ok(())
    }
}

impl Drop for BlockedReader {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("Blocked reader release: {error}");
        }
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                eprintln!("Blocked reader panicked");
            }
        }
    }
}

fn await_condition(mut condition: impl FnMut() -> bool) -> Result<(), String> {
    let deadline = Instant::now().checked_add(TEST_DEADLINE).unwrap();
    while Instant::now() < deadline {
        if condition() {
            return Ok(());
        }
        yield_now();
    }

    Err("Reader condition did not complete before the test deadline".into())
}
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
        cpu_history: None,
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
fn opening_a_recording_with_an_unpresentable_result_preserves_the_current_recording() {
    // Goal: preparation failure cannot replace a readable recording or its selected identity.
    // Method: deliver a candidate result with invalid page metadata through its update port.
    let mut dashboard = dashboard("raw");
    dashboard.snapshot = Some(snapshot());
    dashboard.request.second = Some(1000);
    let previous_request = dashboard.request.clone();
    let previous_generation = dashboard.generation;
    dashboard.begin_recording(missing_recording());
    let mut invalid = snapshot();
    invalid.generation = dashboard.native.candidate.as_ref().unwrap().generation;
    invalid.selection["selected"]["publication"] = json!(8);

    assert!(dashboard.recording_receive(crate::live_source::Update::Ready(invalid)));

    assert_eq!(dashboard.request, previous_request);
    assert_eq!(dashboard.generation, previous_generation);
    assert_eq!(
        dashboard.native.source,
        crate::live_source_mode::SourceMode::Live
    );
    assert_eq!(
        dashboard.snapshot.as_ref().unwrap().selection["selected"]["publication"],
        7
    );
    assert!(
        dashboard
            .native
            .notice
            .as_ref()
            .unwrap()
            .starts_with("Recording unchanged:")
    );
}

#[test]
fn a_saved_source_becomes_recorded_only_when_its_open_commits() {
    // Goal: source presentation changes atomically with an accepted recording, never before.
    // Method: deliver a valid candidate through the same update port used by its reader.
    let mut dashboard = dashboard("overview");
    dashboard.snapshot = Some(snapshot());
    dashboard.begin_recording(missing_recording());
    let mut ready = snapshot();
    ready.generation = dashboard.native.candidate.as_ref().unwrap().generation;
    ready.pages.get_mut("ticks").unwrap()["offset"] = json!(0);
    assert_eq!(
        dashboard.native.source,
        crate::live_source_mode::SourceMode::Live
    );

    assert!(dashboard.recording_receive(crate::live_source::Update::Ready(ready)));

    assert_eq!(
        dashboard.native.source,
        crate::live_source_mode::SourceMode::Recorded
    );
    assert_eq!(dashboard.native.source.following_label(true), "Latest");
    assert_eq!(
        dashboard.native.source.sidebar(),
        "Recorded system readings"
    );
    assert_eq!(
        dashboard.native.source.title(),
        "Flightdeck GPUI · Recorded"
    );
}

#[test]
fn canceling_an_open_keeps_the_current_source_mode() {
    // Goal: cancel cannot relabel retained live readings as an opened saved recording.
    // Method: cancel a pending candidate and inspect the still-current source presentation.
    let mut dashboard = dashboard("overview");
    dashboard.begin_recording(missing_recording());

    dashboard.cancel_recording("Recording unchanged");

    assert_eq!(
        dashboard.native.source,
        crate::live_source_mode::SourceMode::Live
    );
    assert_eq!(dashboard.native.source.following_label(true), "Live");
}

#[test]
fn diagnostic_copy_keeps_current_catalog_and_the_committed_inspection_identity() {
    // Goal: copying diagnostics explains the source without pretending its catalog is selected.
    // Method: serialize distinct current and inspected observations with an exact large counter.
    let mut current = snapshot();
    current.selection = serde_json::Value::Null;
    current.catalog = json!({"path":"/recording", "latest_observation":{"publication":9},
        "tail_cache_bytes":9_007_199_254_740_993_u64});
    let mut request = fresh_request("self");
    request.second = Some(1000);
    request.identity = Some(json!({"publication":7}));
    let selected = json!({"publication":7});

    let text = super::diagnostic_copy(Some(&current), &request, &selected).unwrap();

    let copied: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        copied,
        json!({"catalog":current.catalog,"selected":selected,
        "requested_second":1000,"requested_identity":{"publication":7}})
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
