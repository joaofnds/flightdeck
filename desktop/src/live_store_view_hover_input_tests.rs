//! Hovering Overview drawn from the store's View, which holds seconds the worker has not read.

use std::sync::Arc;

use gpui::{Modifiers, MouseButton, TestAppContext};
use viewer::store::{AppsSecond, ByteBudget, MachineSecond, SampledSecond, View};

use super::super::LiveDashboard;
use super::{InputDriver, InputHarness, PlotExit};
use crate::chart_axes::TimeWindow;
use crate::hardware::Readings;
use crate::live_rankings::{Rankings, TopApps, from_selected};
use crate::live_source::{Update, Worker};
use crate::live_store_app_fixtures::{apps_at, hour};
use crate::live_store_fixtures::{reading_at, trend_point, view};
use crate::live_store_page_fixtures::{
    figure_readings, figure_second, readings, summary_selection, texts,
};
use crate::live_views::summary;
use crate::time_range::Range;

#[gpui::test]
fn hovering_a_second_only_the_view_holds_shows_its_readouts(cx: &mut TestAppContext) {
    // Goal: a hover over Overview drawn from the store's View shows the hovered second's
    // readouts, which the View holds and the worker has not read. Method: the worker's readings
    // end at 10,000 and the View's at 10,005, then the pointer rests on 10,003 of the CPU chart.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&held_seconds());

    harness.driver.hover_held_second(10_003);

    let expected = summary::cards("overview", &summary_selection(&reading_at(10_003, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn hovering_a_second_only_the_view_holds_shows_its_hardware_figure(cx: &mut TestAppContext) {
    // Goal: the hover shows the hardware figure of the hovered second, which the View holds and
    // the worker has not read. Method: the View's 10,003 holds the figure's readings, and the
    // pointer rests on 10,003 of the CPU chart.
    let mut seconds = held_seconds();
    seconds[2] = figure_second(10_003);
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&seconds);

    harness.driver.hover_held_second(10_003);

    assert_eq!(harness.driver.hardware_figure(), figure_readings());
    harness.teardown();
}

#[gpui::test]
fn hovering_a_second_only_the_view_holds_ranks_its_apps(cx: &mut TestAppContext) {
    // Goal: the hover's top Apps rank the Apps of the hovered second, which the View holds and
    // the worker has not read. Method: the pointer rests on 10,003 of the CPU chart, in which A
    // spent more CPU time than in any other second but the View's newest.
    let seconds = held_seconds();
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&seconds);

    harness.driver.hover_held_second(10_003);

    harness
        .driver
        .assert_top_apps(&top_apps(&seconds[2]), 10_003);
    harness.teardown();
}

#[gpui::test]
fn hovering_a_second_only_the_view_holds_reads_the_chart_there(cx: &mut TestAppContext) {
    // Goal: the CPU chart's reading, which macOS's accessibility reads, names the hovered second
    // and the chart's value there, which the View holds and the worker has not read. Method: the
    // pointer rests on 10,003, and Overview following a View that ends at 10,003 reads the chart
    // as the hover should.
    let seconds = held_seconds();
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&seconds);

    harness.driver.hover_held_second(10_003);

    let expected = reading_following("live-cpu", &seconds[..3]);
    assert_eq!(
        harness.driver.chart_accessibility_reading("live-cpu"),
        expected
    );
    harness.teardown();
}

#[gpui::test]
fn hovering_a_second_only_the_view_holds_reads_the_chart_by_app_there(cx: &mut TestAppContext) {
    // Goal: the chart of resident memory by App reads the hovered second, which the View holds
    // and the worker has not read, as it reads any second it draws. Method: the pointer rests on
    // 10,003 of the CPU chart, and Overview following a View that ends at 10,003 reads the chart
    // as the hover should.
    let seconds = held_seconds();
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&seconds);

    harness.driver.hover_held_second(10_003);

    let expected = reading_following(RESIDENT, &seconds[..3]);
    assert_eq!(
        harness.driver.chart_accessibility_reading(RESIDENT),
        expected
    );
    harness.teardown();
}

