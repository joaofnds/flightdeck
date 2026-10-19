use std::collections::BTreeMap;

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::{Value, json};

use super::LiveDashboard;
use crate::live_source::{Snapshot, Update, Worker};

#[path = "live_chart_inspector_input_tests.rs"]
mod chart_inspector;
#[path = "live_computer_input_tests.rs"]
mod computer;
#[path = "live_cpu_layout_input_tests.rs"]
mod cpu_layout;
#[path = "live_disk_input_tests.rs"]
mod disk_attribution;
#[path = "live_hardware_input_tests.rs"]
mod hardware;
#[path = "live_header_input_tests.rs"]
mod header;
#[path = "live_layout_input_tests.rs"]
mod layout;
#[path = "live_rankings_input_tests.rs"]
mod rankings;
#[path = "live_resource_input_tests.rs"]
mod resource;
#[path = "live_section_input_tests.rs"]
mod sections;
#[path = "live_source_mode_input_tests.rs"]
mod source_mode;
#[path = "live_summary_input_tests.rs"]
mod summary;

#[gpui::test]
fn hovering_history_keeps_the_cursor_within_the_plot(cx: &mut TestAppContext) {
    // Inspect the actual painted cursor after dispatching through the rendered history chart.
    let mut harness = InputHarness::setup(cx);

    harness.driver.hover_previous_second();

    harness.driver.assert_energy_cursor_tracks_plot();
    harness.teardown();
}

#[gpui::test]
fn inspection_cursor_is_hidden_before_any_plot_hover(cx: &mut TestAppContext) {
    // Read the composed frame before input so an accepted selection cannot imply a hover.
    let mut harness = InputHarness::setup(cx);

    assert!(!harness.driver.energy_cursor_is_visible());
    harness.teardown();
}

#[gpui::test]
fn hovering_overview_updates_the_status_widgets_to_the_graph_timestamp(cx: &mut TestAppContext) {
    // Dispatch a historical graph hover while observing the exact painted pressure/swap text.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_distinct_overview_timestamps();
    assert!(harness.driver.reading_is_visible("87.0 %"));
    assert!(
        harness
            .driver
            .reading_is_visible("Memory pressure: Normal    Swap: 7.0 GiB")
    );

    harness.driver.hover_previous_second_on("live-cpu-plot");

    assert!(harness.driver.reading_is_visible("13.0 %"));
    let historical = harness
        .driver
        .reading_is_visible("Memory pressure: Critical    Swap: 3.0 GiB");
    let current = harness
        .driver
        .reading_is_visible("Memory pressure: Normal    Swap: 7.0 GiB");
    assert!(
        historical,
        "Historical CPU is visible, but historical pressure/swap is {historical}; \
        current pressure/swap is still visible: {current}"
    );
    assert!(!current);
    harness.teardown();
}

#[gpui::test]
fn resizing_the_window_retains_the_reading_beneath_a_stationary_pointer(cx: &mut TestAppContext) {
    // Grow the viewport without moving the pointer out of its original plotted timestamp.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_distinct_overview_timestamps();
    harness.driver.hover_previous_second_on("live-cpu-plot");
    assert!(harness.driver.reading_is_visible("13.0 %"));
    let pointer = harness.driver.pointer_position();

    harness
        .driver
        .resize_with_retained_pointer(gpui::size(gpui::px(1536.0), gpui::px(2500.0)), pointer);

    assert!(
        harness
            .driver
            .plot_bounds("live-cpu-plot")
            .contains(&pointer)
    );
    assert_eq!(harness.driver.pointer_position(), pointer);
    assert!(harness.driver.cursor_is_visible("live-cpu-plot"));
    assert!(harness.driver.reading_is_visible("13.0 %"));
    assert!(!harness.driver.reading_is_visible("87.0 %"));
    harness.teardown();
}

