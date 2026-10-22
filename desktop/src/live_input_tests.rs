use std::collections::BTreeMap;

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::{Value, json};

use super::LiveDashboard;
use crate::live_source::{Snapshot, Update, Worker};

/// Opens the time menu when `selector` names a control inside it, and returns the control's
/// bounds, so tests reach those controls the way a person does.
pub(crate) fn menu_control_bounds(
    context: &mut VisualTestContext,
    selector: &'static str,
) -> gpui::Bounds<gpui::Pixels> {
    if context.debug_bounds(selector).is_none() {
        let picker = context.debug_bounds("time-range").unwrap();
        context.simulate_click(picker.center(), Modifiers::none());
        context.run_until_parked();
    }
    context.debug_bounds(selector).expect(selector)
}

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
#[path = "live_layout_input_tests.rs"]
mod layout;
#[path = "live_loading_input_tests.rs"]
mod loading;
#[path = "live_range_input_tests.rs"]
mod range;
#[path = "live_rankings_input_tests.rs"]
mod rankings;
#[path = "live_resource_input_tests.rs"]
mod resource;
#[path = "live_scroll_input_tests.rs"]
mod scroll;
#[path = "live_section_input_tests.rs"]
mod sections;
#[path = "live_source_mode_input_tests.rs"]
pub(crate) mod source_mode;
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
    // Dispatch a historical graph hover while observing the exact painted pressure caption.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_distinct_overview_timestamps();
    assert!(harness.driver.reading_is_visible("87%"));
    assert!(
        harness
            .driver
            .reading_is_visible("chart-caption:live-memory-pressure:Normal")
    );

    harness.driver.hover_previous_second_on("live-cpu-plot");

    assert!(harness.driver.reading_is_visible("13%"));
    let historical = harness
        .driver
        .reading_is_visible("chart-caption:live-memory-pressure:Critical");
    let current = harness
        .driver
        .reading_is_visible("chart-caption:live-memory-pressure:Normal");
    assert!(
        historical,
        "Historical CPU is visible, but historical pressure is {historical}; \
        current pressure is still visible: {current}"
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
    assert!(harness.driver.reading_is_visible("13%"));
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
    assert!(harness.driver.reading_is_visible("13%"));
    assert!(!harness.driver.reading_is_visible("87%"));
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
    assert!(harness.driver.reading_is_visible("13%"));
    let pointer = harness.driver.pointer_position();

    harness
        .driver
        .resize_with_retained_pointer(gpui::size(gpui::px(1736.0), gpui::px(2400.0)), pointer);

    let plot = harness.driver.plot_bounds("live-cpu-plot");
    assert!(plot.contains(&pointer));
    assert_eq!(plot.left(), gpui::px(247.0));
    assert_eq!(plot.size.width, gpui::px(422.5));
    assert_eq!(harness.driver.pointer_position(), pointer);
    assert_eq!(
        harness.driver.chart_accessibility_reading("live-cpu").0,
        Some(858)
    );
    assert!(harness.driver.cursor_is_visible("live-cpu-plot"));
    assert!(harness.driver.reading_is_visible("Unavailable"));
    assert!(!harness.driver.reading_is_visible("13%"));
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
    assert!(harness.driver.reading_is_visible("90 W"));

    let (second, reading) = harness.driver.chart_accessibility_reading("live-energy");
    assert_eq!(second, Some(999));
    assert!(reading.contains("Unix second 999"));
    assert!(reading.ends_with("90 W"));

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
    assert!(reading.ends_with("8 W"));
    harness.teardown();
}