#[gpui::test]
fn hovering_a_second_the_view_holds_puts_the_cursor_on_its_point(cx: &mut TestAppContext) {
    // Goal: the cursor stands on the point the View draws for the hovered second, wherever the
    // worker's readings hold that second among theirs. Method: the View holds 9,990 before the
    // seconds both read, the range shows a minute so that a second is pixels wide, and the
    // pointer rests on 10,000, the worker's second reading and the View's third.
    let mut seconds = vec![reading_at(9_990, 0)];
    seconds.extend(held_seconds());
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&seconds);
    harness.driver.show_range(Range::Last(60));

    harness.driver.hover_held_second(10_000);

    harness.driver.assert_cpu_cursor_on_point(2);
    harness.teardown();
}

#[gpui::test]
fn hovering_a_second_the_view_holds_reads_nothing(cx: &mut TestAppContext) {
    // Goal: a hover over a second the store's View holds shows it with no read of the worker,
    // which takes tens of milliseconds where a hover has 16. Method: the pointer rests on 9,999,
    // which the worker read without its top Apps, then the hover's tick runs, which sends a
    // hover's read where one is needed.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&held_seconds());
    harness.driver.hover_held_second(9_999);

    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 0);
    harness.teardown();
}

#[gpui::test]
fn hovering_a_retained_older_sample_uses_its_machine_and_app_readings(cx: &mut TestAppContext) {
    // Goal: a thirty-minute-old retained hover uses its exact machine and ranked App payload.
    // Method: rest the worker, hover a budgeted sample missing from its readings, then tick.
    let older = reading_at(8_200, 0);
    let mut harness = InputHarness::open_route(cx, "overview");
    harness
        .driver
        .show_sampled_tail(&older, &held_seconds(), 1_048_576);
    harness.driver.show_range(Range::Last(3_600));

    harness.driver.hover_held_second(8_200);
    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 0);
    harness.driver.assert_top_apps(&top_apps(&older), 8_200);
    let expected = summary::cards("overview", &summary_selection(&older));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn a_refused_older_sample_keeps_its_reading_and_reports_unavailable_apps(cx: &mut TestAppContext) {
    // Goal: a refused retained ranking stays unavailable instead of starting an unbudgeted read.
    // Method: hover a thirty-minute-old sample with a zero-byte budget and its worker rested.
    let older = reading_at(8_200, 0);
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_sampled_tail(&older, &held_seconds(), 0);
    harness.driver.show_range(Range::Last(3_600));

    harness.driver.hover_held_second(8_200);
    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 0);
    assert!(harness.driver.reading_is_visible(
        "App totals unavailable: the complete payload exceeds its byte budget"
    ));
    let expected = summary::cards("overview", &summary_selection(&older));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn hovering_the_older_tail_reads_its_top_apps(cx: &mut TestAppContext) {
    // Goal: a hover over the older tail, whose Apps the store's View does not hold, has the
    // worker read the hovered second's top Apps. Method: the View and the worker's readings both
    // hold 9,500 in their older tail, the pointer rests on it, then the hover's tick runs.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness
        .driver
        .show_held_view_after(&[reading_at(9_500, 0)], &held_seconds());
    harness.driver.hover_held_second(9_500);

    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 1);
    harness.teardown();
}

#[gpui::test]
fn hovering_a_stretch_of_the_hour_reads_the_chart_at_the_second_standing_for_it(
    cx: &mut TestAppContext,
) {
    // Goal: at the hour range a hover over a second the View holds, older than the 16 minutes
    // the charts draw in full detail, reads the CPU chart at the second that stands for its
    // stretch, the one point the chart draws there, as it reads that second when it is the
    // newest. Method: the View holds 9,030 to 9,039, older than 9,046 where the charts' full
    // detail starts, and the pointer rests on 9,035 of the hour.
    let mut seconds: Vec<MachineSecond> =
        (9_030..9_040).map(|second| reading_at(second, 0)).collect();
    seconds.extend(held_seconds());
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&seconds);
    harness.driver.show_range(Range::Last(3_600));

    harness.driver.hover_held_second(9_035);

    let standing = [reading_at(9_020, 0), reading_at(9_030, 0)];
    assert_eq!(
        harness.driver.chart_accessibility_reading("live-cpu"),
        reading_following("live-cpu", &standing)
    );
    harness.teardown();
}