#[gpui::test]
fn widening_the_window_recomputes_the_timestamp_beneath_a_stationary_pointer(
    cx: &mut TestAppContext,
) {
    // Widen the actual plot so the same pointer moves from a measured second into a gap.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_distinct_overview_timestamps();
    harness.driver.hover_previous_second_on("live-cpu-plot");
    assert!(harness.driver.reading_is_visible("13.0 %"));
    let pointer = harness.driver.pointer_position();

    harness
        .driver
        .resize_with_retained_pointer(gpui::size(gpui::px(1736.0), gpui::px(2400.0)), pointer);

    let plot = harness.driver.plot_bounds("live-cpu-plot");
    assert!(plot.contains(&pointer));
    assert_eq!(plot.left(), gpui::px(249.5));
    assert_eq!(plot.size.width, gpui::px(428.0));
    assert_eq!(harness.driver.pointer_position(), pointer);
    assert_eq!(
        harness.driver.chart_accessibility_reading("live-cpu").0,
        Some(859)
    );
    assert!(harness.driver.cursor_is_visible("live-cpu-plot"));
    assert!(harness.driver.reading_is_visible("Unavailable"));
    assert!(!harness.driver.reading_is_visible("13.0 %"));
    harness.teardown();
}

#[gpui::test]
fn shrinking_the_viewport_beyond_the_pointer_ends_the_hover(cx: &mut TestAppContext) {
    // Keep the physical pointer fixed while the clipped viewport stops containing it.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_distinct_overview_timestamps();
    harness.driver.hover_previous_second_on("live-cpu-plot");
    assert!(harness.driver.cursor_is_visible("live-cpu-plot"));
    let pointer = harness.driver.pointer_position();

    harness
        .driver
        .resize_with_retained_pointer(gpui::size(gpui::px(1536.0), gpui::px(300.0)), pointer);

    assert!(pointer.y > gpui::px(300.0));
    assert_eq!(harness.driver.pointer_position(), pointer);
    assert!(!harness.driver.cursor_is_visible("live-cpu-plot"));
    harness.teardown();
}

#[gpui::test]
fn chart_accessibility_metadata_matches_the_hovered_reading_and_gap(cx: &mut TestAppContext) {
    // Dispatch real hover input, then read the exact values passed to production aria setters.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    harness.driver.hover_previous_second_on("live-energy-plot");
    assert!(harness.driver.reading_is_visible("90.0 W"));

    let (second, reading) = harness.driver.chart_accessibility_reading("live-energy");
    assert_eq!(second, Some(999));
    assert!(reading.contains("Unix second 999"));
    assert!(reading.ends_with("90.0 W"));

    let gap = harness
        .driver
        .second_position("live-energy-plot", 897.5 / 900.0);
    harness.driver.hover_position(gap);
    assert!(harness.driver.reading_is_visible("Unavailable"));
    let (second, reading) = harness.driver.chart_accessibility_reading("live-energy");
    assert_eq!(second, Some(998));
    assert!(reading.ends_with("Unavailable"));
    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Sidebar);
    let (second, reading) = harness.driver.chart_accessibility_reading("live-energy");
    assert_eq!(second, Some(1000));
    assert!(reading.ends_with("8.0 W"));
    harness.teardown();
}

#[gpui::test]
fn moving_directly_from_the_first_plot_keeps_both_cursors_synchronized(cx: &mut TestAppContext) {
    // Deliver one move across sibling plots, then inspect the settled scene without more input.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    harness.driver.hover_selected_second_on("live-energy-plot");
    assert!(harness.driver.reading_is_visible("8.0 W"));

    harness
        .driver
        .hover_previous_second_on("live-gpu-power-plot");

    assert!(harness.driver.reading_is_visible("90.0 W"));
    assert!(harness.driver.reading_is_visible("60.0 °C"));
    harness.driver.assert_cursors_track_previous_second();
    assert!(harness.driver.cursor_is_visible("live-energy-plot"));
    assert!(harness.driver.cursor_is_visible("live-gpu-power-plot"));
    harness.teardown();
}

