//! Holding a second of the live Mac with a click, which keeps every chart as it was drawn at
//! the click and shows the held second's readings until the user lets go.

use std::sync::Arc;

use gpui::{Modifiers, MouseButton, TestAppContext};

use super::range::cpu_by_app_lines_drawing;
use super::store_view_hover::held_seconds;
use super::{InputDriver, InputHarness, PlotExit};
use crate::chart_axes::TimeWindow;
use crate::live_hover::Preview;
use crate::live_source::Update;
use crate::live_store_app_fixtures::{HOUR_NEWEST, hour, hour_led_from_its_older_part};
use crate::live_store_fixtures::{reading_at, trend_point};
use crate::live_store_page_fixtures::{readings, summary_selection, texts};
use crate::live_views::summary;
use crate::time_range::Range;

/// The second the tests hold, 800 seconds before the newest, inside the newest fifteen minutes
/// but outside the newest seven and a half, so a window centred on it would move.
const HELD: u64 = HOUR_NEWEST - 800;
/// A second of the older tail of the View `show_held_view_after` shows, which holds no Apps
/// there.
const TAIL: u64 = 9_500;
/// The window of the newest fifteen minutes the page draws before the click.
const DRAWN: TimeWindow = TimeWindow {
    first: HOUR_NEWEST - 899,
    span: 900,
};

#[gpui::test]
fn a_click_on_the_cpu_page_holds_the_second_without_reading_the_page_again(
    cx: &mut TestAppContext,
) {
    // Goal: a click on a chart of the live Mac holds the second under the pointer in the window
    // the charts were drawn in, and asks the reader for nothing, since the hover already read
    // that second. Method: the CPU page over the View of the hour at the newest fifteen minutes,
    // a hover 800 seconds before the newest on its chart by App that reads its second, a click
    // there, then the page's window and the commands the reader was sent.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.tick_hover();
    let commands = harness.driver.reader_commands();

    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    assert_eq!(harness.driver.requested_second(), Some(HELD));
    assert_eq!(harness.driver.reader_commands(), commands);
    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    harness.teardown();
}

#[gpui::test]
fn leaving_the_chart_after_a_click_shows_the_held_second(cx: &mut TestAppContext) {
    // Goal: the page shows the held second once the pointer leaves the chart, with the value
    // the chart read under the pointer, rather than the newest second it showed before the
    // click. Method: click 800 seconds before the newest on the CPU page's chart by App, whose
    // value there differs from its newest, move the pointer off it, then the second the CPU
    // chart reads and what the chart by App reads.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    let newest = harness.driver.cpu_apps_value();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    let hovered = harness.driver.cpu_apps_value();
    assert_ne!(hovered, newest);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Top);
    harness.driver.context.run_until_parked();

    assert_eq!(harness.driver.cpu_shown_second(), Some(HELD));
    assert_eq!(harness.driver.cpu_apps_value(), hovered);
    harness.teardown();
}

#[gpui::test]
fn the_end_of_a_hover_while_a_second_is_held_shows_the_held_second_without_reading(
    cx: &mut TestAppContext,
) {
    // Goal: a hover of another second while the page holds one shows the held second again
    // when it ends, and asks the reader for nothing to show it. Method: click 800 seconds before
    // the newest on the CPU page's chart by App and answer the read of it, hover 700 seconds
    // before the newest and let it read its second, leave the chart, then the second the CPU
    // chart reads and the commands the reader was sent.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness.driver.answer_held_read();
    harness
        .driver
        .hover_second_on("live-cpu-apps-plot", HELD + 100);
    harness.driver.tick_hover();
    assert_eq!(harness.driver.hover_reads(), 1);
    let commands = harness.driver.reader_commands();

    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Top);
    harness.driver.context.run_until_parked();

    assert_eq!(harness.driver.cpu_shown_second(), Some(HELD));
    assert_eq!(harness.driver.reader_commands(), commands);
    harness.teardown();
}

