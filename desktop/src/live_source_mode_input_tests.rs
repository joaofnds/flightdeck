use std::path::{Path, PathBuf};
use std::thread::park_timeout;
use std::time::{Duration, Instant};

use collector::recording::{Registration, identity_text, random_identity};
use collector::ring::{ObservationTimes, Place, Readings, Shape, Writer};
use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::Value;

use super::{InputDriver, InputHarness};
use crate::live_dashboard::LiveDashboard;
use crate::live_source::Worker;
use crate::live_source_mode::SourceMode;

#[gpui::test]
fn opening_a_saved_recording_updates_the_existing_window_and_rendered_source_controls(
    cx: &mut TestAppContext,
) {
    // Open a real empty store through the installed File action and platform path response.
    let saved = SavedRecording::new();
    let mut harness = InputHarness::setup(cx);
    assert_eq!(harness.driver.source_title(), "Flightdeck · Live");

    harness.driver.choose_recording(&saved.root);
    harness.driver.await_source(SourceMode::Recorded);

    assert_eq!(harness.driver.source_title(), "Flightdeck · Recorded");
    assert!(
        harness
            .driver
            .visible("source-sidebar:Recorded system readings")
    );
    assert!(harness.driver.visible("source-status:Recorded"));
    harness.driver.open_time_menu();
    assert!(harness.driver.visible("source-follow:Pause latest"));
    assert!(!harness.driver.visible("source-follow:Pause live"));
    harness.teardown();
}

#[gpui::test]
fn failing_to_open_a_recording_retains_the_existing_window_source_and_selection(
    cx: &mut TestAppContext,
) {
    // A real reader failure must leave the accepted live presentation and window title intact.
    let mut harness = InputHarness::setup(cx);
    let selected = harness.driver.accepted_selection();
    let missing = unique_root("missing");

    harness.driver.choose_recording(&missing);
    harness.driver.await_open_failure();

    assert_eq!(harness.driver.source_title(), "Flightdeck · Live");
    assert_eq!(harness.driver.accepted_selection(), selected);
    assert!(
        harness
            .driver
            .visible("source-sidebar:Live system readings")
    );
    assert!(harness.driver.visible("source-follow:Live"));
    harness.teardown();
}

#[gpui::test]
fn canceling_the_recording_picker_retains_the_existing_window_source_and_selection(
    cx: &mut TestAppContext,
) {
    // Cancel the real installed path prompt before any candidate recording is committed.
    let mut harness = InputHarness::setup(cx);
    let selected = harness.driver.accepted_selection();

    harness.driver.open_from_menu();
    harness.driver.cancel_directory_picker();
    harness.driver.render_pending();

    assert_eq!(harness.driver.source_title(), "Flightdeck · Live");
    assert_eq!(harness.driver.accepted_selection(), selected);
    assert!(
        harness
            .driver
            .visible("source-sidebar:Live system readings")
    );
    assert!(harness.driver.visible("source-follow:Live"));
    harness.teardown();
}

#[gpui::test]
fn self_monitoring_preserves_the_paused_inspection_while_the_live_position_advances(
    cx: &mut TestAppContext,
) {
    // Navigate and copy through rendered controls while a real ring advances beyond inspection.
    let mut recording = LiveRecording::new();
    recording.publish();
    let mut harness = InputHarness::setup_source(cx, &recording.root);
    harness.driver.await_publication(1);
    harness
        .driver
        .click_source_control("source-follow:Pause live");
    harness.driver.await_publication(1);
    let requested = harness.driver.inspection_request();
    recording.publish();

    harness.driver.click_source_control("navigation:self");
    harness.driver.await_diagnostic_publication(2);
    recording.publish();
    harness.driver.await_diagnostic_publication(3);

    assert_eq!(harness.driver.inspection_request(), requested);
    assert!(!harness.driver.visible("missing-observation"));
    assert!(!harness.driver.visible("inspection-time"));
    harness.teardown();
}

#[gpui::test]
fn computer_navigation_preserves_the_accepted_diagnostic_inspection(cx: &mut TestAppContext) {
    // Metadata routes must not replace the accepted inspected observation with a null selection.
    let mut recording = LiveRecording::new();
    recording.publish();
    let mut harness = InputHarness::setup_source(cx, &recording.root);
    harness.driver.await_publication(1);
    harness
        .driver
        .click_source_control("source-follow:Pause live");
    harness.driver.await_publication(1);
    let requested = harness.driver.inspection_request();
    recording.publish();

    for _visit in 0..2 {
        harness.driver.click_source_control("navigation:computer");
        harness
            .driver
            .await_source_state(LiveDashboard::tick, |dashboard| {
                dashboard.snapshot.is_some()
            });
        harness.driver.click_source_control("navigation:self");
        harness.driver.await_diagnostic_publication(2);

        assert_eq!(harness.driver.inspection_request(), requested);
    }
    harness.teardown();
}

#[gpui::test]
fn an_opened_recording_can_follow_later_publications_without_reopening_its_reader(
    cx: &mut TestAppContext,
) {
    // Follow a real registered source after Open, then observe another publication and pause.
    let mut recording = LiveRecording::new();
    recording.publish();
    let mut harness = InputHarness::setup(cx);
    harness.driver.choose_recording(&recording.root);
    harness.driver.await_source(SourceMode::Recorded);
    harness.driver.await_publication(1);
    recording.publish();

    harness.driver.click_source_control("source-live");
    harness.driver.await_publication(2);
    recording.publish();
    harness.driver.await_publication(3);

    assert_eq!(harness.driver.source_title(), "Flightdeck · Live");
    assert!(
        harness
            .driver
            .visible("source-sidebar:Live system readings")
    );
    harness
        .driver
        .click_source_control("source-follow:Pause live");
    harness.driver.await_publication(3);
    assert_eq!(
        harness.driver.inspection_request().1.unwrap()["publication"],
        3
    );
    harness.teardown();
}

