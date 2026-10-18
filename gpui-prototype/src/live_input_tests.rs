use std::collections::BTreeMap;

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::{Value, json};

use super::LiveDashboard;
use crate::live_source::{Snapshot, Update, Worker};

#[gpui::test]
fn hovering_a_repeated_second_shows_the_observation_requested_by_click(cx: &mut TestAppContext) {
    // Hover and click the same repeated second, reading the headline from GPUI's rendered frame.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();

    harness.driver.hover_previous_second();
    let latest_reading_visible = harness.driver.energy_reading_is_visible("90.0 W");
    let previous_reading_visible = harness.driver.energy_reading_is_visible("10.0 W");
    harness.driver.click_previous_second();

    assert!(
        latest_reading_visible,
        "90 W visible: {latest_reading_visible}; 10 W visible: {previous_reading_visible}; \
         clicked publication: {:?}",
        harness.driver.requested_publication()
    );
    assert!(!previous_reading_visible);
    assert_eq!(harness.driver.requested_second(), Some(999));
    assert_eq!(harness.driver.requested_publication(), Some(3));
    harness.teardown();
}

#[gpui::test]
fn repeated_arrows_keep_inspecting_after_pending_readings_disappear(cx: &mut TestAppContext) {
    // Dispatch through the rendered chart while withholding the reader's subsequent output.
    let mut harness = InputHarness::setup(cx);
    harness.driver.focus_chart();

    harness.driver.press("right");
    assert_eq!(harness.driver.requested_second(), Some(1001));
    harness.driver.render_pending();
    harness.driver.press("right");
    assert_eq!(harness.driver.requested_second(), Some(1002));
    harness.driver.press("left");

    assert_eq!(harness.driver.requested_second(), Some(1001));
    assert!(harness.driver.readings_are_hidden());
    assert!(harness.driver.copy_selected().is_err());
    assert!(harness.driver.inspection_has_focus());
    harness.teardown();
}

#[gpui::test]
fn clicking_history_keeps_inspection_available_while_readings_load(cx: &mut TestAppContext) {
    // A chart click must keep its keyboard owner mounted after the selected values are hidden.
    let mut harness = InputHarness::setup(cx);

    harness.driver.click_selected_second();
    assert_eq!(harness.driver.requested_second(), Some(1000));
    harness.driver.render_pending();
    harness.driver.press("right");

    assert_eq!(harness.driver.requested_second(), Some(1001));
    assert!(harness.driver.readings_are_hidden());
    assert!(harness.driver.inspection_has_focus());
    harness.teardown();
}

#[gpui::test]
fn installed_open_action_requests_one_recording_directory(cx: &mut TestAppContext) {
    // Exercise the production action registration rather than inspecting a menu factory.
    let mut harness = InputHarness::setup(cx);
    let previous = harness.driver.accepted_selection();

    harness.driver.open_from_menu();

    assert!(harness.driver.picker_is_open());
    harness.driver.cancel_directory_picker();
    assert_eq!(harness.driver.accepted_selection(), previous);
    harness.teardown();
}

#[gpui::test]
fn installed_command_o_requests_one_recording_directory(cx: &mut TestAppContext) {
    // Keyboard dispatch needs both the production binding and installed action handler.
    let mut harness = InputHarness::setup(cx);
    let previous = harness.driver.accepted_selection();

    harness.driver.press("cmd-o");

    assert!(harness.driver.picker_is_open());
    harness.driver.cancel_directory_picker();
    assert_eq!(harness.driver.accepted_selection(), previous);
    harness.teardown();
}

#[gpui::test]
fn local_clock_entry_can_replace_a_pending_selection(cx: &mut TestAppContext) {
    // Retained context resolves a valid local time without restoring stale presented values.
    let mut harness = InputHarness::setup(cx);
    harness.driver.select_second(1001);

    harness
        .driver
        .enter_time(&crate::live_presentation::clock(999, true));

    assert_eq!(harness.driver.requested_second(), Some(999));
    assert!(harness.driver.readings_are_hidden());
    assert!(harness.driver.copy_selected().is_err());
    harness.teardown();
}

#[gpui::test]
fn local_clock_entry_recovers_after_the_pending_selection_fails(cx: &mut TestAppContext) {
    // A failed read retains navigation context while its reading and copy payload stay absent.
    let mut harness = InputHarness::setup(cx);
    harness.driver.select_second(1001);
    harness.driver.fail_pending_selection();

    harness
        .driver
        .enter_time(&crate::live_presentation::clock(999, true));

    assert_eq!(harness.driver.requested_second(), Some(999));
    assert!(harness.driver.readings_are_hidden());
    assert!(harness.driver.copy_selected().is_err());
    harness.teardown();
}

#[gpui::test]
fn exact_unix_entry_can_request_a_gap_after_a_failed_selection(cx: &mut TestAppContext) {
    // Exact timestamps remain usable even when retained history contains no matching point.
    let mut harness = InputHarness::setup(cx);
    harness.driver.select_second(1001);
    harness.driver.fail_pending_selection();

    harness.driver.enter_time("1004");

    assert_eq!(harness.driver.requested_second(), Some(1004));
    assert!(harness.driver.readings_are_hidden());
    assert!(harness.driver.copy_selected().is_err());
    harness.teardown();
}

struct InputHarness {
    driver: InputDriver,
}

impl InputHarness {
    fn setup(cx: &mut TestAppContext) -> Self {
        let root =
            std::env::temp_dir().join(format!("flightdeck-input-contract-{}", std::process::id()));
        let worker = Worker::start(root).unwrap();
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                crate::native_workflows::create_dashboard(worker, "energy".into(), None, cx)
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let context = VisualTestContext::from_window(window.into(), cx);
        let mut harness = Self {
            driver: InputDriver { view, context },
        };
        harness.driver.seed_accepted_selection();
        harness
    }