#[gpui::test]
fn moving_directly_from_the_first_plot_keeps_both_cursors_synchronized(cx: &mut TestAppContext) {
    // Deliver one move across sibling plots, then inspect the settled scene without more input.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    harness.driver.hover_selected_second_on("live-energy-plot");
    assert!(harness.driver.reading_is_visible("8 W"));

    harness
        .driver
        .hover_previous_second_on("live-gpu-power-plot");

    assert!(harness.driver.reading_is_visible("90 W"));
    assert!(harness.driver.reading_is_visible("60 °C"));
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
    assert!(harness.driver.reading_is_visible("8 W"));

    harness.driver.hover_previous_second_on("live-energy-plot");

    assert!(harness.driver.reading_is_visible("90 W"));
    assert!(harness.driver.reading_is_visible("60 °C"));
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
    let page = harness.driver.plot_bounds("live-energy-plot").center();
    harness.driver.scroll_at(page, -10.0);
    let plot = harness.driver.plot_bounds("live-energy-plot");
    let pointer = gpui::point(plot.center().x, plot.top() + gpui::px(1.0));
    harness.driver.hover_position(pointer);
    assert!(harness.driver.cursor_is_visible("live-energy-plot"));
    assert!(harness.driver.cursor_is_visible("live-gpu-power-plot"));

    harness.driver.scroll_at(pointer, 2.0);

    let moved = harness.driver.plot_bounds("live-energy-plot");
    assert_eq!(moved.top(), plot.top() + gpui::px(2.0));
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
fn leaving_a_chart_on_a_paused_page_reads_nothing_again(cx: &mut TestAppContext) {
    // Goal: a paused page already holds its selected second, so ending a hover there does not
    // make the reader select that second again. Method: hover the previous second of the
    // seeded selection, leave the plot, and compare the page's request generation.
    let mut harness = InputHarness::setup(cx);
    let paused = harness.driver.request_generation();
    harness.driver.hover_previous_second();

    harness.driver.leave_plot("live-energy-plot", PlotExit::Top);

    assert_eq!(harness.driver.request_generation(), paused);
    harness.teardown();
}

#[gpui::test]
fn a_reply_a_hover_kept_from_the_page_is_asked_for_again_when_it_ends(cx: &mut TestAppContext) {
    // Goal: a hover keeps the page's readings still, so a reply to the page's request that
    // arrives during it is never shown and the page asks again once the hover ends. Method:
    // hover the previous second, hand the page a reply to its request, then leave the plot.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    let kept = harness.driver.deliver_page_reply();

    harness.driver.leave_plot("live-energy-plot", PlotExit::Top);

    assert!(harness.driver.request_generation() > kept);
    harness.teardown();
}

#[gpui::test]
fn a_reply_to_a_request_sent_during_a_hover_is_asked_for_again_when_it_ends(
    cx: &mut TestAppContext,
) {
    // Goal: a hover keeps the page's readings still even when the page asks for others under
    // it, so that reply is never shown and the page asks again once the hover ends. Method:
    // hover the previous second, go back to the newest second, hand the page that reply, then
    // let the page take its next tick, which ends a hover begun over the old request.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.follow_latest();
    let kept = harness.driver.deliver_page_reply();

    harness.driver.preview_tick();

    assert!(harness.driver.request_generation() > kept);
    harness.teardown();
}

#[gpui::test]
fn a_hover_after_the_page_asked_again_reads_nothing_again(cx: &mut TestAppContext) {
    // Goal: asking again settles what an earlier hover kept from the page, so the next hover
    // on the paused page ends without a read. Method: keep a reply from the page with one
    // hover, leave, then hover and leave again, comparing the generation across the second.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.deliver_page_reply();
    harness.driver.leave_plot("live-energy-plot", PlotExit::Top);
    let asked = harness.driver.request_generation();
    harness.driver.hover_previous_second();

    harness.driver.leave_plot("live-energy-plot", PlotExit::Top);

    assert_eq!(harness.driver.request_generation(), asked);
    harness.teardown();
}

#[gpui::test]
fn a_request_the_page_sends_during_a_hover_settles_the_reply_it_kept(cx: &mut TestAppContext) {
    // Goal: once the page asks again during a hover, the reply it will get replaces the one
    // the hover kept, so the hover ends without another read. Method: keep a reply from the
    // page, go back to the newest second, which asks again, then leave the plot.
    let mut harness = InputHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.deliver_page_reply();
    harness.driver.follow_latest();
    let asked = harness.driver.request_generation();

    harness.driver.leave_plot("live-energy-plot", PlotExit::Top);

    assert_eq!(harness.driver.request_generation(), asked);
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
    assert!(harness.driver.reading_is_visible("90 W"));
    assert!(harness.driver.reading_is_visible("60 °C"));

    harness
        .driver
        .leave_plot("live-gpu-power-plot", PlotExit::Bottom);

    harness.driver.assert_cursors_hidden();
    assert!(harness.driver.reading_is_visible("8 W"));
    assert!(harness.driver.reading_is_visible("40 °C"));
    harness.teardown();
}

#[gpui::test]
fn scrolling_the_plot_back_under_the_pointer_restores_synchronized_hover(cx: &mut TestAppContext) {
    // Reverse layout movement so reentry cannot depend on another pointer event.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    // The energy page is taller than a 1400-pixel window, which still shows its temperature
    // chart under the room its App chart holds for ten Apps.
    harness.driver.make_scrollable(1400.0);
    let plot = harness.driver.plot_bounds("live-energy-plot");
    let previous = harness.driver.previous_second_position();
    let pointer = gpui::point(previous.x, plot.bottom() - gpui::px(1.0));
    harness.driver.hover_position(pointer);
    harness.driver.scroll_at(pointer, -2.0);
    harness.driver.assert_cursors_hidden();
    assert!(harness.driver.reading_is_visible("8 W"));

    harness.driver.scroll_at(pointer, 2.0);

    assert_eq!(harness.driver.pointer_position(), pointer);
    assert_eq!(harness.driver.plot_bounds("live-energy-plot"), plot);
    harness.driver.assert_cursors_visible();
    assert!(harness.driver.reading_is_visible("90 W"));
    assert!(harness.driver.reading_is_visible("60 °C"));
    harness.teardown();
}

#[gpui::test]
fn clicking_a_chart_leaves_the_view_where_it_was(cx: &mut TestAppContext) {
    // Goal: a plain click on a plot neither pauses the view nor requests another second, since
    // only a drag acts on a chart. Method: click the previous second of a chart that repeats a
    // second, then read the request and whether any reading was hidden.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    let before = (
        harness.driver.requested_second(),
        harness.driver.requested_publication(),
    );

    harness.driver.hover_previous_second();
    harness.driver.click_previous_second();

    let after = (
        harness.driver.requested_second(),
        harness.driver.requested_publication(),
    );
    assert_eq!(after, before);
    assert!(!harness.driver.readings_are_hidden());
    harness.teardown();
}