#[gpui::test]
fn a_reply_to_the_request_out_at_the_click_leaves_the_held_page_as_it_was(cx: &mut TestAppContext) {
    // Goal: the page takes no reply to a request it sent before the click, as the reader
    // publishes the live Mac's newer seconds under it, whose readings would move the window
    // the click held. Method: click 800 seconds before the newest on the CPU page's chart by
    // App, hand the page a reply to the request it followed the live Mac with, reading ten
    // seconds past the newest, then the page's window.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    let generation = harness.driver.request_generation();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    harness
        .driver
        .reply_to_page_reading(generation, HOUR_NEWEST + 10);

    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    harness.teardown();
}

#[gpui::test]
fn a_page_opened_while_a_second_is_held_reads_the_context_the_charts_were_drawn_in(
    cx: &mut TestAppContext,
) {
    // Goal: a page opened while a second is held asks the reader for the context that ends
    // where the charts were drawn at the click, since a context that ends later, as the live
    // Mac's newer seconds arrive, would leave the held second out of the charts it draws.
    // Method: click 800 seconds before the newest on the CPU page's chart by App, open Memory,
    // then the second the reader was asked for, whose context runs 450 seconds past it.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    harness.driver.navigate("memory");

    assert_eq!(
        harness.driver.reader_asked_second(),
        Some(HOUR_NEWEST - viewer::CONTEXT_AFTER_SECONDS)
    );
    harness.teardown();
}