impl InputHarness {
    fn setup_source(cx: &mut TestAppContext, root: &Path) -> Self {
        let worker = Worker::start(root.to_owned()).unwrap();
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                crate::native_workflows::create_dashboard(worker, "energy".into(), None, cx)
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let context = VisualTestContext::from_window(window.into(), cx);
        Self {
            driver: InputDriver { view, context },
        }
    }
}

impl InputDriver {
    fn source_title(&mut self) -> String {
        self.context.window_title().unwrap()
    }

    fn open_time_menu(&mut self) {
        super::menu_control_bounds(&mut self.context, "time-menu");
    }

    fn visible(&mut self, selector: &'static str) -> bool {
        self.context.debug_bounds(selector).is_some()
    }

    fn click_source_control(&mut self, selector: &'static str) {
        let bounds = super::menu_control_bounds(&mut self.context, selector);
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    fn choose_recording(&mut self, root: &Path) {
        self.open_from_menu();
        self.context
            .simulate_path_prompt_response(|_| Some(vec![root.to_owned()]));
        self.view
            .update(&mut self.context, LiveDashboard::native_tick);
    }

    fn await_source(&mut self, mode: SourceMode) {
        self.await_source_state(LiveDashboard::native_tick, |dashboard| {
            dashboard.native.source == mode
        });
    }

    fn await_open_failure(&mut self) {
        self.await_source_state(LiveDashboard::native_tick, |dashboard| {
            dashboard
                .native
                .notice
                .as_ref()
                .is_some_and(|notice| notice.starts_with("Recording unchanged:"))
        });
    }

    fn await_publication(&mut self, publication: u64) {
        self.await_source_state(LiveDashboard::tick, |dashboard| {
            dashboard.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.selection["selected"]["publication"].as_u64() == Some(publication)
            })
        });
    }

    fn await_diagnostic_publication(&mut self, publication: u64) {
        self.await_source_state(LiveDashboard::tick, |dashboard| {
            dashboard.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.catalog["latest_observation"]["publication"].as_u64() == Some(publication)
            })
        });
    }

    fn await_source_state(
        &mut self,
        step: fn(&mut LiveDashboard, &mut gpui::Context<LiveDashboard>),
        ready: impl Fn(&LiveDashboard) -> bool,
    ) {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(5);
        for _attempt in 0..5001 {
            self.view.update(&mut self.context, step);
            self.context.run_until_parked();
            if self
                .view
                .read_with(&self.context, |dashboard, _| ready(dashboard))
            {
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            park_timeout(Duration::from_millis(1));
        }
        let observed = self.view.read_with(&self.context, |dashboard, _| {
            serde_json::json!({"route":dashboard.request.route,
                "second":dashboard.request.second,"identity":dashboard.request.identity,
                "generation":dashboard.generation,"error":dashboard.error,
                "notice":dashboard.native.notice,
                "snapshot_generation":dashboard.snapshot.as_ref().map(|s|s.generation),
                "latest":dashboard.snapshot.as_ref().map(|s|&s.catalog["latest_observation"]),
                "selected":dashboard.snapshot.as_ref().map(|s|&s.selection["selected"])})
        });
        panic!(
            "Source input condition did not complete after {:?}; {observed}",
            started.elapsed()
        );
    }

    fn inspection_request(&self) -> (Option<u64>, Option<Value>) {
        self.view.read_with(&self.context, |dashboard, _| {
            (dashboard.request.second, dashboard.request.identity.clone())
        })
    }
}

struct SavedRecording {
    root: PathBuf,
}

impl SavedRecording {
    fn new() -> Self {
        let root = unique_root("saved");
        let store = persistence::Store::open(&root).unwrap();
        drop(store);
        Self { root }
    }
}

impl Drop for SavedRecording {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

struct LiveRecording {
    root: PathBuf,
    writer: Writer,
    publication: u64,
}

impl LiveRecording {
    fn new() -> Self {
        let root = unique_root("live");
        let place = Place::for_user(root.file_name().unwrap().to_str().unwrap()).unwrap();
        let shape = Shape {
            slot_count: 4,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        };
        let (writer, registered) = Registration::start(&root, &place, shape, 1)
            .unwrap()
            .into_parts();
        registered.unwrap();
        Self {
            root,
            writer,
            publication: 0,
        }
    }

    fn publish(&mut self) {
        let readings = Readings {
            run_tick: self.publication,
            cpus: &[collector::machine::CPUTicks::default()],
            memory: &collector::machine::Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: collector::ring::Dropped::default(),
        };
        self.writer.publish_observed(
            &readings,
            &ObservationTimes {
                wall_ns: (1000 + self.publication) * 1_000_000_000,
                continuous_ns: (self.publication + 1) * 1_000_000_000,
                awake_ns: (self.publication + 1) * 1_000_000_000,
            },
        );
        self.publication += 1;
    }
}

impl Drop for LiveRecording {
    fn drop(&mut self) {
        self.writer.finish();
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn unique_root(kind: &str) -> PathBuf {
    let token = identity_text(random_identity().unwrap());
    std::env::temp_dir().join(format!("flightdeck-source-{kind}-{token}"))
}