#[gpui::test]
fn an_arrow_step_shows_the_adjacent_second_the_page_holds(cx: &mut TestAppContext) {
    // Goal: an arrow step on a focused chart shows the adjacent second from the readings the
    // page holds, as a hover does, and asks the reader for no selection.
    // Method: step left from the accepted second 1000, whose 8 W differs from the 90 W of
    // 999, through the rendered chart, then read the chart and the page's request.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    harness.driver.focus_chart();

    harness.driver.press("left");

    assert_eq!(harness.driver.shown_second(), Some(999));
    assert!(harness.driver.reading_is_visible("90 W"));
    assert_eq!(harness.driver.requested_second(), Some(1000));
    assert!(!harness.driver.readings_are_hidden());
    assert!(harness.driver.inspection_has_focus());
    harness.teardown();
}

#[gpui::test]
fn a_step_past_the_selected_second_shows_the_later_second_the_context_holds(
    cx: &mut TestAppContext,
) {
    // Goal: a step right from a selected second shows the next second from the readings the
    // context holds after the selection, and asks the reader for nothing.
    // Method: select 999 in a context that runs to 1000, whose 90 W differs from the 8 W of
    // 999, step right through the rendered chart, then read the chart and the page's request.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_selection_before_a_held_second();
    harness.driver.focus_chart();

    harness.driver.press("right");

    assert_eq!(harness.driver.shown_second(), Some(1000));
    assert!(harness.driver.reading_is_visible("90 W"));
    assert_eq!(harness.driver.requested_second(), Some(999));
    harness.teardown();
}