#[gpui::test]
fn moving_directly_from_the_second_plot_keeps_both_cursors_synchronized(cx: &mut TestAppContext) {
    // Reverse the sibling transition because event-listener order differs from the forward move.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    harness
        .driver
        .hover_selected_second_on("live-gpu-power-plot");
    assert!(harness.driver.reading_is_visible("8.0 W"));

    harness.driver.hover_previous_second_on("live-energy-plot");

    assert!(harness.driver.reading_is_visible("90.0 W"));
    assert!(harness.driver.reading_is_visible("60.0 °C"));
    harness.driver.assert_cursors_track_previous_second();
    assert!(harness.driver.cursor_is_visible("live-energy-plot"));
    assert!(harness.driver.cursor_is_visible("live-gpu-power-plot"));
    harness.teardown();
}

#[gpui::test]
fn scrolling_a_plot_away_from_a_stationary_pointer_hides_both_cursors(cx: &mut TestAppContext) {
    // A wheel event moves only layout: the pointer leaves the plot but stays inside its wrapper.
    let mut harness = InputHarness::setup(cx);
    harness.driver.make_scrollable(850.0);
    let plot = harness.driver.plot_bounds("live-energy-plot");
    let pointer = gpui::point(plot.center().x, plot.bottom() - gpui::px(1.0));
    harness.driver.hover_position(pointer);
    assert!(harness.driver.cursor_is_visible("live-energy-plot"));
    assert!(harness.driver.cursor_is_visible("live-gpu-power-plot"));

    harness.driver.scroll_at(pointer, -2.0);

    let moved = harness.driver.plot_bounds("live-energy-plot");
    assert_eq!(moved.bottom(), plot.bottom() - gpui::px(2.0));
    assert!(!moved.contains(&pointer));
    assert!(
        harness
            .driver
            .chart_bounds("live-energy-plot")
            .contains(&pointer)
    );
    assert_eq!(harness.driver.pointer_position(), pointer);
    assert!(!harness.driver.cursor_is_visible("live-energy-plot"));
    assert!(!harness.driver.cursor_is_visible("live-gpu-power-plot"));
    harness.teardown();
}

#[gpui::test]
fn moving_above_the_plot_hides_both_inspection_cursors(cx: &mut TestAppContext) {
    // Leave the top edge after positively establishing synchronized cursors.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.assert_cursors_visible();

    harness.driver.leave_plot("live-energy-plot", PlotExit::Top);

    harness.driver.assert_cursors_hidden();
    harness.teardown();
}

#[gpui::test]
fn moving_below_the_plot_hides_both_inspection_cursors(cx: &mut TestAppContext) {
    // Leave the bottom edge while remaining in the larger chart wrapper.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.assert_cursors_visible();

    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Bottom);

    harness.driver.assert_cursors_hidden();
    harness.teardown();
}

#[gpui::test]
fn moving_left_of_the_plot_hides_both_inspection_cursors(cx: &mut TestAppContext) {
    // Leave the actual plot through its label margin.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.assert_cursors_visible();

    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Left);

    harness.driver.assert_cursors_hidden();
    harness.teardown();
}

#[gpui::test]
fn moving_right_of_the_plot_hides_both_inspection_cursors(cx: &mut TestAppContext) {
    // Leave the actual plot through its right margin.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.assert_cursors_visible();

    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Right);

    harness.driver.assert_cursors_hidden();
    harness.teardown();
}

#[gpui::test]
fn moving_onto_timestamps_hides_both_inspection_cursors(cx: &mut TestAppContext) {
    // Timestamp labels stay outside graph hover even within the same panel.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.assert_cursors_visible();

    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Timestamp);

    harness.driver.assert_cursors_hidden();
    harness.teardown();
}

#[gpui::test]
fn moving_onto_the_legend_hides_both_inspection_cursors(cx: &mut TestAppContext) {
    // A legend remains outside graph hover even within the same panel.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.assert_cursors_visible();

    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Legend);

    harness.driver.assert_cursors_hidden();
    harness.teardown();
}

#[gpui::test]
fn moving_onto_the_sidebar_hides_both_inspection_cursors(cx: &mut TestAppContext) {
    // Sidebar movement clears both synchronized cursors.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.assert_cursors_visible();

    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Sidebar);

    harness.driver.assert_cursors_hidden();
    harness.teardown();
}