    fn teardown(self) {
        drop(self);
    }
}

impl Drop for InputHarness {
    fn drop(&mut self) {
        self.driver.context.update(|window, cx| {
            self.driver
                .view
                .update(cx, |dashboard, _| dashboard.worker.stop());
            window.remove_window();
        });
        self.driver.context.run_until_parked();
    }
}

struct InputDriver {
    view: Entity<LiveDashboard>,
    context: VisualTestContext,
}

impl InputDriver {
    fn seed_repeated_second(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let mut previous = snapshot.selection["trends"][0].clone();
            previous["wall_ns"] = json!(999_100_000_000_u64);
            previous["publication"] = json!(2);
            previous["power_watts"] = json!(10);
            let mut latest = previous.clone();
            latest["wall_ns"] = json!(999_750_000_000_u64);
            latest["publication"] = json!(3);
            latest["power_watts"] = json!(90);
            let mut selected = snapshot.selection["trends"][1].clone();
            selected["publication"] = json!(4);
            snapshot.selection["selected"]["publication"] = json!(4);
            snapshot.selection["trends"] = json!([previous, latest, selected]);

            dashboard.request.identity = Some(snapshot.selection["selected"].clone());
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn seed_accepted_selection(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = accepted_snapshot(dashboard.generation);
            dashboard.request.second = Some(1000);
            dashboard.request.identity = Some(snapshot.selection["selected"].clone());
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn focus_chart(&mut self) {
        // Right mouse focus leaves the accepted second intact; left clicks request a selection.
        let bounds = self.context.debug_bounds("live-energy-plot").unwrap();
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
    }

    fn press(&mut self, keys: &str) {
        self.context.simulate_keystrokes(keys);
    }

    fn previous_second_position(&mut self) -> gpui::Point<gpui::Pixels> {
        let bounds = self.context.debug_bounds("live-energy-plot").unwrap();
        let plot = crate::charts::plot_bounds(bounds);
        gpui::point(
            plot.left() + plot.size.width * (898.5 / 900.0),
            plot.center().y,
        )
    }

    fn hover_previous_second(&mut self) {
        let position = self.previous_second_position();
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
    }

    fn click_previous_second(&mut self) {
        let position = self.previous_second_position();
        self.context.simulate_click(position, Modifiers::none());
    }

    fn energy_reading_is_visible(&mut self, reading: &'static str) -> bool {
        self.context.debug_bounds(reading).is_some()
    }

    fn requested_publication(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.request.identity.as_ref()?["publication"].as_u64()
        })
    }

    fn click_selected_second(&mut self) {
        let bounds = self.context.debug_bounds("live-energy-plot").unwrap();
        let plot = crate::charts::plot_bounds(bounds);
        let selected = gpui::point(plot.right() - gpui::px(0.1), plot.center().y);
        self.context.simulate_click(selected, Modifiers::none());
    }

    fn render_pending(&mut self) {
        self.context.refresh().unwrap();
        self.context.run_until_parked();
    }

    fn open_from_menu(&mut self) {
        self.context
            .dispatch_action(crate::native_workflows::OpenRecording);
    }

    fn picker_is_open(&self) -> bool {
        self.context.did_prompt_for_paths()
    }

    fn cancel_directory_picker(&mut self) {
        self.context.simulate_path_prompt_response(|options| {
            assert!(options.directories);
            assert!(!options.files);
            assert!(!options.multiple);
            None
        });
        self.view
            .update(&mut self.context, LiveDashboard::native_tick);
        assert!(!self.context.did_prompt_for_paths());
    }

    fn select_second(&mut self, second: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.inspect(second, cx);
        });
    }

    fn enter_time(&mut self, text: &str) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.inspect_text(text, cx);
        });
    }

    fn fail_pending_selection(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.receive(Update::Failed {
                generation: dashboard.generation,
                message: "Recording read failed".into(),
            }));
            cx.notify();
        });
    }

    fn requested_second(&self) -> Option<u64> {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.request.second)
    }

    fn accepted_selection(&self) -> Option<Value> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.selection.clone())
        })
    }

    fn readings_are_hidden(&self) -> bool {
        self.view.read_with(&self.context, |dashboard, _| {
            if dashboard.snapshot.is_some() {
                return false;
            }

            dashboard.panels.is_empty()
        })
    }

    fn copy_selected(&self) -> Result<String, String> {
        self.view.read_with(&self.context, |dashboard, _| {
            crate::native_workflows::selected_copy(
                dashboard.snapshot.as_deref(),
                &dashboard.request,
                dashboard.expanded.as_ref(),
            )
        })
    }

    fn inspection_has_focus(&mut self) -> bool {
        self.context.update(|window, cx| {
            self.view
                .read(cx)
                .inspection_focus
                .as_ref()
                .is_some_and(|focus| focus.is_focused(window))
        })
    }
}

fn accepted_snapshot(generation: u64) -> Box<Snapshot> {
    let identity = json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004", "publication":2});
    let previous = json!({"run":identity["run"],"boot":identity["boot"],
        "publication":1,"wall_ns":999_000_000_000_u64,
        "segment":0,"power_watts":8.0,"gpu_watts":2.0});
    let selected = json!({"run":identity["run"],"boot":identity["boot"],
        "publication":2,"wall_ns":1_000_000_000_000_u64,
        "segment":0,"power_watts":8.0,"gpu_watts":2.0});
    Box::new(Snapshot {
        generation,
        catalog: json!({"first_second":900,"last_second":1100}),
        selection: json!({"second":1000,"selected":identity,
            "trends":[previous, selected]}),
        pages: BTreeMap::new(),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        cpu_history: None,
        read_ms: 0.0,
    })
}