#[gpui::test]
fn a_step_past_the_held_seconds_asks_the_reader_for_that_second(cx: &mut TestAppContext) {
    // Goal: a step to a second the page's context does not hold reads that second, since the
    // page holds no readings of it to show.
    // Method: step right from the accepted second 1000, the last second its context holds, in
    // a recording that runs to 1100, then read the page's request.
    let mut harness = InputHarness::setup(cx);
    harness.driver.focus_chart();

    harness.driver.press("right");

    assert_eq!(harness.driver.requested_second(), Some(1001));
    harness.teardown();
}

#[gpui::test]
fn a_step_past_the_held_seconds_keeps_the_pages_readings_shown(cx: &mut TestAppContext) {
    // Goal: a step to a second the page holds no readings of leaves the page's readings shown
    // while the reader reads that second, so the page never goes blank between seconds.
    // Method: step right from 1000, the last second the context holds, then read the request,
    // the chart and the readings before any reply.
    let mut harness = InputHarness::setup(cx);
    harness.driver.focus_chart();

    harness.driver.press("right");

    assert_eq!(harness.driver.requested_second(), Some(1001));
    assert_eq!(harness.driver.shown_second(), Some(1000));
    assert!(!harness.driver.readings_are_hidden());
    harness.teardown();
}

#[gpui::test]
fn pausing_keeps_the_pages_readings_shown(cx: &mut TestAppContext) {
    // Goal: pausing a page that follows the recording selects the second it shows without
    // blanking the page while the reader reads that second.
    // Method: show 1000 as the newest second, pause, then read the request and the readings
    // before any reply.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_followed_selection();

    harness.driver.pause();

    assert_eq!(harness.driver.requested_second(), Some(1000));
    assert!(!harness.driver.readings_are_hidden());
    harness.teardown();
}

#[gpui::test]
fn going_to_the_latest_reading_keeps_the_pages_readings_shown(cx: &mut TestAppContext) {
    // Goal: leaving a selected second for the newest one keeps the selected second's readings
    // shown until the reader answers with the newest.
    // Method: from the accepted second 1000, go to the latest reading, then read the request,
    // the chart and the readings before any reply.
    let mut harness = InputHarness::setup(cx);

    harness.driver.pause();

    assert_eq!(harness.driver.requested_second(), None);
    assert_eq!(harness.driver.shown_second(), Some(1000));
    assert!(!harness.driver.readings_are_hidden());
    harness.teardown();
}

#[gpui::test]
fn a_step_while_a_selection_is_read_moves_from_the_second_it_asks_for(cx: &mut TestAppContext) {
    // Goal: steps past the context keep moving while the page waits on the read the last one
    // sent, as they did before a step showed held readings.
    // Method: step right twice from 1000 with no reply between the steps, then read the
    // page's request and the charts' focus.
    let mut harness = InputHarness::setup(cx);
    harness.driver.focus_chart();
    harness.driver.press("right");

    harness.driver.press("right");

    assert_eq!(harness.driver.requested_second(), Some(1002));
    assert!(harness.driver.inspection_has_focus());
    harness.teardown();
}

#[gpui::test]
fn a_step_on_a_page_that_follows_the_recording_moves_from_the_newest_second(
    cx: &mut TestAppContext,
) {
    // Goal: a page that follows the newest second, which selects no second of its own, steps
    // from the second it shows.
    // Method: show 1000 as the newest second, step left, then read the chart and the request.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_followed_selection();
    harness.driver.focus_chart();

    harness.driver.press("left");

    assert_eq!(harness.driver.shown_second(), Some(999));
    assert_eq!(harness.driver.requested_second(), None);
    harness.teardown();
}