#[gpui::test]
fn hovering_the_older_tail_shows_the_reading_standing_for_its_stretch(cx: &mut TestAppContext) {
    // Goal: at the hour range a hover over a second of the older tail shows the readouts of the
    // reading that stands for its stretch, since the store keeps one second in every ten there.
    // Method: only the View's older tail holds 8,000, as when the worker rests while the page
    // follows the View, and the pointer rests on 8,005 of the hour.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness
        .driver
        .show_held_view_over_unread_tail(&[reading_at(8_000, 0)], &held_seconds());
    harness.driver.show_range(Range::Last(3_600));

    harness.driver.hover_held_second(8_005);

    let expected = summary::cards("overview", &summary_selection(&reading_at(8_000, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn hovering_the_older_part_of_the_hour_shows_each_apps_mean_over_its_stretch(
    cx: &mut TestAppContext,
) {
    // Goal: at the hour range a hover over the older part of a chart by App shows what each App
    // used over the ten seconds its point stands for, the mean the chart draws there, rather
    // than one second of it. Method: the View of the hour, in whose oldest stretch A uses 0.07 s
    // at 9,000 and 0.27 s at each of the nine seconds after it, 0.25 s a second over the
    // stretch, and the pointer on 9,005 of the CPU chart by App.
    let (view, _) = hour();
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_store_view(view);
    harness.driver.show_range(Range::Last(3_600));
    harness
        .driver
        .context
        .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));

    harness
        .driver
        .hover_held_second_on("live-cpu-apps-plot", 9_005);

    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-cpu-apps:1:A:25%")
    );
    harness.teardown();
}

#[gpui::test]
fn hovering_the_older_part_of_the_hour_on_the_cpu_page_shows_each_apps_mean_over_its_stretch(
    cx: &mut TestAppContext,
) {
    // Goal: on a page other than Overview, a hover over the older part of its chart by App shows
    // what each App used over the ten seconds its point stands for, as on Overview. Method: as
    // for Overview, on the CPU page, whose other charts are the worker's.
    let (view, _) = hour();
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_store_view(view);
    harness.driver.show_range(Range::Last(3_600));
    harness
        .driver
        .context
        .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));

    harness
        .driver
        .hover_held_second_on("live-cpu-apps-plot", 9_005);

    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-cpu-apps:1:A:25%")
    );
    harness.teardown();
}

#[gpui::test]
fn hovering_a_recent_second_on_the_cpu_page_reads_the_workers_chart_there(cx: &mut TestAppContext) {
    // Goal: on a page that draws only its charts by App from the View, a hover names the second
    // as the View holds it and the worker's charts still find their reading of it. Method: the
    // CPU page over the View of the hour, a minute's range, and the pointer on 9,999 of the
    // worker's CPU chart, which the worker and the View both read, 99.5% busy.
    let (view, _) = hour();
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_store_view(view);
    harness.driver.show_range(Range::Last(60));

    harness.driver.hover_held_second(9_999);

    assert!(harness.driver.cpu_reading().ends_with("; 99.5%"));
    harness.teardown();
}