#[gpui::test]
fn leaving_overviews_chart_after_a_click_shows_the_held_second_on_the_readouts(
    cx: &mut TestAppContext,
) {
    // Goal: Overview's readouts, which the store's View draws, show the held second once the
    // pointer leaves the chart, rather than the View's newest second. Method: click 800 seconds
    // before the newest on Overview's CPU chart by App, move the pointer off it, then the text of
    // each summary readout against those of the held second's readings, the second the top Apps
    // read, and the second the CPU chart reads.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Top);
    harness.driver.context.run_until_parked();

    let expected = summary::cards("overview", &summary_selection(&reading_at(HELD, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    assert_eq!(harness.driver.ranked_second(), HELD);
    assert_eq!(harness.driver.cpu_shown_second(), Some(HELD));
    harness.teardown();
}

#[gpui::test]
fn a_click_on_overview_over_a_rested_reader_holds_the_second_without_reading(
    cx: &mut TestAppContext,
) {
    // Goal: a click on Overview, whose charts and readings the store's View draws while the
    // reader rests, holds the second and sends the reader nothing, since the View holds the
    // held second's readings and top Apps and the rest already stopped the reader following the
    // live Mac. Method: Overview over the View of the hour at the newest fifteen minutes, the
    // reader rested, a hover and a click 800 seconds before the newest on the CPU chart by App,
    // then the commands the reader was sent and the second the page's readings show.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.rest_reader();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    let commands = harness.driver.reader_commands();

    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    assert_eq!(harness.driver.reader_commands(), commands);
    assert_eq!(harness.driver.shown_reading_second(), Some(HELD));
    harness.teardown();
}

#[gpui::test]
fn the_reader_keeps_reading_a_second_held_on_overview_until_it_answers(cx: &mut TestAppContext) {
    // Goal: the page leaves the reader reading a held second whose top Apps the store's View
    // lacks until the read answers, rather than resting it as it does while it draws Overview
    // from the View, which would drop the read. Method: Overview over a View whose older tail
    // holds 9,500 without its Apps, a hover there that reads its second, a click there, the
    // page's step that rests the reader before the read answers, then whether the reader rests.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_view_over_tail();
    harness.driver.hover_second_on("live-cpu-plot", TAIL);
    harness.driver.tick_hover();
    harness.driver.click_second_on("live-cpu-plot", TAIL);

    let rested = harness.driver.offer_rest();

    assert!(!rested);
    harness.teardown();
}

#[gpui::test]
fn a_click_over_a_rested_reader_on_a_second_the_view_lacks_reads_the_page_first(
    cx: &mut TestAppContext,
) {
    // Goal: a click on Overview over a rested reader, on a second whose top Apps the store's
    // View lacks, asks the reader for the page before the held second, since a rested reader
    // reads a second's own read as a newly opened page. Method: Overview over a View whose older
    // tail holds 9,500 without its Apps, the reader rested, a hover and a click there, then the
    // second the reader was last asked for, which the page's read names 450 seconds before the
    // held window's last second.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_view_over_tail();
    harness.driver.rest_reader();
    harness.driver.hover_second_on("live-cpu-plot", TAIL);
    harness.driver.tick_hover();

    harness.driver.click_second_on("live-cpu-plot", TAIL);

    assert_eq!(
        harness.driver.reader_asked_second(),
        Some(harness.driver.held_last() - viewer::CONTEXT_AFTER_SECONDS)
    );
    harness.teardown();
}

#[gpui::test]
fn a_second_held_over_a_rested_reader_is_read_once_the_page_answers(cx: &mut TestAppContext) {
    // Goal: the held second's own read follows the page's reply that a click over a rested
    // reader asked for first, so the held second's top Apps arrive. Method: Overview over a View
    // whose older tail holds 9,500 without its Apps, the reader rested, a hover and a click
    // there, the reply to the page's read, the hold's step, then the second the reader was last
    // asked for.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_view_over_tail();
    harness.driver.rest_reader();
    harness.driver.hover_second_on("live-cpu-plot", TAIL);
    harness.driver.tick_hover();
    harness.driver.click_second_on("live-cpu-plot", TAIL);
    harness.driver.answer_page_read_over_tail();

    harness.driver.tick_hold();

    assert_eq!(harness.driver.reader_asked_second(), Some(TAIL));
    harness.teardown();
}

#[gpui::test]
fn a_rank_link_while_a_second_is_held_holds_the_second_it_names(cx: &mut TestAppContext) {
    // Goal: a rank link of a second the page shows over a held one opens the App's page holding
    // the second the link names, in the window the charts were drawn in, rather than marking
    // that second over the held second's readings. Method: click 800 seconds before the newest
    // on the CPU page's chart by App, step a second left on that chart, whose top Apps the
    // store's View holds, click the App the CPU ranking leads with, then the second the page's
    // readings show and the page's window.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Top);
    harness.driver.step_left_on_cpu_apps();

    harness.driver.click_ranked_app("ranking:CPU:A:59.0%");

    assert_eq!(harness.driver.shown_reading_second(), Some(HELD - 1));
    assert_eq!(harness.driver.requested_second(), Some(HELD - 1));
    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    harness.teardown();
}

#[gpui::test]
fn an_executable_pick_while_a_second_is_held_holds_the_second_shown_and_keeps_the_charts(
    cx: &mut TestAppContext,
) {
    // Goal: an executable picked while the page shows a second over a held one holds the shown
    // second in the held one's place and keeps the page's readings and charts, rather than
    // clearing the page to read it again. Method: click 800 seconds before the newest on
    // Overview's CPU chart by App, step a second left on that chart and note its value there,
    // pick an executable as the executables table does, then the second the page's readings
    // show, the value the chart reads and the page's window.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Top);
    harness.driver.step_left_on_cpu_apps();
    let stepped = harness.driver.cpu_apps_value();

    harness.driver.pick_executable("hex:61");

    assert_eq!(harness.driver.shown_reading_second(), Some(HELD - 1));
    assert_eq!(harness.driver.cpu_apps_value(), stepped);
    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    harness.teardown();
}

#[gpui::test]
fn pause_on_overview_at_the_hour_keeps_the_apps_that_led_over_it(cx: &mut TestAppContext) {
    // Goal: Pause on the live Mac's Overview at the last 60 minutes holds the newest second over
    // the View its charts by App were drawn from, so they keep drawing the Apps that led over the
    // hour, which the worker's fifteen minutes of readings cannot. Method: the View of an hour in
    // whose two oldest stretches D led, the last 60 minutes, Pause, a tick of the page, then the
    // lines the CPU chart by App draws.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_store_view(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));

    harness.driver.pause();
    harness.driver.tick_page();

    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["D", "A", "B", "C"]))
    );
    harness.teardown();
}