#[gpui::test]
fn a_second_arrow_step_moves_from_the_second_the_first_showed(cx: &mut TestAppContext) {
    // Goal: arrow steps walk from the second shown, not from the accepted selection.
    // Method: step left to 999 and back right, which reaches 1000 from the shown second and
    // would reach 1001 from the accepted one.
    let mut harness = InputHarness::setup(cx);
    harness.driver.focus_chart();
    harness.driver.press("left");

    harness.driver.press("right");

    assert_eq!(harness.driver.shown_second(), Some(1000));
    harness.teardown();
}

#[gpui::test]
fn enter_selects_the_second_an_arrow_step_showed(cx: &mut TestAppContext) {
    // Goal: Enter makes the stepped second the page's selection, as Enter does on the CPU
    // history. Method: step left to 999, press Enter, then read the page's request.
    let mut harness = InputHarness::setup(cx);
    harness.driver.focus_chart();
    harness.driver.press("left");

    harness.driver.press("enter");

    assert_eq!(harness.driver.requested_second(), Some(999));
    assert_eq!(harness.driver.requested_publication(), Some(1));
    assert!(harness.driver.inspection_has_focus());
    harness.teardown();
}

#[gpui::test]
fn enter_keeps_the_stepped_seconds_readings_shown_while_it_is_read(cx: &mut TestAppContext) {
    // Goal: Enter selects the stepped second without blanking the page, which goes on showing
    // that second from the readings it holds until the reader answers.
    // Method: step left from 1000 to 999, whose 90 W differs from the 8 W of 1000, press Enter,
    // then read the request, the chart and the readings before any reply.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    harness.driver.focus_chart();
    harness.driver.press("left");

    harness.driver.press("enter");

    assert_eq!(harness.driver.requested_second(), Some(999));
    assert_eq!(harness.driver.shown_second(), Some(999));
    assert!(harness.driver.reading_is_visible("90 W"));
    assert!(!harness.driver.readings_are_hidden());
    harness.teardown();
}

#[gpui::test]
fn a_hover_ending_during_a_selections_read_shows_the_selected_second(cx: &mut TestAppContext) {
    // Goal: a hover that ends while the page reads its selected second returns the page to that
    // second from the readings it holds, and not to the second the page showed before.
    // Method: select 999, whose 90 W differs from the 8 W of 1000, hover 1000, leave the plot
    // before any reply, then read the chart.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_repeated_second();
    harness.driver.select_second(999);
    harness.driver.hover_selected_second_on("live-energy-plot");

    harness.driver.leave_plot("live-energy-plot", PlotExit::Top);

    assert_eq!(harness.driver.shown_second(), Some(999));
    assert!(harness.driver.reading_is_visible("90 W"));
    harness.teardown();
}

#[gpui::test]
fn the_reply_to_a_selection_shows_the_selected_second(cx: &mut TestAppContext) {
    // Goal: the reader's answer to Enter becomes the page's readings, so the page shows the
    // selected second from its own reading and no longer from the step.
    // Method: step left to 999, press Enter, hand the page the reply for 999, then read the
    // accepted second and the second the chart shows.
    let mut harness = InputHarness::setup(cx);
    harness.driver.focus_chart();
    harness.driver.press("left");
    harness.driver.press("enter");

    harness.driver.reply_with_second(999);

    assert_eq!(harness.driver.accepted_second(), Some(999));
    assert_eq!(harness.driver.shown_second(), Some(999));
    assert!(!harness.driver.shows_a_step());
    harness.teardown();
}

#[gpui::test]
fn a_reply_whose_context_ends_before_its_second_is_refused(cx: &mut TestAppContext) {
    // Goal: the page refuses a selection whose context ends before the selected second, since
    // it lays the charts out around that second within the context.
    // Method: select 1001, hand the page a reply for 1001 whose context ends at 1000, then read
    // the page's error.
    let mut harness = InputHarness::setup(cx);
    harness.driver.select_second(1001);

    harness.driver.reply_with_context(1001, 1000);

    assert_eq!(
        harness.driver.page_error().as_deref(),
        Some("Selection context ends before its second")
    );
    harness.teardown();
}

