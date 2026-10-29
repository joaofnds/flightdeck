//! Holding a second of the live Mac with a click, which keeps every chart as it was drawn at
//! the click and shows the held second's readings until the user lets go.

use std::sync::Arc;

use gpui::{Modifiers, TestAppContext};

use super::{InputDriver, InputHarness, PlotExit};
use crate::chart_axes::TimeWindow;
use crate::live_hover::Preview;
use crate::live_source::Update;
use crate::live_store_app_fixtures::{HOUR_NEWEST, hour};
use crate::live_store_fixtures::reading_at;
use crate::live_store_page_fixtures::readings;
use crate::time_range::Range;

/// The second the tests hold, 800 seconds before the newest, inside the newest fifteen minutes
/// but outside the newest seven and a half, so a window centred on it would move.
const HELD: u64 = HOUR_NEWEST - 800;
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

    /// The value the CPU page's chart by App reads at the second it shows, without the clock
    /// and the Unix second its reading starts with.
    fn cpu_apps_value(&self) -> String {
        let (_, reading) = self.chart_accessibility_reading("live-cpu-apps");

        reading.splitn(3, "; ").nth(2).unwrap().to_owned()
    }

    /// How many commands the window has sent the reader.
    fn reader_commands(&self) -> u64 {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.worker.commands_sent()
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