#[gpui::test]
fn a_drag_on_the_cpu_page_at_the_hour_reads_once_and_keeps_the_chart_by_app(
    cx: &mut TestAppContext,
) {
    // Goal: a stretch dragged out on a chart of the live Mac holds the newest second, so the
    // reader gets the one read that stops it following the live Mac, and neither the wait for
    // that read nor its answer takes the chart by App off the View it was drawn from. Method: the
    // CPU page over the View of the hour at the last 60 minutes, a drag across the middle of its
    // chart by App, the pointer moved off it, the page's step that drops a View it no longer
    // follows, the reader's answer to its newest command, then the commands the reader was sent
    // and the lines the chart by App draws.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.show_range(Range::Last(3_600));
    let commands = harness.driver.reader_commands();

    harness.driver.drag_across("live-cpu-apps-plot", 0.25, 0.75);
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Top);
    harness.driver.tick_presence();
    harness.driver.answer_newest_command();

    assert_eq!(harness.driver.reader_commands(), commands + 1);
    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["A", "B", "C"]))
    );
    harness.teardown();
}

impl InputDriver {
    /// Shows the page over the View of the hour and the worker's reply to its request, the
    /// readings of the hour's newest two seconds, following the newest second at the newest
    /// fifteen minutes.
    fn show_hour_at_fifteen_minutes(&mut self) {
        let (view, _) = hour();
        self.show_range(Range::History);
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut reply = readings(&[reading_at(HOUR_NEWEST - 1, 0), reading_at(HOUR_NEWEST, 0)]);
            reply.generation = dashboard.generation;
            assert!(dashboard.take_update(Update::Ready(reply)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            assert!(dashboard.take_view(Arc::new(view)));
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));
        self.context.run_until_parked();
    }

    /// Shows Overview over a View whose older tail holds `TAIL` without its Apps, and the worker's
    /// reply to the page's request, which reads `TAIL` in its older tail too.
    fn show_view_over_tail(&mut self) {
        self.show_held_view_after(&[reading_at(TAIL, 0)], &held_seconds());
        self.answer_page_read_over_tail();
    }