#[gpui::test]
fn enter_asks_for_the_stepped_second_once(cx: &mut TestAppContext) {
    // Goal: the selection Enter sends is the only request, so the reader is not made to start
    // the selected second's read again. Method: step left, press Enter, let the page take its
    // next tick, and compare the page's request generation before and after it.
    let mut harness = InputHarness::setup(cx);
    harness.driver.focus_chart();
    harness.driver.press("left");
    harness.driver.press("enter");
    let selected = harness.driver.request_generation();

    harness.driver.preview_tick();

    assert_eq!(harness.driver.request_generation(), selected);
    harness.teardown();
}

#[gpui::test]
fn moving_focus_off_the_charts_returns_an_arrow_step_to_the_selected_second(
    cx: &mut TestAppContext,
) {
    // Goal: a step lasts while the charts hold focus, as a keyboard inspection of the CPU
    // history does, so the page does not stay on a second nobody is inspecting.
    // Method: step left to 999, move focus to the next control with Tab, then read the chart.
    let mut harness = InputHarness::setup(cx);
    harness.driver.activate_window();
    harness.driver.focus_chart();
    harness.driver.press("left");

    harness.driver.dispatch_tab();

    assert!(!harness.driver.inspection_has_focus());
    assert_eq!(harness.driver.shown_second(), Some(1000));
    harness.teardown();
}