#[gpui::test]
fn leaving_the_second_plot_restores_selected_readings_and_hides_both_cursors(
    cx: &mut TestAppContext,
) {
    // Clear the destination hover after a direct move and restore both accepted readouts.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    harness.driver.hover_selected_second_on("live-energy-plot");
    harness
        .driver
        .hover_previous_second_on("live-gpu-power-plot");
    assert!(harness.driver.reading_is_visible("90.0 W"));
    assert!(harness.driver.reading_is_visible("60.0 °C"));

    harness
        .driver
        .leave_plot("live-gpu-power-plot", PlotExit::Bottom);

    harness.driver.assert_cursors_hidden();
    assert!(harness.driver.reading_is_visible("8.0 W"));
    assert!(harness.driver.reading_is_visible("40.0 °C"));
    harness.teardown();
}

#[gpui::test]
fn scrolling_the_plot_back_under_the_pointer_restores_synchronized_hover(cx: &mut TestAppContext) {
    // Reverse layout movement so reentry cannot depend on another pointer event.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    // The shorter chart cards fit a 1500-pixel window, so a 1000-pixel one keeps it scrollable.
    harness.driver.make_scrollable(1000.0);
    let plot = harness.driver.plot_bounds("live-energy-plot");
    let previous = harness.driver.previous_second_position();
    let pointer = gpui::point(previous.x, plot.bottom() - gpui::px(1.0));
    harness.driver.hover_position(pointer);
    harness.driver.scroll_at(pointer, -2.0);
    harness.driver.assert_cursors_hidden();
    assert!(harness.driver.reading_is_visible("8.0 W"));

    harness.driver.scroll_at(pointer, 2.0);

    assert_eq!(harness.driver.pointer_position(), pointer);
    assert_eq!(harness.driver.plot_bounds("live-energy-plot"), plot);
    harness.driver.assert_cursors_visible();
    assert!(harness.driver.reading_is_visible("90.0 W"));
    assert!(harness.driver.reading_is_visible("60.0 °C"));
    harness.teardown();
}

#[gpui::test]
fn hovering_a_repeated_second_shows_the_observation_requested_by_click(cx: &mut TestAppContext) {
    // Hover and click the same repeated second, reading the headline from GPUI's rendered frame.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();

    harness.driver.hover_previous_second();
    let latest_reading_visible = harness.driver.reading_is_visible("90.0 W");
    let previous_reading_visible = harness.driver.reading_is_visible("10.0 W");
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
        Self::setup_route(cx, "energy")
    }

    fn setup_route(cx: &mut TestAppContext, route: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("flightdeck-input-contract-{}", std::process::id()));
        let worker = Worker::start(root).unwrap();
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                crate::native_workflows::create_dashboard(worker, route.into(), None, cx)
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

#[derive(Clone, Copy)]
enum PlotExit {
    Top,
    Bottom,
    Left,
    Right,
    Timestamp,
    Legend,
    Sidebar,
}