#[gpui::test]
fn a_page_reply_during_a_hover_on_the_cpu_page_keeps_its_chart_by_app_from_the_view(
    cx: &mut TestAppContext,
) {
    // Goal: the page's own reply, which a hover takes while the page waits on its App histories,
    // leaves the chart by App drawn from the View under the pointer. Method: the CPU page waits
    // on its histories, a hover rests on the older part of the hour, then the worker's reply
    // brings the histories.
    let (view, _) = hour();
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_store_view_waiting_on_histories(view);
    harness.driver.show_range(Range::Last(3_600));
    harness
        .driver
        .hover_held_second_on("live-cpu-apps-plot", 9_005);

    harness.driver.deliver_page_histories();

    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-cpu-apps:1:A:25%")
    );
    harness.teardown();
}

#[gpui::test]
fn hovering_the_older_part_of_the_hour_shows_each_apps_mean_resident_memory_over_its_stretch(
    cx: &mut TestAppContext,
) {
    // Goal: at the hour range a hover over the older part of the chart of resident memory by
    // App shows the mean each App held over the ten seconds its point stands for. Method: the
    // View of the hour, in whose oldest stretch A holds 70 MB at 9,000 and 270 MB at each of
    // the nine seconds after it, 250 MB or 238 MiB over the stretch, and the pointer on 9,005.
    let (view, _) = hour();
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_store_view(view);
    harness.driver.show_range(Range::Last(3_600));
    harness
        .driver
        .context
        .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));

    harness
        .driver
        .hover_held_second_on("live-memory-app-resident-plot", 9_005);

    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-memory-app-resident:1:A:238 MiB")
    );
    harness.teardown();
}

#[gpui::test]
fn hovering_the_older_part_of_the_hour_on_the_memory_page_shows_each_apps_mean_resident_memory(
    cx: &mut TestAppContext,
) {
    // Goal: the Memory page's chart of resident memory by App shows, at a hover over its older
    // part, the mean each App held over the ten seconds its point stands for, as Overview's does.
    // Method: as for Overview, on the Memory page.
    let (view, _) = hour();
    let mut harness = InputHarness::open_route(cx, "memory");
    harness.driver.show_store_view(view);
    harness.driver.show_range(Range::Last(3_600));
    harness
        .driver
        .context
        .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));

    harness
        .driver
        .hover_held_second_on("live-memory-app-resident-plot", 9_005);

    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-memory-app-resident:1:A:238 MiB")
    );
    harness.teardown();
}