    /// Hands the page the worker's reply to its request, the readings of the hour's newest two
    /// seconds with `TAIL` in their older tail.
    fn answer_page_read_over_tail(&mut self) {
        let mut reply = readings(&[reading_at(HOUR_NEWEST - 1, 0), reading_at(HOUR_NEWEST, 0)]);
        reply.selection.legacy_mut().unwrap()["earlier_trends"] =
            vec![trend_point(&reading_at(TAIL, 0))].into();
        self.view.update(&mut self.context, |dashboard, cx| {
            reply.generation = dashboard.generation;
            assert!(dashboard.take_update(Update::Ready(reply)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Runs one of the page's quarter-second ticks.
    fn tick_page(&mut self) {
        self.view.update(
            &mut self.context,
            crate::live_dashboard::LiveDashboard::tick,
        );
        self.context.run_until_parked();
    }

    /// Runs the page's step that keeps the View only while the page follows the live Mac or
    /// holds a second.
    fn tick_presence(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.tick_presence(cx);
        });
        self.context.run_until_parked();
    }

    /// Hands the page the reader's answer to its newest command, the readings of the hour's
    /// newest two seconds.
    fn answer_newest_command(&mut self) {
        let generation = self.reader_commands();
        self.reply_to_page_reading(generation, HOUR_NEWEST);
    }

    /// Runs the hold's step, which reads the held second where its reading lacks what the page
    /// shows.
    fn tick_hold(&mut self) {
        self.view
            .update(&mut self.context, |dashboard, _| dashboard.hold_tick());
    }

    /// Runs the page's step that rests the reader while it draws Overview from the store's
    /// View, and answers whether the reader rests.
    fn offer_rest(&mut self) -> bool {
        self.view.update(&mut self.context, |dashboard, _| {
            dashboard.tick_rest();
            dashboard.reader_rested()
        })
    }

    /// Rests the reader, as the page does while it draws Overview from the store's View.
    fn rest_reader(&mut self) {
        assert!(self.offer_rest());
    }

    /// Rests the pointer on `second` of the chart whose plot is `plot`.
    fn hover_second_on(&mut self, plot: &'static str, second: u64) {
        let position = self.held_second_position(plot, second);
        self.hover_position(position);
        self.context.run_until_parked();
    }

    /// Clicks `second` of the chart whose plot is `plot`, where the pointer rests, as a person
    /// holds a second.
    fn click_second_on(&mut self, plot: &'static str, second: u64) {
        let position = self.held_second_position(plot, second);
        self.context.simulate_click(position, Modifiers::none());
        self.context.run_until_parked();
    }

    /// Where `second` lies on the chart whose plot is `plot`, in the window the page shows.
    fn held_second_position(
        &mut self,
        plot: &'static str,
        second: u64,
    ) -> gpui::Point<gpui::Pixels> {
        let window = self.view_window().unwrap();

        self.second_position(plot, window.fraction_of(second).unwrap())
    }

    /// Hands the page the reader's answer to its read of the held second.
    fn answer_held_read(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let hold = dashboard.hold.as_ref().unwrap();
            let snapshot = dashboard.snapshot.as_deref().unwrap();
            let target = hold.reading.target.clone();
            let mut reply = Preview::resident(snapshot, &dashboard.request, target).reading;
            reply.generation = hold.reading.generation.unwrap();
            assert!(dashboard.take_update(Update::Ready(reply)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Focuses the CPU page's chart by App with a right press, which selects no second, and
    /// steps it a second left.
    fn step_left_on_cpu_apps(&mut self) {
        let bounds = self.context.debug_bounds("live-cpu-apps-plot").unwrap();
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
        self.press("left");
        self.context.run_until_parked();
    }

    /// Picks the executable `key` as a click on its row of the executables table does.
    fn pick_executable(&mut self, key: &str) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.select_executable(Some(key.into()));
            dashboard.rebuild();
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// The second the page's readings show.
    fn shown_reading_second(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.reading_target().map(|target| target.second)
        })
    }

    /// The value the CPU page's chart by App reads at the second it shows, without the clock
    /// and the Unix second its reading starts with.
    fn cpu_apps_value(&self) -> String {
        let (_, reading) = self.chart_accessibility_reading("live-cpu-apps");

        reading.splitn(3, "; ").nth(2).unwrap().to_owned()
    }

    /// The second the page's top Apps read.
    fn ranked_second(&self) -> u64 {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.ranking_reading().unwrap().1.second
        })
    }

    /// How many commands the window has sent the reader.
    fn reader_commands(&self) -> u64 {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.worker.commands_sent()
        })
    }

    /// The last second of the window the held second was held in.
    fn held_last(&self) -> u64 {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.hold.as_ref().unwrap().last
        })
    }

    /// The second the window's last command asked the reader for.
    fn reader_asked_second(&self) -> Option<u64> {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.worker.sent_second())
    }

    /// Hands the page the reply to its request of `generation`, the live readings up to
    /// `newest`.
    fn reply_to_page_reading(&mut self, generation: u64, newest: u64) {
        let mut reply = readings(&[reading_at(newest - 1, 0), reading_at(newest, 0)]);
        reply.generation = generation;
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.take_update(Update::Ready(reply));
            cx.notify();
        });
        self.context.run_until_parked();
    }
}