impl InputDriver {
    fn resize_with_retained_pointer(
        &mut self,
        size: gpui::Size<gpui::Pixels>,
        pointer: gpui::Point<gpui::Pixels>,
    ) {
        self.context.update(|window, cx| {
            window.resize(size);
            window.bounds_changed(cx);
            // TestPlatform reports pointer zero on resize; restore its saved coordinate
            // without a MouseMove, in the same input batch before layout is rendered.
            window.dispatch_event(
                gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                    position: pointer,
                    delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(0.0))),
                    modifiers: Modifiers::none(),
                    touch_phase: gpui::TouchPhase::Moved,
                }),
                cx,
            );
        });
        self.context.run_until_parked();
    }

    fn seed_distinct_overview_timestamps(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let mut historical = snapshot.selection["trends"][0].clone();
            historical["cpu_percent"] = json!(13);
            historical["pressure"] = json!(2);
            historical["swap_used_bytes"] = json!(3_221_225_472_u64);
            let mut current = snapshot.selection["trends"][1].clone();
            current["cpu_percent"] = json!(87);
            current["pressure"] = json!(0);
            current["swap_used_bytes"] = json!(7_516_192_768_u64);
            snapshot.selection["summary"] = current.clone();
            snapshot.selection["trends"] = json!([historical, current]);

            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2400.0)));
        self.context.run_until_parked();
    }

    fn seed_repeated_second(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let mut previous = snapshot.selection["trends"][0].clone();
            previous["wall_ns"] = json!(999_100_000_000_u64);
            previous["publication"] = json!(2);
            previous["power_watts"] = json!(10);
            previous["soc_celsius"] = json!(20);
            let mut latest = previous.clone();
            latest["wall_ns"] = json!(999_750_000_000_u64);
            latest["publication"] = json!(3);
            latest["power_watts"] = json!(90);
            latest["soc_celsius"] = json!(60);
            let mut selected = snapshot.selection["trends"][1].clone();
            selected["publication"] = json!(4);
            selected["soc_celsius"] = json!(40);
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
        self.second_position("live-energy-plot", 898.5 / 900.0)
    }

    fn second_position(
        &mut self,
        selector: &'static str,
        fraction: f32,
    ) -> gpui::Point<gpui::Pixels> {
        let plot = self.plot_bounds(selector);
        gpui::point(plot.left() + plot.size.width * fraction, plot.center().y)
    }

    fn hover_selected_second_on(&mut self, selector: &'static str) {
        let position = self.second_position(selector, 899.5 / 900.0);
        self.hover_position(position);
    }

    fn hover_previous_second_on(&mut self, selector: &'static str) {
        let position = self.second_position(selector, 898.5 / 900.0);
        self.hover_position(position);
    }

    fn hover_previous_second(&mut self) {
        let position = self.previous_second_position();
        self.hover_position(position);
    }

    fn hover_position(&mut self, position: gpui::Point<gpui::Pixels>) {
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
    }

    fn leave_plot(&mut self, selector: &'static str, exit: PlotExit) {
        let bounds = self.chart_bounds(selector);
        let plot = crate::charts::plot_bounds(bounds);
        let position = match exit {
            PlotExit::Top => gpui::point(plot.center().x, plot.top() - gpui::px(1.0)),
            PlotExit::Bottom => gpui::point(plot.center().x, plot.bottom() + gpui::px(1.0)),
            PlotExit::Left => gpui::point(plot.left() - gpui::px(1.0), plot.center().y),
            PlotExit::Right => gpui::point(plot.right() + gpui::px(1.0), plot.center().y),
            PlotExit::Timestamp => gpui::point(plot.center().x, bounds.bottom() + gpui::px(10.0)),
            PlotExit::Legend => gpui::point(plot.center().x, bounds.bottom() + gpui::px(40.0)),
            PlotExit::Sidebar => gpui::point(gpui::px(20.0), plot.center().y),
        };
        self.hover_position(position);
    }

    fn energy_cursor_is_visible(&mut self) -> bool {
        self.cursor_is_visible("live-energy-plot")
    }

    fn chart_bounds(&mut self, selector: &'static str) -> gpui::Bounds<gpui::Pixels> {
        self.context.debug_bounds(selector).unwrap()
    }

    fn plot_bounds(&mut self, selector: &'static str) -> gpui::Bounds<gpui::Pixels> {
        crate::charts::plot_bounds(self.chart_bounds(selector))
    }

    fn make_scrollable(&mut self, height: f32) {
        self.context
            .simulate_resize(gpui::size(gpui::px(1200.0), gpui::px(height)));
        self.context.run_until_parked();
    }

    fn scroll_at(&mut self, position: gpui::Point<gpui::Pixels>, delta_y: f32) {
        self.context.simulate_event(gpui::ScrollWheelEvent {
            position,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(delta_y))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
    }

    fn pointer_position(&mut self) -> gpui::Point<gpui::Pixels> {
        self.context.update(|window, _| window.mouse_position())
    }

    fn cursor_is_visible(&mut self, selector: &'static str) -> bool {
        let Some(bounds) = self.context.debug_bounds(selector) else {
            return false;
        };

        let plot = crate::charts::plot_bounds(bounds);
        self.context.update(|window, _| {
            let expected =
                gpui::Bounds::new(plot.origin, gpui::size(gpui::px(1.0), plot.size.height))
                    .scale(window.scale_factor());
            let right = plot.right().scale(window.scale_factor());
            let cursor = gpui::Background::from(crate::charts::cursor_color());
            window.painted_quads().into_iter().any(|quad| {
                // Time gridlines share the cursor's shape, so only its colour tells them apart.
                if quad.background != cursor {
                    return false;
                }
                if quad.bounds.size != expected.size {
                    return false;
                }
                if (quad.bounds.top() - expected.top()).as_f32().abs() > 1.0 {
                    return false;
                }
                (expected.left() - expected.size.width..=right).contains(&quad.bounds.left())
            })
        })
    }

    fn assert_energy_cursor_tracks_plot(&mut self) {
        self.assert_cursor_tracks_previous_second("live-energy-plot");
    }

    fn assert_cursors_track_previous_second(&mut self) {
        self.assert_cursor_tracks_previous_second("live-energy-plot");
        self.assert_cursor_tracks_previous_second("live-gpu-power-plot");
    }

    fn assert_cursors_visible(&mut self) {
        assert!(self.cursor_is_visible("live-energy-plot"));
        assert!(self.cursor_is_visible("live-gpu-power-plot"));
    }

    fn assert_cursors_hidden(&mut self) {
        assert!(!self.cursor_is_visible("live-energy-plot"));
        assert!(!self.cursor_is_visible("live-gpu-power-plot"));
    }

    fn assert_cursor_tracks_previous_second(&mut self, selector: &'static str) {
        self.assert_cursor_tracks_fraction(selector, 898.0 / 900.0);
    }

    fn assert_cursor_tracks_fraction(&mut self, selector: &'static str, fraction: f32) {
        let plot = self.plot_bounds(selector);
        let x = plot.left() + plot.size.width * fraction;

        let (expected, cursor) = self.context.update(|window, _| {
            let expected = gpui::Bounds::new(
                gpui::point(x, plot.top()),
                gpui::size(gpui::px(1.0), plot.size.height),
            )
            .scale(window.scale_factor());
            let cursor = window
                .painted_quads()
                .into_iter()
                .find(|quad| {
                    if quad.bounds.size != expected.size {
                        return false;
                    }

                    if (quad.bounds.top() - expected.top()).as_f32().abs() > 1.0 {
                        return false;
                    }

                    (quad.bounds.left() - expected.left()).as_f32().abs() <= 1.0
                })
                .map(|quad| quad.bounds);

            (expected, cursor)
        });
        let cursor = cursor.expect("The hovered history must paint its inspection cursor");

        assert!(
            (cursor.top() - expected.top()).as_f32().abs() <= 1.0,
            "Cursor {cursor:?} must start at the plot {expected:?}"
        );
        assert!(
            (cursor.bottom() - expected.bottom()).as_f32().abs() <= 1.0,
            "Cursor must end at the plot, above its timestamp and legend"
        );
    }

    fn click_previous_second(&mut self) {
        let position = self.previous_second_position();
        self.context.simulate_click(position, Modifiers::none());
    }

    fn reading_is_visible(&mut self, reading: &'static str) -> bool {
        self.context.debug_bounds(reading).is_some()
    }

    fn chart_accessibility_reading(&self, panel_id: &str) -> (Option<u64>, String) {
        self.view.read_with(&self.context, |dashboard, _| {
            let panel = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == panel_id)
                .unwrap();
            dashboard.chart_accessibility_reading(panel)
        })
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
        resident_history: None,
        app_attribution_history: None,
        overview_app_histories: None,
        process_history: None,
        rankings: None,
        read_ms: 0.0,
    })
}