#[gpui::test]
fn hovering_the_older_part_of_the_hour_shows_the_readouts_of_the_second_standing_for_it(
    cx: &mut TestAppContext,
) {
    // Goal: a hover over the older part of the hour, which names the tick of the stretch the
    // charts by App draw there, still shows the readouts of the second the older tail keeps for
    // that stretch. Method: the View of the hour, whose older tail keeps 9,000 for stretch 900,
    // and the pointer on 9,005 of the CPU chart.
    let (view, _) = hour();
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_store_view(view);
    harness.driver.show_range(Range::Last(3_600));

    harness.driver.hover_held_second(9_005);

    let expected = summary::cards("overview", &summary_selection(&reading_at(9_000, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn a_newer_view_leaves_the_hovered_charts_where_they_were(cx: &mut TestAppContext) {
    // Goal: a View the store hands over while the pointer rests on a chart drawn from the View
    // leaves the range where it was, so the hovered second stays under the pointer, as the
    // worker's readings hold the page still under a hover. Method: the pointer rests on 10,003
    // of the last minute, then the store hands over a View that ends at 10,008.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&held_seconds());
    harness.driver.show_range(Range::Last(60));
    harness.driver.hover_held_second(10_003);

    harness.driver.take_newer_view();

    assert_eq!(harness.driver.view_window(), Some(minute_ending_at(10_005)));
    harness.teardown();
}

#[gpui::test]
fn a_hover_that_ends_shows_the_newer_view(cx: &mut TestAppContext) {
    // Goal: once the hover ends, the charts draw the View the store handed over under it, so the
    // page follows the newest second again. Method: the store hands over a View that ends at
    // 10,008 while the pointer rests on 10,003, then the pointer leaves the chart, and Overview
    // following that View reads the CPU chart as the page should.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&held_seconds());
    harness.driver.show_range(Range::Last(60));
    harness.driver.hover_held_second(10_003);
    harness.driver.take_newer_view();

    harness.driver.leave_cpu_chart();

    let expected = reading_following("live-cpu", &newer_seconds());
    assert_eq!(
        harness.driver.chart_accessibility_reading("live-cpu"),
        expected
    );
    harness.teardown();
}

#[gpui::test]
fn an_arrow_step_steps_from_the_newest_second_the_view_holds(cx: &mut TestAppContext) {
    // Goal: an arrow step on Overview drawn from the store's View steps from the newest second
    // the page shows, which the worker has not read, since a rested reader reads no more
    // seconds. Method: the worker's readings end at 10,000 and the View's at 10,005, then a left
    // step on the focused CPU chart.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&stepped_seconds());
    harness.driver.focus_cpu_chart();

    harness.driver.press("left");

    assert_eq!(harness.driver.cpu_shown_second(), Some(10_004));
    harness.teardown();
}

#[gpui::test]
fn an_arrow_step_to_a_second_only_the_view_holds_reads_the_chart_by_app_there(
    cx: &mut TestAppContext,
) {
    // Goal: an arrow step to a second only the store's View holds names the tick the View holds
    // of it, so the chart of resident memory by App reads that second, as it reads a hover's.
    // Method: a left step from the View's newest second, 10,005, to 10,004, and Overview
    // following a View that ends at 10,004 reads the chart as the step should.
    let seconds = stepped_seconds();
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&seconds);
    harness.driver.focus_cpu_chart();

    harness.driver.press("left");

    let expected = reading_following(RESIDENT, &seconds[..3]);
    assert_eq!(
        harness.driver.chart_accessibility_reading(RESIDENT),
        expected
    );
    harness.teardown();
}

#[gpui::test]
fn a_drag_on_overview_drawn_from_the_view_holds_its_newest_second(cx: &mut TestAppContext) {
    // Goal: a drag across a chart of Overview drawn from the store's View holds the page at the
    // newest second it shows, which the worker has not read, so the stretch stays where it was
    // drawn. Method: the worker's readings end at 10,000 and the View's at 10,005, a drag across
    // the middle half of the CPU chart, then the second the page asks the reader for.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_held_view(&held_seconds());

    harness.driver.drag_across("live-cpu-plot", 0.25, 0.75);

    assert_eq!(harness.driver.requested_second(), Some(10_005));
    harness.teardown();
}

/// The last minute of the clock, ending at `last`.
fn minute_ending_at(last: u64) -> TimeWindow {
    TimeWindow {
        first: last - 59,
        span: 60,
    }
}

/// The chart of resident memory by App.
const RESIDENT: &str = crate::live_resident_history::PANEL_ID;

/// The seconds 9,999, 10,000, 10,003 and 10,005, which the View holds and of which the worker
/// has read the first two.
fn held_seconds() -> [MachineSecond; 4] {
    [
        reading_at(9_999, 0),
        reading_at(10_000, 0),
        reading_at(10_003, 0),
        reading_at(10_005, 0),
    ]
}

/// The seconds 9,999, 10,000, 10,004 and 10,005, which the View holds and of which the worker
/// has read the first two, so a step from the newest lands on a second only the View holds.
fn stepped_seconds() -> [MachineSecond; 4] {
    [
        reading_at(9_999, 0),
        reading_at(10_000, 0),
        reading_at(10_004, 0),
        reading_at(10_005, 0),
    ]
}

/// The `held_seconds` and 10,008, which the store's newer View holds.
fn newer_seconds() -> Vec<MachineSecond> {
    let mut seconds = held_seconds().to_vec();
    seconds.push(reading_at(10_008, 0));
    seconds
}

/// The View `version` of `seconds` after the older tail `older`, each of `seconds` with its
/// `held_apps`, which the chart of resident memory by App draws.
fn held_view(version: u64, older: &[MachineSecond], seconds: &[MachineSecond]) -> Arc<View> {
    let mut held = view(version, older, seconds);
    held.apps_loaded = true;
    held.apps = seconds
        .iter()
        .map(|second| Arc::new(held_apps(second)))
        .collect();
    Arc::new(held)
}

/// The Apps of `second`: one executable, A, that spent a thousand nanoseconds of CPU time for
/// each publication of the collector.
fn held_apps(second: &MachineSecond) -> AppsSecond {
    apps_at(second, &[("A", second.publication * 1_000)])
}

/// What the chart `panel_id` reads, as macOS's accessibility reads it, on Overview following
/// the newest of `seconds`, drawn from their `held_view` over the worker's readings of the first
/// two.
fn reading_following(panel_id: &str, seconds: &[MachineSecond]) -> (Option<u64>, String) {
    let root = std::env::temp_dir().join(format!(
        "flightdeck-store-view-hover-{}",
        std::process::id()
    ));
    let worker = Worker::start(root).unwrap();
    let mut dashboard = LiveDashboard::new(worker, "overview".into(), None);
    dashboard.accept(readings(&seconds[..2]));
    assert!(dashboard.take_view(held_view(1, &[], seconds)));

    let chart = dashboard.panels.iter().find(|panel| panel.id == panel_id);
    dashboard.chart_accessibility_reading(chart.unwrap())
}

/// The top Apps of `second` and its `held_apps`, as the worker ranks a second's Apps.
fn top_apps(second: &MachineSecond) -> TopApps {
    let ranks = held_apps(second).ranks();
    let Rankings::Ready(top) = from_selected(ranks.map_err(|missing| missing.to_string())) else {
        panic!("the Apps of {} rank", second.publication);
    };
    *top
}

impl InputDriver {
    /// Shows a budgeted older sample and rests the legacy reader before inspecting it.
    fn show_sampled_tail(
        &mut self,
        older: &MachineSecond,
        seconds: &[MachineSecond],
        bytes: usize,
    ) {
        self.show_held_view_over_unread_tail(std::slice::from_ref(older), seconds);
        let budget = Arc::new(ByteBudget::new(bytes));
        let sample = SampledSecond::from_second(older, &held_apps(older), &budget);
        let mut held = held_view(2, std::slice::from_ref(older), seconds);
        Arc::get_mut(&mut held)
            .unwrap()
            .sampled
            .push(Arc::new(sample));
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.take_view(held));
            if let Some(resting) = dashboard.worker.hand_over().unwrap() {
                dashboard.worker = resting;
            }
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Shows Overview drawn from the `held_view` of `seconds` over the worker's readings of
    /// 9,999 and 10,000, following the newest second.
    fn show_held_view(&mut self, seconds: &[MachineSecond]) {
        self.show_held_view_after(&[], seconds);
    }

    /// Shows Overview as `show_held_view` does, with `older` the older tail of both the View and
    /// the worker's readings.
    fn show_held_view_after(&mut self, older: &[MachineSecond], seconds: &[MachineSecond]) {
        self.show_held_view_reading_older(older, older, seconds);
    }

    /// Shows Overview as `show_held_view` does, with `older` the older tail of the View, of which
    /// the worker's readings hold nothing.
    fn show_held_view_over_unread_tail(
        &mut self,
        older: &[MachineSecond],
        seconds: &[MachineSecond],
    ) {
        self.show_held_view_reading_older(&[], older, seconds);
    }

    /// Shows Overview as `show_held_view` does, with `older` the older tail of the View and
    /// `read_older` that of the worker's readings.
    fn show_held_view_reading_older(
        &mut self,
        read_older: &[MachineSecond],
        older: &[MachineSecond],
        seconds: &[MachineSecond],
    ) {
        let read = [reading_at(9_999, 0), reading_at(10_000, 0)];
        let mut snapshot = readings(&read);
        snapshot.selection.legacy_mut().unwrap()["earlier_trends"] =
            read_older.iter().map(trend_point).collect();
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            assert!(dashboard.take_view(held_view(1, older, seconds)));
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));
        self.context.run_until_parked();
    }

    /// Shows the page drawn from `view` over the worker's readings of 9,999 and 10,000, which
    /// wait on their App histories.
    fn show_store_view_waiting_on_histories(&mut self, view: View) {
        let mut snapshot = readings(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        snapshot.histories_pending = true;
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            assert!(dashboard.take_view(Arc::new(view)));
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));
        self.context.run_until_parked();
    }

    /// Delivers the worker's reply to the page's request, which brings its App histories.
    fn deliver_page_histories(&mut self) {
        let mut snapshot = readings(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        self.view.update(&mut self.context, |dashboard, cx| {
            snapshot.generation = dashboard.generation;
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Hands the page the store's View of `newer_seconds`, as the store does each tick.
    fn take_newer_view(&mut self) {
        let seconds = newer_seconds();
        self.view.update(&mut self.context, |dashboard, cx| {
            if dashboard.take_view(held_view(2, &[], &seconds)) {
                cx.notify();
            }
        });
        self.context.run_until_parked();
    }

    /// Focuses the CPU chart for arrow steps, with a right press, which selects no second.
    fn focus_cpu_chart(&mut self) {
        let bounds = self.context.debug_bounds("live-cpu-plot").unwrap();
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
    }

    /// The second the CPU chart shows, as macOS's accessibility reads it.
    fn cpu_shown_second(&self) -> Option<u64> {
        self.chart_accessibility_reading("live-cpu").0
    }

    /// What the CPU chart reads at the second it shows.
    fn cpu_reading(&self) -> String {
        self.chart_accessibility_reading("live-cpu").1
    }

    /// Moves the pointer off the CPU chart, which ends its hover.
    fn leave_cpu_chart(&mut self) {
        self.leave_plot("live-cpu-plot", PlotExit::Top);
        self.context.run_until_parked();
    }

    /// Asserts that the CPU chart paints its cursor on its point `index`.
    fn assert_cpu_cursor_on_point(&mut self, index: usize) {
        let position = self.view.read_with(&self.context, |dashboard, _| {
            let cpu = dashboard.panels.iter().find(|panel| panel.id == "live-cpu");
            cpu.unwrap().positions.as_ref().unwrap()[index]
        });

        self.assert_cursor_tracks_fraction("live-cpu-plot", position);
    }

    /// Rests the pointer on `second` of the CPU chart, where the range shows it.
    fn hover_held_second(&mut self, second: u64) {
        self.hover_held_second_on("live-cpu-plot", second);
    }

    /// Rests the pointer on `second` of the chart whose plot is `plot`, where the range shows it.
    fn hover_held_second_on(&mut self, plot: &'static str, second: u64) {
        let window = self
            .view
            .read_with(&self.context, |dashboard, _| dashboard.view_window());
        let fraction = window.unwrap().fraction_of(second).unwrap();
        let position = self.second_position(plot, fraction);

        self.hover_position(position);
        self.context.run_until_parked();
    }

    /// The readings the hardware figure shows, which nothing kept from it.
    fn hardware_figure(&self) -> Readings {
        self.view.read_with(&self.context, |dashboard, _| {
            let (readings, failures) = dashboard.live_hardware_readings();
            assert_eq!(failures, [None, None]);
            readings
        })
    }

    /// Asserts that the top Apps the page shows are `expected`, of the second `second`.
    fn assert_top_apps(&self, expected: &TopApps, second: u64) {
        self.view.read_with(&self.context, |dashboard, _| {
            let (top, target) = dashboard.ranking_reading().unwrap();
            assert_eq!((top, target.second), (expected, second));
        });
    }

    /// The label, reading, note and colour of each of the readouts the page shows.
    fn readout_texts(&self) -> Vec<(String, String, String, u32)> {
        self.view.read_with(&self.context, |dashboard, _| {
            texts(&dashboard.summary_cards())
        })
    }
}