#[gpui::test]
fn escape_returns_an_arrow_step_to_the_selected_second(cx: &mut TestAppContext) {
    // Goal: Escape leaves the stepped second and shows the selection again, sending nothing.
    // Method: step left to 999, press Escape, then read the chart and compare the page's
    // request generation before and after Escape.
    let mut harness = InputHarness::setup(cx);
    harness.driver.focus_chart();
    harness.driver.press("left");
    let stepped = harness.driver.request_generation();

    harness.driver.press("escape");

    assert_eq!(harness.driver.shown_second(), Some(1000));
    assert_eq!(harness.driver.request_generation(), stepped);
    assert!(!harness.driver.readings_are_hidden());
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
    // Goal: a typed time replaces a selection still being read and keeps the page's readings
    // shown until its reply, while a copy refuses the second it has not read yet.
    // Method: select 1001, type the local time of 999 before any reply, then read the request,
    // the readings and the copy.
    let mut harness = InputHarness::setup(cx);
    harness.driver.select_second(1001);

    harness
        .driver
        .enter_time(&crate::live_presentation::clock(999, true));

    assert_eq!(harness.driver.requested_second(), Some(999));
    assert!(!harness.driver.readings_are_hidden());
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

#[gpui::test]
fn tab_with_nothing_focused_focuses_a_control(cx: &mut TestAppContext) {
    // Goal: the first Tab of a window that has focused nothing yet reaches a control, as it
    // does for a keyboard user and for the control socket, which sends the same keystroke.
    // Method: open the dashboard, send Tab through the window and ask whether it focuses
    // anything.
    let mut harness = InputHarness::setup(cx);
    assert!(!harness.driver.a_control_has_focus());

    harness.driver.dispatch_tab();

    assert!(harness.driver.a_control_has_focus());
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
        let mut harness = Self::open_route(cx, route);
        harness.driver.seed_accepted_selection();
        harness
    }

    /// Opens the window on `route` with the page's first reading still to come.
    fn open_route(cx: &mut TestAppContext, route: &str) -> Self {
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
        Self {
            driver: InputDriver { view, context },
        }
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

            assert!(dashboard.take_update(Update::Ready(snapshot)));
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
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Selects 999 in a context that holds the second after it, 1000, at 90 W.
    fn seed_selection_before_a_held_second(&mut self) {
        self.seed_selection_before_held_seconds(1000);
    }

    /// Selects 999 in a context that holds the seconds after it up to `context_last`, with the
    /// reading of 1000 at 90 W.
    fn seed_selection_before_held_seconds(&mut self, context_last: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let selected = snapshot.selection["trends"][0].clone();
            let mut later = snapshot.selection["trends"][1].clone();
            later["power_watts"] = json!(90);
            snapshot.selection["second"] = json!(999);
            snapshot.selection["context_last_second"] = json!(context_last);
            snapshot.selection["selected"]["publication"] = selected["publication"].clone();
            snapshot.selection["summary"] = selected.clone();
            snapshot.selection["trends"] = json!([selected, later]);

            dashboard.request.second = Some(999);
            dashboard.request.identity = Some(snapshot.selection["selected"].clone());
            assert!(dashboard.take_update(Update::Ready(snapshot)));
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
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Shows the accepted snapshot as the newest second of a page that follows the recording.
    fn seed_followed_selection(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = accepted_snapshot(dashboard.generation);
            dashboard.request.second = None;
            dashboard.request.identity = None;
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Pauses a page that follows the recording, or returns a paused one to the latest reading.
    fn pause(&mut self) {
        self.view.update(&mut self.context, LiveDashboard::pause);
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

    fn dispatch_tab(&mut self) {
        let tab = gpui::Keystroke::parse("tab").unwrap();
        self.context.update(|window, cx| {
            window.dispatch_keystroke(tab, cx);
        });
        self.context.run_until_parked();
    }

    fn a_control_has_focus(&mut self) -> bool {
        let root = self
            .view
            .read_with(&self.context, |dashboard, _| dashboard.root_focus.clone());

        self.context.update(|window, cx| {
            let focused = window.focused(cx);
            focused.is_some() && focused != root
        })
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

    /// Makes the window the active one, since GPUI tells a view its focus moved only in an
    /// active window.
    fn activate_window(&mut self) {
        self.context.update(|window, _| window.activate_window());
        self.context.run_until_parked();
    }

    /// The second the energy chart reads out, which is the second the page shows.
    fn shown_second(&self) -> Option<u64> {
        self.chart_accessibility_reading("live-energy").0
    }

    fn request_generation(&self) -> u64 {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.generation)
    }

    /// Goes back to following the newest second, as the range's reset does.
    fn follow_latest(&mut self) {
        self.view
            .update(&mut self.context, LiveDashboard::reset_range);
    }

    /// Hands the page a reply to its own request the way the worker's reply reaches it, and
    /// returns that request's generation.
    fn deliver_page_reply(&mut self) -> u64 {
        self.view.update(&mut self.context, |dashboard, _| {
            let generation = dashboard.generation;
            let reply = crate::live_source::Update::Ready(accepted_snapshot(generation));
            assert!(!dashboard.preview_receive(reply));
            generation
        })
    }

    /// Hands the page the reader's answer to its request for `second`, the way a tick does.
    fn reply_with_second(&mut self, second: u64) {
        self.reply_with_context(second, 1000);
    }

    /// Hands the page the reader's answer for `second` with a context that ends at
    /// `context_last`.
    fn reply_with_context(&mut self, second: u64, context_last: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let identity = snapshot.selection["trends"][0].clone();
            snapshot.selection["second"] = json!(second);
            snapshot.selection["context_last_second"] = json!(context_last);
            snapshot.selection["selected"] = json!({"run":identity["run"],
                "boot":identity["boot"],"publication":identity["publication"]});
            snapshot.selection["trends"] = json!([identity]);
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn page_error(&self) -> Option<String> {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.error.clone())
    }

    fn accepted_second(&self) -> Option<u64> {
        self.accepted_selection()?["second"].as_u64()
    }

    /// Whether the page shows a second an arrow step reached rather than its own selection.
    fn shows_a_step(&self) -> bool {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.preview.is_some())
    }

    fn preview_tick(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
        });
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

            // A page still reading keeps its charts in place, each without a reading.
            dashboard.panels.iter().all(|panel| {
                panel
                    .lines
                    .iter()
                    .all(|line| line.values.iter().all(Option::is_none))
            })
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
        selection: json!({"second":1000,"context_last_second":1000,"selected":identity,
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
        histories_pending: false,
        process_history: None,
        rankings: None,
        app_ranks: None,
        app_parts: None,
        read_ms: 0.0,
    })
}
