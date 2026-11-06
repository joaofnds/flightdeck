//! Holding a second of the live Mac with a click, which keeps every chart as it was drawn at
//! the click and shows the held second's readings until the user lets go.

use std::sync::Arc;

use collector::recording::identity_text;
use gpui::{Modifiers, MouseButton, TestAppContext};

use super::range::cpu_by_app_lines_drawing;
use super::store_view_hover::held_seconds;
use super::{InputDriver, InputHarness, PlotExit};
use crate::chart_axes::TimeWindow;
use crate::live_hover::{Preview, Target};
use crate::live_source::{Snapshot, Update};
use crate::live_store_app_fixtures::{
    HOUR_NEWEST, apps_at, hour, hour_led_from_its_older_part, hour_until,
};
use crate::live_store_fixtures::{reading_at, trend_point};
use crate::live_store_page_fixtures::{readings, summary_selection, texts};
use crate::live_views::summary;
use crate::time_range::Range;

/// The second the tests hold, 800 seconds before the newest, inside the newest fifteen minutes
/// but outside the newest seven and a half, so a window centred on it would move.
const HELD: u64 = HOUR_NEWEST - 800;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
/// A second of the older tail of the View `show_held_view_after` shows, which holds no Apps
/// there.
const TAIL: u64 = 9_500;
/// The newest second of the View `show_view_over_tail` shows, the last of `held_seconds`, which
/// the worker's readings lack.
const TAIL_VIEW_NEWEST: u64 = 10_005;
/// The first second of the stretch of the older tail that `recording_peaking_in_its_older_tail`
/// peaks in, and the reading the chart draws for that stretch.
const OLDER_STRETCH: u64 = HOUR_NEWEST - 2_000;
/// The second of the peak in that stretch, which the chart draws no point of.
const OLDER_PEAK: u64 = OLDER_STRETCH + 8;
/// The window of the newest fifteen minutes the page draws before the click.
const DRAWN: TimeWindow = TimeWindow {
    first: HOUR_NEWEST - 899,
    span: 900,
};

/// The worker's reply following the newest second of a recording whose older tail reads 10% CPU,
/// with a stretch at `OLDER_STRETCH` that reaches 95% at `OLDER_PEAK`. The reply's own reading
/// reads 33%, which no read of the peak may show.
fn recording_peaking_in_its_older_tail() -> Box<Snapshot> {
    let run = identity_text(collector::ring::Identity([1, 2])).to_string();
    let boot = identity_text(collector::ring::Identity([3, 4])).to_string();
    let mut older: Vec<serde_json::Value> = (0..300_u64)
        .map(|index| {
            let second = OLDER_STRETCH - 1_500 + index * 10;
            serde_json::json!({"run": run, "boot": boot, "segment": 0, "publication": second,
                "wall_ns": second * NANOSECONDS_PER_SECOND, "cpu_percent": 10.0})
        })
        .collect();
    older[150]["reach"] = serde_json::json!({
        "leaves": {"cpu_percent": [10.0, 95.0, 1]},
        "places": [
            [run, boot, OLDER_STRETCH, OLDER_STRETCH * NANOSECONDS_PER_SECOND],
            [run, boot, OLDER_PEAK, OLDER_PEAK * NANOSECONDS_PER_SECOND],
        ],
    });
    let mut reply = readings(&[reading_at(HOUR_NEWEST - 1, 0), reading_at(HOUR_NEWEST, 0)]);
    let selection = reply.selection.legacy_mut().unwrap();
    selection["earlier_stride_seconds"] = serde_json::json!(10);
    selection["earlier_trends"] = serde_json::json!(older);
    selection["summary"] = selection["selected"].clone();
    selection["summary"]["cpu_percent"] = serde_json::json!(33.0);

    reply
}

/// Makes `selection` the reader's answer for `OLDER_PEAK`, whose reading read 95% CPU.
fn older_peak_selection(selection: &mut serde_json::Value) {
    let reading = serde_json::json!({
        "run": identity_text(collector::ring::Identity([1, 2])).to_string(),
        "boot": identity_text(collector::ring::Identity([3, 4])).to_string(),
        "segment": 0,
        "publication": OLDER_PEAK,
        "wall_ns": OLDER_PEAK * NANOSECONDS_PER_SECOND,
        "cpu_percent": 95.0,
    });
    selection["second"] = serde_json::json!(OLDER_PEAK);
    selection["selected"] = reading.clone();
    selection["summary"] = reading.clone();
    selection["trends"] = serde_json::json!([reading]);
}

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
fn a_click_after_a_hover_read_on_another_table_page_reads_the_held_second_again(
    cx: &mut TestAppContext,
) {
    // Goal: a click after a hover that read its second on another page of the process table
    // reads the held second again, at the table's first page that holding it shows, rather than
    // keep the hover's read of the other page. Method: the CPU page over the View of the hour at
    // the newest fifteen minutes, its table at its second page of 512 rows, a hover 800 seconds
    // before the newest on its chart by App that reads its second, a click there, then the
    // commands the reader was sent.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.page_process_table(512);
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.tick_hover();
    let commands = harness.driver.reader_commands();

    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    assert_eq!(harness.driver.reader_commands(), commands + 1);
    harness.teardown();
}

#[gpui::test]
fn a_hover_after_a_click_while_the_page_was_read_reads_its_second(cx: &mut TestAppContext) {
    // Goal: a click while the reader still reads the page forgets that read, so a later hover
    // of another second reads its second rather than wait on a page read that never answers.
    // Method: the CPU page over the View of the hour at the newest fifteen minutes, opened again
    // from the Memory page, which asks the reader for it, a hover and a click 800 seconds before
    // the newest on its chart by App, the answer to the hold's read, a hover 700 seconds before
    // the newest and the hover's step, then the reads the hovers sent.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.navigate("memory");
    harness.driver.navigate("cpu");
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness.driver.answer_held_read();
    harness
        .driver
        .hover_second_on("live-cpu-apps-plot", HELD + 100);

    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 1);
    harness.teardown();
}

#[gpui::test]
fn a_hover_while_the_held_second_is_read_sends_no_read_of_its_own(cx: &mut TestAppContext) {
    // Goal: a hover of another second while the reader still reads the held second waits for
    // that read rather than sending its own, which would cancel it. Method: hover and click 800
    // seconds before the newest on the CPU page's chart by App before the hover reads, so the
    // hold reads its second, hover 700 seconds before the newest and run the hover's step, then
    // the reads the hovers sent.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness
        .driver
        .hover_second_on("live-cpu-apps-plot", HELD + 100);

    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 0);
    harness.teardown();
}

#[gpui::test]
fn the_page_takes_no_reply_to_the_request_out_at_the_click(cx: &mut TestAppContext) {
    // Goal: the page takes no reply to a request it sent before the click, as the reader
    // publishes the live Mac's newer seconds under it, whose readings would replace the held
    // page's. Method: click 800 seconds before the newest on the CPU page's chart by App, then
    // hand the page a reply to the request it followed the live Mac with, reading ten seconds
    // past the newest.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    let generation = harness.driver.request_generation();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    let taken = harness
        .driver
        .reply_to_page_reading(generation, HOUR_NEWEST + 10);

    assert!(!taken);
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
fn a_page_opened_while_a_second_is_held_shows_the_held_second_s_readings_from_its_context(
    cx: &mut TestAppContext,
) {
    // Goal: a page opened while a second is held shows the held second's readings once the
    // reader answers with the context the charts were drawn in, rather than the newest second
    // that context reads. Method: click 800 seconds before the newest on the CPU page's chart by
    // App, move the pointer to the sidebar and open Memory, hand the page the reader's answer,
    // whose context holds the held second, then the second the page's readings show and the
    // text of each summary readout against those of the held second's readings.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Sidebar);
    harness.driver.navigate("memory");

    harness.driver.answer_page_read_holding(HELD);

    assert_eq!(harness.driver.shown_reading_second(), Some(HELD));
    let expected = summary::cards("memory", &summary_selection(&reading_at(HELD, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn a_page_visited_before_a_second_is_held_shows_nothing_it_kept_until_the_reader_answers(
    cx: &mut TestAppContext,
) {
    // Goal: a page the page kept from the live Mac before a second was held is read again in
    // the context the charts were drawn in, and shows nothing of what it kept meanwhile, whose
    // charts end where it was left. Method: open Memory over readings that end 1,000 seconds
    // before the hour's newest, open the CPU page over the View of the hour, click 800 seconds
    // before the newest on its chart by App, move the pointer to the sidebar and open Memory
    // again, then the second the reader was asked for and the text of each summary readout
    // against those of a page still reading.
    let mut harness = InputHarness::open_route(cx, "memory");
    harness
        .driver
        .answer_newest_command_up_to(HOUR_NEWEST - 1_000);
    harness.driver.navigate("cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Sidebar);

    harness.driver.navigate("memory");

    assert_eq!(
        harness.driver.reader_asked_second(),
        Some(HOUR_NEWEST - viewer::CONTEXT_AFTER_SECONDS)
    );
    let expected = summary::loading_cards("memory");
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn overview_opened_while_a_second_is_held_draws_the_held_second_from_the_kept_view_at_once(
    cx: &mut TestAppContext,
) {
    // Goal: Overview opened while a second is held draws its charts and the held second's
    // readings from the store's View the page kept at the click, before the reader answers, so
    // the page shows no loading state. Method: click 800 seconds before the newest on the CPU
    // page's chart by App over the View of the hour, move the pointer to the sidebar and open
    // Overview, then the page's window, the text of each summary readout against those of the
    // held second's readings, and the lines the CPU chart by App draws.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Sidebar);

    harness.driver.navigate("overview");

    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    let expected = summary::cards("overview", &summary_selection(&reading_at(HELD, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["A", "B", "C"]))
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
fn a_click_after_the_view_lands_under_a_still_pointer_holds_the_views_second(
    cx: &mut TestAppContext,
) {
    // Goal: a click holds the second under the pointer as the View the page draws records it,
    // even where that View landed after the hover began over the worker's readings, rather
    // than keeping the hover's record from before the View. Method: Overview over the worker's
    // readings of its two newest seconds, a hover 800 seconds before the newest on the CPU
    // chart, the store's View of the hour under the still pointer, which the hover keeps from
    // the charts, a click there, then the second the CPU chart reads.
    let mut harness = InputHarness::open_route(cx, "overview");
    let newest = readings(&[reading_at(HOUR_NEWEST - 1, 0), reading_at(HOUR_NEWEST, 0)]);
    harness.driver.show_reply_at_fifteen_minutes(newest);
    harness.driver.hover_second_on("live-cpu-plot", HELD);
    assert!(!harness.driver.offer_next_view(hour().0));

    harness.driver.click_second_on("live-cpu-plot", HELD);

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
fn the_reader_rests_once_the_read_of_a_second_held_on_overview_answers(cx: &mut TestAppContext) {
    // Goal: once the read of a held second whose top Apps the store's View lacks answers, the
    // page rests the reader, as it does while it draws Overview from the View, since the hold
    // needs nothing more from it. Method: Overview over a View whose older tail holds 9,500
    // without its Apps, a hover there that reads its second, a click there, the answer to that
    // read, the page's step that rests the reader, then whether the reader rests.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_view_over_tail();
    harness.driver.hover_second_on("live-cpu-plot", TAIL);
    harness.driver.tick_hover();
    harness.driver.click_second_on("live-cpu-plot", TAIL);
    harness.driver.answer_held_read();

    let rested = harness.driver.offer_rest();

    assert!(rested);
    harness.teardown();
}

#[gpui::test]
fn a_click_over_a_rested_reader_on_a_second_the_view_lacks_reads_the_page_first(
    cx: &mut TestAppContext,
) {
    // Goal: a click on Overview over a rested reader, on a second whose top Apps the store's
    // View lacks, asks the reader for the page before the held second, since a rested reader
    // reads a second's own read as a newly opened page. Method: Overview over a View whose older
    // tail holds 9,500 without its Apps, the reader rested, a hover and a click there, the
    // hold's step, which leaves the page's read out, then the second the reader was last asked
    // for, which the page's read names 450 seconds before the held window's last second.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_view_over_tail();
    harness.driver.rest_reader();
    harness.driver.hover_second_on("live-cpu-plot", TAIL);
    harness.driver.tick_hover();

    harness.driver.click_second_on("live-cpu-plot", TAIL);
    harness.driver.tick_hold();

    assert_eq!(
        harness.driver.reader_asked_second(),
        Some(TAIL_VIEW_NEWEST - viewer::CONTEXT_AFTER_SECONDS)
    );
    harness.teardown();
}

#[gpui::test]
fn pause_over_a_rested_reader_asks_it_for_nothing_the_view_holds(cx: &mut TestAppContext) {
    // Goal: Pause on Overview over a rested reader holds the View's newest second, whose
    // readings and top Apps the View holds, and asks the reader for nothing, since a rested
    // reader follows nothing and the page shows nothing of that second the View lacks. Method:
    // Overview over a View five seconds ahead of the reader's readings, the reader rested,
    // Pause, then the commands the reader was sent.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_view_over_tail();
    harness.driver.rest_reader();
    let commands = harness.driver.reader_commands();

    harness.driver.pause();

    assert_eq!(harness.driver.reader_commands(), commands);
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
fn a_click_while_the_app_histories_are_pending_reads_the_page_up_to_the_held_window(
    cx: &mut TestAppContext,
) {
    // Goal: a click while the page's App histories are still being read asks the reader for the
    // page whose context ends at the held window's last second, whose reply brings them, since a
    // read of the held second alone would cancel that read and leave the charts by App unread.
    // Method: the CPU page over the View of the hour at the newest fifteen minutes, its reply
    // without the App histories, a hover and a click 800 seconds before the newest on its chart
    // by App, then the second the reader was last asked for.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness
        .driver
        .show_hour_at_fifteen_minutes_before_app_histories();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);

    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    assert_eq!(
        harness.driver.reader_asked_second(),
        Some(HOUR_NEWEST - viewer::CONTEXT_AFTER_SECONDS)
    );
    harness.teardown();
}

#[gpui::test]
fn the_held_seconds_read_waits_for_a_page_reply_still_reading_the_app_histories(
    cx: &mut TestAppContext,
) {
    // Goal: the page's reply to the read a click sent while the App histories were pending,
    // when it still lacks them, holds back the held second's own read, which would cancel the
    // reader's read of the histories. Method: the CPU page over the View of the hour at the
    // newest fifteen minutes, its reply without the App histories, a hover and a click 800
    // seconds before the newest on its chart by App, a reply to the page's read still without
    // them, the hold's step, then the second the reader was last asked for.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness
        .driver
        .show_hour_at_fifteen_minutes_before_app_histories();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness.driver.reply_to_page_reading_before_app_histories();

    harness.driver.tick_hold();

    assert_eq!(
        harness.driver.reader_asked_second(),
        Some(HOUR_NEWEST - viewer::CONTEXT_AFTER_SECONDS)
    );
    harness.teardown();
}

#[gpui::test]
fn a_click_on_overview_before_the_first_reading_holds_the_second_over_the_view(
    cx: &mut TestAppContext,
) {
    // Goal: a click on Overview in the seconds after launch, which it draws from the store's
    // View before the worker's first reading, holds the second in the window the charts were
    // drawn in and shows its readings from the View, rather than clearing the page to read a
    // window around the second. Method: Overview over the View of the hour at the newest fifteen
    // minutes, which the worker has not answered, a click 800 seconds before the newest on its
    // CPU chart by App, then the page's window and the text of each summary readout against
    // those of the held second's readings.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_before_the_first_reading();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);

    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    let expected = summary::cards("overview", &summary_selection(&reading_at(HELD, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn pause_on_overview_before_the_first_reading_reads_the_page_up_to_the_held_window(
    cx: &mut TestAppContext,
) {
    // Goal: Pause on Overview before the worker's first reading, which holds the View's newest
    // second, asks the reader for the page whose context ends at the held window's last second,
    // since the page has no readings of its own yet and a read of the held second alone would
    // leave them loading while the hold lasts. Method: Overview over the View of the hour at the
    // newest fifteen minutes, which the worker has not answered, Pause, then the second the
    // reader was last asked for.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_before_the_first_reading();

    harness.driver.pause();

    assert_eq!(
        harness.driver.reader_asked_second(),
        Some(HOUR_NEWEST - viewer::CONTEXT_AFTER_SECONDS)
    );
    harness.teardown();
}

#[gpui::test]
fn letting_go_before_the_first_reading_keeps_the_charts_by_app_drawn_from_the_view(
    cx: &mut TestAppContext,
) {
    // Goal: letting go of a hold made before the worker's first reading keeps the charts the
    // page drew from the View it lets go of, its charts by App with them, until the store's next
    // View, rather than leaving Overview's charts empty. Method: Overview over the View of the
    // hour at the newest fifteen minutes, which the worker has not answered, a click 800 seconds
    // before the newest on its CPU chart, Pause, which lets go, then whether the CPU chart by App
    // is on screen and the lines it draws.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_before_the_first_reading();
    harness.driver.hover_second_on("live-cpu-plot", HELD);
    harness.driver.click_second_on("live-cpu-plot", HELD);

    harness.driver.pause();

    assert!(harness.driver.reading_is_visible("live-cpu-apps-plot"));
    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["A", "B", "C"]))
    );
    harness.teardown();
}

#[gpui::test]
fn pause_after_letting_go_before_the_first_reading_holds_the_newest_second_over_the_kept_charts(
    cx: &mut TestAppContext,
) {
    // Goal: Pause on Overview after letting go of a hold made before the worker's first reading,
    // where the page draws only what it kept of the View it let go of, holds the newest second
    // it shows over those charts and reads the page up to them, rather than clearing the page.
    // Method: Overview over the View of the hour at the newest fifteen minutes, which the worker
    // has not answered, Pause, which holds the newest second, Pause again, which lets go, Pause
    // a third time, then the page's window and the second the reader was last asked for.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_before_the_first_reading();
    harness.driver.pause();
    harness.driver.pause();

    harness.driver.pause();

    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    assert_eq!(
        harness.driver.reader_asked_second(),
        Some(HOUR_NEWEST - viewer::CONTEXT_AFTER_SECONDS)
    );
    harness.teardown();
}

#[gpui::test]
fn a_click_after_letting_go_before_the_first_reading_shows_the_held_second_once_the_reader_answers(
    cx: &mut TestAppContext,
) {
    // Goal: a second clicked on Overview after letting go of a hold made before the worker's
    // first reading, where the page draws only what it kept of the View it let go of, shows
    // its readings once the reader answers the page's read, rather than reading unavailable.
    // Method: Overview over the View of the hour at the newest fifteen minutes, which the worker
    // has not answered, a click 800 seconds before the newest on its CPU chart, Pause, which
    // lets go, a click there again, the reader's answer to the page's read, which holds that
    // second, then the text of each summary readout against those of its readings.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_before_the_first_reading();
    harness.driver.hover_second_on("live-cpu-plot", HELD);
    harness.driver.click_second_on("live-cpu-plot", HELD);
    harness.driver.pause();
    harness.driver.hover_second_on("live-cpu-plot", HELD);
    harness.driver.click_second_on("live-cpu-plot", HELD);

    harness.driver.reply_to_page_reading_holding(HELD);

    let expected = summary::cards("overview", &summary_selection(&reading_at(HELD, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn a_click_after_letting_go_before_the_first_reading_inside_an_older_stretch_marks_its_reading(
    cx: &mut TestAppContext,
) {
    // Goal: a second clicked inside an older stretch after letting go of a hold made before the
    // worker's first reading is marked, once the reader answers, at the second the stretch's
    // reading was made, as a click over readings marks it, since every second of the stretch
    // shows that one reading. Method: Overview over the View of the hour at the last 60
    // minutes, which the worker has not answered, a click five seconds into the hour's oldest
    // stretch on its CPU chart, Pause, which lets go, a click there again, the reader's answer
    // to the page's read, which holds that stretch in its older tail, then the marked second.
    let stretch = HOUR_NEWEST - 1_000;
    let clicked = stretch + 5;
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_before_the_first_reading();
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.hover_second_on("live-cpu-plot", clicked);
    harness.driver.click_second_on("live-cpu-plot", clicked);
    harness.driver.pause();
    harness.driver.hover_second_on("live-cpu-plot", clicked);
    harness.driver.click_second_on("live-cpu-plot", clicked);

    harness.driver.reply_to_page_reading_over_older(stretch);

    assert_eq!(harness.driver.pinned_second(), Some(stretch));
    harness.teardown();
}

#[gpui::test]
fn a_click_on_a_saved_recording_selects_the_second_and_reads_the_window_around_it(
    cx: &mut TestAppContext,
) {
    // Goal: a click on a chart of a saved recording selects the second under the pointer and
    // asks the reader for the window around it, rather than holding it in the window the charts
    // were drawn in, since a saved recording has no Store and its window moves with the selected
    // second. Method: the CPU page of a saved recording whose reader answered its newest two
    // seconds at the newest fifteen minutes, a click 800 seconds before the newest on its CPU
    // chart, then the second the reader was last asked for.
    let mut harness = InputHarness::open_recorded_route(cx, "cpu");
    harness.driver.show_recording_at_fifteen_minutes();

    harness.driver.click_second_on("live-cpu-plot", HELD);

    assert_eq!(harness.driver.reader_asked_second(), Some(HELD));
    harness.teardown();
}

#[gpui::test]
fn a_click_on_a_peaks_mark_on_a_saved_recording_shows_the_reading_that_read_it(
    cx: &mut TestAppContext,
) {
    // Goal: a stem in the older tail of a saved recording shows the reading it names, while the
    // reader reads the window around it and after, rather than the first reading of the stretch,
    // which is the one the chart draws there.
    // Method: the CPU page over the last hour, a click on the mark, the second the page shows,
    // the reader's reply for that second, then the second and the reading the page shows.
    let mut harness = InputHarness::open_recorded_route(cx, "cpu");
    harness
        .driver
        .show_reply_at_the_hour(recording_peaking_in_its_older_tail());
    let position = harness
        .driver
        .position_beside_peak("live-cpu-plot", OLDER_PEAK);
    harness.driver.hover_position(position);
    harness.driver.context.run_until_parked();

    harness
        .driver
        .context
        .simulate_click(position, Modifiers::none());
    harness.driver.context.run_until_parked();
    let (shown_during_read, _) = harness.driver.chart_accessibility_reading("live-cpu");
    harness
        .driver
        .reply_with_selection(OLDER_PEAK, OLDER_PEAK + 450, older_peak_selection);
    let (shown, reading) = harness.driver.chart_accessibility_reading("live-cpu");

    assert_eq!(shown_during_read, Some(OLDER_PEAK));
    assert_eq!(shown, Some(OLDER_PEAK));
    assert!(reading.ends_with("95%"), "{reading}");
    harness.teardown();
}

#[gpui::test]
fn a_hover_of_a_peaks_mark_reads_out_the_reading_no_point_of_the_chart_holds(
    cx: &mut TestAppContext,
) {
    // Goal: under a stem the chart reads out what the peak read, though it draws another reading
    // for that stretch, and reads out no other reading while the peak's is on its way.
    // Method: the CPU page over the last hour, a hover of the mark, the chart's reading, the
    // reader's reply to the hover, then the chart's reading.
    let mut harness = InputHarness::open_recorded_route(cx, "cpu");
    harness
        .driver
        .show_reply_at_the_hour(recording_peaking_in_its_older_tail());
    let position = harness
        .driver
        .position_beside_peak("live-cpu-plot", OLDER_PEAK);

    harness.driver.hover_position(position);
    harness.driver.context.run_until_parked();
    let (_, during_read) = harness.driver.chart_accessibility_reading("live-cpu");
    harness.driver.answer_hover(older_peak_selection);
    let (shown, reading) = harness.driver.chart_accessibility_reading("live-cpu");

    assert!(during_read.ends_with("Unavailable"), "{during_read}");
    assert_eq!(shown, Some(OLDER_PEAK));
    assert!(reading.ends_with("95%"), "{reading}");
    harness.teardown();
}

#[gpui::test]
fn a_step_past_the_first_second_of_a_held_window_on_overview_stays_at_that_second(
    cx: &mut TestAppContext,
) {
    // Goal: a keyboard step while a second is held stops at the first second of the window the
    // charts were drawn in, since the page holds no readings of the seconds before it that the
    // charts show. Method: click the second after the first of Overview's window on its CPU
    // chart by App, step a second left on that chart and then another, then the second the
    // page's readings show and the page's window.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness
        .driver
        .hover_second_on("live-cpu-apps-plot", DRAWN.first + 1);
    harness
        .driver
        .click_second_on("live-cpu-apps-plot", DRAWN.first + 1);
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Top);
    harness.driver.step_left_on_cpu_apps();

    harness.driver.press("left");

    assert_eq!(harness.driver.shown_reading_second(), Some(DRAWN.first));
    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    harness.teardown();
}

#[gpui::test]
fn a_step_past_the_last_second_of_a_held_window_on_the_cpu_page_reads_nothing(
    cx: &mut TestAppContext,
) {
    // Goal: a keyboard step while a second is held stops at the last second of the window the
    // charts were drawn in, rather than selecting a later second the recording holds, which
    // would read it and lay the window around it. Method: the CPU page over readings that end
    // 100 seconds before the recording's newest, a click a second before their last on its chart
    // by App, a step a second right on that chart and then another, then the second the page's
    // readings show, the commands the reader was sent, and the page's window.
    let last = HOUR_NEWEST - 100;
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes_up_to(last);
    harness
        .driver
        .hover_second_on("live-cpu-apps-plot", last - 1);
    harness
        .driver
        .click_second_on("live-cpu-apps-plot", last - 1);
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Top);
    harness.driver.step_right_on_cpu_apps();
    let commands = harness.driver.reader_commands();

    harness.driver.press("right");

    assert_eq!(harness.driver.shown_reading_second(), Some(last));
    assert_eq!(harness.driver.reader_commands(), commands);
    let drawn = TimeWindow {
        first: last - 899,
        span: 900,
    };
    assert_eq!(harness.driver.view_window(), Some(drawn));
    harness.teardown();
}

#[gpui::test]
fn a_click_inside_a_window_laid_around_the_held_second_keeps_that_window(cx: &mut TestAppContext) {
    // Goal: a click while a second is held keeps the window the charts show, also where a range
    // picked while held laid the window around the held second, since the window ending where
    // the charts were drawn left it out. Method: Overview over the View of the hour at the last
    // 60 minutes, a click 1,800 seconds before the newest on its CPU chart, the last 5 minutes,
    // a click 100 seconds later inside the window that range shows, then the page's window.
    let held = HOUR_NEWEST - 1_800;
    let clicked = held + 100;
    let laid = TimeWindow {
        first: held - 149,
        span: 300,
    };
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.hover_second_on("live-cpu-plot", held);
    harness.driver.click_second_on("live-cpu-plot", held);
    harness.driver.show_range(Range::Last(300));
    assert_eq!(harness.driver.view_window(), Some(laid));
    harness.driver.hover_second_on("live-cpu-plot", clicked);

    harness.driver.click_second_on("live-cpu-plot", clicked);

    assert_eq!(harness.driver.view_window(), Some(laid));
    harness.teardown();
}

#[gpui::test]
fn a_range_picked_after_a_second_click_is_laid_against_the_second_held_last(
    cx: &mut TestAppContext,
) {
    // Goal: a range picked while a second is held shows the second held last, rather than the
    // second the window was laid against before it. Method: Overview over the View of the hour
    // at the last 60 minutes, a click 1,800 seconds before the newest on its CPU chart, a click
    // 100 seconds before the newest, the last 5 minutes, then the page's window.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.show_range(Range::Last(3_600));
    harness
        .driver
        .hover_second_on("live-cpu-plot", HOUR_NEWEST - 1_800);
    harness
        .driver
        .click_second_on("live-cpu-plot", HOUR_NEWEST - 1_800);
    harness
        .driver
        .hover_second_on("live-cpu-plot", HOUR_NEWEST - 100);
    harness
        .driver
        .click_second_on("live-cpu-plot", HOUR_NEWEST - 100);

    harness.driver.show_range(Range::Last(300));

    let newest = TimeWindow {
        first: HOUR_NEWEST - 299,
        span: 300,
    };
    assert_eq!(harness.driver.view_window(), Some(newest));
    harness.teardown();
}

#[gpui::test]
fn the_custom_stepper_while_a_second_is_held_steps_from_the_minutes_the_charts_show(
    cx: &mut TestAppContext,
) {
    // Goal: the range menu's Custom minutes step from the window the charts show while a second
    // is held, which a window laid around the held second would cut at the loaded hour's oldest
    // second. Method: Overview over the View of the hour at the last 60 minutes, a click 3,500
    // seconds before the newest on its CPU chart, Custom's minus, then the page's window.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.show_range(Range::Last(3_600));
    harness
        .driver
        .hover_second_on("live-cpu-plot", HOUR_NEWEST - 3_500);
    harness
        .driver
        .click_second_on("live-cpu-plot", HOUR_NEWEST - 3_500);

    harness.driver.click_control("time-custom-less");

    let fifty_nine_minutes = TimeWindow {
        first: HOUR_NEWEST - 3_539,
        span: 3_540,
    };
    assert_eq!(harness.driver.view_window(), Some(fifty_nine_minutes));
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
fn a_range_picked_while_held_redraws_the_apps_from_the_kept_view_without_reading(
    cx: &mut TestAppContext,
) {
    // Goal: a range picked while a second is held reads nothing, and the charts by App choose
    // the Apps that led over the new range from the View the page keeps. Method: Overview over
    // the View of an hour in whose two oldest stretches D led, the last 60 minutes, Pause, then
    // the default 15 minutes, which D is absent from, and the commands the reader was sent and the
    // lines the CPU chart by App draws.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_store_view(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    let commands = harness.driver.reader_commands();

    harness.driver.show_range(Range::History);

    assert_eq!(harness.driver.reader_commands(), commands);
    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["A", "B", "C"]))
    );
    harness.teardown();
}

#[gpui::test]
fn letting_go_of_a_hold_keeps_no_part_of_the_view_it_held(cx: &mut TestAppContext) {
    // Goal: letting go of a second held on Overview leaves the page holding nothing of the View
    // the hold kept, so the store, which wakes to catch up, can write over every buffer of it.
    // Method: Overview over the View of the hour at the last 60 minutes, Pause and a tick of the
    // page, which hold the newest second, Pause again, which lets go, then how many owners the
    // View and its newest second have besides the test.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_store_view(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    let view = harness.driver.kept_view();

    harness.driver.pause();

    assert_eq!(Arc::strong_count(&view), 1);
    assert_eq!(Arc::strong_count(view.seconds.last().unwrap()), 1);
    harness.teardown();
}

#[gpui::test]
fn the_range_s_reset_lets_go_of_the_view_a_hold_kept(cx: &mut TestAppContext) {
    // Goal: the range's reset, which lets go of a held second as Pause does, leaves the page
    // holding nothing of the View the hold kept. Method: Overview over the View of the hour at
    // the last 60 minutes, Pause and a tick of the page, then the range's reset, and how many
    // owners the View and its newest second have besides the test.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_store_view(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    let view = harness.driver.kept_view();

    harness.driver.follow_latest();

    assert_eq!(Arc::strong_count(&view), 1);
    assert_eq!(Arc::strong_count(view.seconds.last().unwrap()), 1);
    harness.teardown();
}

#[gpui::test]
fn letting_go_on_overview_shows_the_newest_second_the_view_drew(cx: &mut TestAppContext) {
    // Goal: letting go of a second held on Overview keeps showing what the page drew from the
    // View the hold kept, rather than the older readings of a rested reader, until the store's
    // next View. Method: Overview over the View of the hour, whose reader read only up to 1,000
    // seconds before its newest, at the last 60 minutes, Pause and a tick of the page, which hold
    // the newest second, Pause again, which lets go, then the page's window, the text of each
    // summary readout against those of the View's newest second, and the lines the CPU chart by
    // App draws.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_view_ahead_of_the_reader(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();

    harness.driver.pause();

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: HOUR_NEWEST - 3_599,
            span: 3_600
        })
    );
    let expected = summary::cards("overview", &summary_selection(&reading_at(HOUR_NEWEST, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["D", "A", "B", "C"]))
    );
    harness.teardown();
}

#[gpui::test]
fn the_reader_s_answer_after_letting_go_on_overview_keeps_the_charts_by_app_the_view_drew(
    cx: &mut TestAppContext,
) {
    // Goal: the reader's answer to the live page, which letting go asks for, leaves Overview's
    // charts by App as the View the hold kept drew them until the store's next View, since the
    // reader draws them over fifteen minutes at most. Method: Overview over the View of an hour
    // in whose two oldest stretches D led, at the last 60 minutes, Pause and a tick of the page,
    // Pause again, the reader's answer with the readings of the 50 seconds since, then the lines
    // the CPU chart by App draws.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_store_view(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    harness.driver.pause();

    harness.driver.answer_newest_command_up_to(HOUR_NEWEST + 50);

    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["D", "A", "B", "C"]))
    );
    harness.teardown();
}

#[gpui::test]
fn the_store_s_next_view_after_letting_go_draws_overview_from_it(cx: &mut TestAppContext) {
    // Goal: the store's next View after letting go takes the place of what the page kept of the
    // View the hold kept, so the charts follow the live Mac again. Method: Overview over the View
    // of an hour in whose two oldest stretches D led, at the last 60 minutes, Pause and a tick of
    // the page, Pause again and the reader's answer with the readings of the 50 seconds since,
    // then the View of the hour to that second, in which D does not lead, and the page's window
    // and the lines the CPU chart by App draws.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_store_view(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    harness.driver.pause();
    harness.driver.answer_newest_command_up_to(HOUR_NEWEST + 50);
    let (mut next, _) = hour_until(HOUR_NEWEST + 50);
    next.version = 2;

    harness.driver.take_next_view(next);

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: HOUR_NEWEST + 50 - 3_599,
            span: 3_600
        })
    );
    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["A", "B", "C"]))
    );
    harness.teardown();
}

#[gpui::test]
fn the_reader_s_answer_after_letting_go_on_the_cpu_page_keeps_its_chart_by_app(
    cx: &mut TestAppContext,
) {
    // Goal: on a page other than Overview, the reader's answer to the live page that letting go
    // asks for leaves the chart by App the View the hold kept drew until the store's next View,
    // since the reader draws none at the last 60 minutes. Method: the CPU page over the View of
    // the hour at the last 60 minutes, Pause and a tick of the page, Pause again, the reader's
    // answer with the readings of the 50 seconds since, then the lines the chart by App draws.
    let mut harness = InputHarness::open_route(cx, "cpu");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    harness.driver.pause();

    harness.driver.answer_newest_command_up_to(HOUR_NEWEST + 50);

    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["A", "B", "C"]))
    );
    harness.teardown();
}

#[gpui::test]
fn a_time_typed_after_letting_go_lays_the_window_from_the_reader(cx: &mut TestAppContext) {
    // Goal: a time typed after letting go of a held second, before the store's next View, lays
    // the window around the typed second from the reader's readings, as it does over a View,
    // rather than at the newest second of the View the hold kept. Method: Overview over the View
    // of the hour, whose reader read only up to 1,000 seconds before its newest, at the last 60
    // minutes, Pause, a tick of the page, Pause again, a time typed 1,000 seconds before the
    // newest, then the page's window.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_view_ahead_of_the_reader(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    harness.driver.pause();

    harness.driver.inspect_typed_second(HOUR_NEWEST - 1_000);

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: HOUR_NEWEST - 1_000 - 3_599,
            span: 3_600
        })
    );
    harness.teardown();
}

#[gpui::test]
fn a_window_hidden_after_letting_go_keeps_nothing_the_hold_s_view_drew(cx: &mut TestAppContext) {
    // Goal: a window macOS hides after letting go of a held second, before the store's next
    // View, keeps nothing the page drew from the View the hold kept, as a window hidden while it
    // follows the newest second keeps no View. Method: Overview over the View of the hour, whose
    // reader read only up to 1,000 seconds before its newest, at the last 60 minutes, Pause, a
    // tick of the page, Pause again, the page's step for a window macOS hid, then the page's
    // window, which the reader's readings lay out.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_view_ahead_of_the_reader(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    harness.driver.pause();

    harness.driver.hide_window();

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: HOUR_NEWEST - 1_000 - 3_599,
            span: 3_600
        })
    );
    harness.teardown();
}

#[gpui::test]
fn a_page_opened_after_letting_go_on_overview_draws_its_chart_by_app_from_the_reader(
    cx: &mut TestAppContext,
) {
    // Goal: a page other than Overview opened after letting go of a second held on Overview,
    // before the store's next View, draws its chart by App from the reader, since Overview's
    // charts carry Overview's machine charts and titles. Method: Overview over the View of an
    // hour in whose two oldest stretches D led, at the last 60 minutes, Pause, a tick of the page,
    // Pause again, the CPU page and the reader's answer to it with the readings of the 50 seconds
    // since, which hold no history of the Apps, then the lines of the CPU page's chart by App,
    // of which the reader draws none.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_store_view(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    harness.driver.pause();

    harness.driver.navigate("cpu");
    harness.driver.answer_newest_command_up_to(HOUR_NEWEST + 50);

    assert_eq!(harness.driver.cpu_by_app_lines(), None);
    harness.teardown();
}

#[gpui::test]
fn a_hover_after_letting_go_on_overview_keeps_the_window_the_view_drew(cx: &mut TestAppContext) {
    // Goal: a hover after letting go of a second held on Overview, before the store's next View,
    // keeps the window the View the hold kept laid out, so the charts stay still under the
    // pointer although the reader read only older seconds. Method: Overview over the View of the
    // hour, whose reader read only up to 1,000 seconds before its newest, at the last 60 minutes,
    // Pause, a tick of the page, Pause again, a hover 1,000 seconds before the newest on the CPU
    // chart, then the page's window and the second the hover shows.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_view_ahead_of_the_reader(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    harness.driver.pause();

    harness
        .driver
        .hover_second_on("live-cpu-plot", HOUR_NEWEST - 1_000);

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: HOUR_NEWEST - 3_599,
            span: 3_600
        })
    );
    assert_eq!(
        harness.driver.shown_reading_second(),
        Some(HOUR_NEWEST - 1_000)
    );
    harness.teardown();
}

#[gpui::test]
fn a_hover_after_letting_go_on_overview_leaves_the_readouts_to_the_reader(cx: &mut TestAppContext) {
    // Goal: a hover after letting go of a second held on Overview, before the store's next View,
    // shows the reader's readings of the hovered second on the readouts rather than those of the
    // newest second of the View the hold kept, which is all the page kept of its seconds.
    // Method: Overview over the View of the hour, whose reader read only up to 1,000 seconds
    // before its newest and names none of its seconds, at the last 60 minutes, Pause, a tick of
    // the page, Pause again, a hover 1,000 seconds before the newest on the CPU chart, then the
    // text of each summary readout against those of a second the reader has not read.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_view_ahead_of_the_reader(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    harness.driver.pause();

    harness
        .driver
        .hover_second_on("live-cpu-plot", HOUR_NEWEST - 1_000);

    let expected = summary::cards("overview", &serde_json::Value::Null);
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    harness.teardown();
}

#[gpui::test]
fn a_view_behind_the_reader_after_letting_go_draws_overview_from_the_reader(
    cx: &mut TestAppContext,
) {
    // Goal: the store's next View after letting go of a second held on Overview takes the place
    // of what the page kept of the View the hold kept even where it falls behind the reader, so
    // the page draws the reader's readings, as it does over any View behind them. Method:
    // Overview over the View of an hour in whose two oldest stretches D led, at the last 60
    // minutes, Pause, a tick of the page, Pause again, the reader's answer with the readings of
    // the 50 seconds since, then the View of the hour to 40 seconds since, which the page does not
    // draw from, and the page's window.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_range(Range::History);
    harness
        .driver
        .show_store_view(hour_led_from_its_older_part());
    harness.driver.show_range(Range::Last(3_600));
    harness.driver.pause();
    harness.driver.tick_page();
    harness.driver.pause();
    harness.driver.answer_newest_command_up_to(HOUR_NEWEST + 50);
    let (mut next, _) = hour_until(HOUR_NEWEST + 40);
    next.version = 2;

    let drawn = harness.driver.offer_next_view(next);

    assert!(!drawn);
    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: HOUR_NEWEST + 50 - 3_599,
            span: 3_600
        })
    );
    harness.teardown();
}

#[gpui::test]
fn hiding_and_showing_the_window_over_a_complete_hold_keeps_the_held_page_and_reads_nothing(
    cx: &mut TestAppContext,
) {
    // Goal: a window macOS hides and shows again while a second is held, whose reading the
    // store's View completes, keeps the held second over the charts and readings drawn at the
    // click and sends the reader nothing, since the page shows all it holds. Method: Overview
    // over the View of the hour at the newest fifteen minutes, the reader rested, a hover and a
    // click 800 seconds before the newest on the CPU chart by App, the page's steps for a window
    // macOS hid and showed again, then the second the reader was asked for since it rested, the
    // page's window, and the text of each summary readout against those of the held second's
    // readings.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.rest_reader();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    harness.driver.hide_window();
    harness.driver.show_window();

    assert_eq!(harness.driver.reader_asked_second(), None);
    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    let expected = summary::cards("overview", &summary_selection(&reading_at(HELD, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["A", "B", "C"]))
    );
    harness.teardown();
}

#[gpui::test]
fn hiding_and_showing_the_window_over_a_hold_made_before_the_first_reading_reads_the_page_again(
    cx: &mut TestAppContext,
) {
    // Goal: a window macOS hides and shows again while the page still waits for the readings a
    // hold made before the worker's first reading asked for asks for them again, since hiding
    // forgets the request out and the page has no readings of its own to show. Method: Overview
    // over the View of the hour at the newest fifteen minutes, which the worker has not
    // answered, a hover and a click 800 seconds before the newest on the CPU chart by App, the
    // page's steps for a window macOS hid and showed again, then the commands the reader was
    // sent since the window was hidden and the second it was last asked for.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_before_the_first_reading();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);
    harness.driver.hide_window();
    let commands = harness.driver.reader_commands();

    harness.driver.show_window();

    assert_eq!(harness.driver.reader_commands(), commands + 1);
    let page = HOUR_NEWEST - viewer::CONTEXT_AFTER_SECONDS;
    assert_eq!(harness.driver.reader_asked_second(), Some(page));
    harness.teardown();
}

#[gpui::test]
fn closing_and_opening_the_window_over_a_complete_hold_keeps_the_held_page_and_reads_nothing(
    cx: &mut TestAppContext,
) {
    // Goal: a window closed and opened again while a second is held, whose reading the store's
    // View completes, keeps the held second over the charts and readings drawn at the click and
    // sends the reader nothing, as a window macOS hid and showed again does. Method: Overview
    // over the View of the hour at the newest fifteen minutes, the reader rested, a hover and a
    // click 800 seconds before the newest on the CPU chart by App, the window closed and opened
    // from what the app keeps of it, then the second the reader was asked for since it rested,
    // the page's window, the text of each summary readout against those of the held second's
    // readings, and the lines the CPU chart by App draws.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_hour_at_fifteen_minutes();
    harness.driver.rest_reader();
    harness.driver.hover_second_on("live-cpu-apps-plot", HELD);
    harness.driver.click_second_on("live-cpu-apps-plot", HELD);

    harness.driver.close_and_open_window();

    assert_eq!(harness.driver.reader_asked_second(), None);
    assert_eq!(harness.driver.view_window(), Some(DRAWN));
    let expected = summary::cards("overview", &summary_selection(&reading_at(HELD, 0)));
    assert_eq!(harness.driver.readout_texts(), texts(&expected));
    assert_eq!(
        harness.driver.cpu_by_app_lines(),
        Some(cpu_by_app_lines_drawing(&["A", "B", "C"]))
    );
    harness.teardown();
}

#[gpui::test]
fn closing_and_opening_the_window_over_a_hold_still_reading_asks_for_the_page_it_was_held_in(
    cx: &mut TestAppContext,
) {
    // Goal: a window closed and opened again while the reader still reads the held second asks
    // for the page whose context ends where the charts were drawn, which the held second's own
    // read follows, rather than for the held second, whose context would move the window.
    // Method: Overview over a View whose older tail holds 9,500 without its Apps, a hover there
    // on the CPU chart and its read, a click, which keeps that read, the window closed and opened
    // from what the app keeps of it, then the second the reader was last asked for.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness.driver.show_view_over_tail();
    harness.driver.hover_second_on("live-cpu-plot", TAIL);
    harness.driver.tick_hover();
    harness.driver.click_second_on("live-cpu-plot", TAIL);
    let page = TAIL_VIEW_NEWEST - viewer::CONTEXT_AFTER_SECONDS;

    harness.driver.close_and_open_window();

    assert_eq!(harness.driver.reader_asked_second(), Some(page));
    harness.teardown();
}

#[gpui::test]
fn closing_and_opening_the_window_while_it_follows_the_live_mac_keeps_no_part_of_its_view(
    cx: &mut TestAppContext,
) {
    // Goal: a window closed while it follows the live Mac keeps nothing of the store's View its
    // charts were drawn from, so the store can write over every buffer of it while the window
    // stays closed and as it catches up once the window opens again. Method: Overview over the
    // View of the hour, the window closed and opened from what the app keeps of it, then how
    // many owners the View and its newest second have besides the test.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness
        .driver
        .show_store_view(hour_led_from_its_older_part());
    let view = harness.driver.kept_view();

    harness.driver.close_and_open_window();

    assert_eq!(Arc::strong_count(&view), 1);
    assert_eq!(Arc::strong_count(view.seconds.last().unwrap()), 1);
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

/// The View of the hour, at whose second `peak` the CPU reads 95% where it reads 10% elsewhere.
fn hour_peaking_at(peak: u64) -> viewer::store::View {
    let (mut view, _) = hour();
    for second in &mut view.seconds {
        let wall_second = second.wall_ns / 1_000_000_000;
        let cpu = if wall_second == peak { 95.0 } else { 10.0 };
        *second = Arc::new(viewer::store::MachineSecond {
            cpu_percent: Some(cpu),
            ..viewer::store::MachineSecond::clone(second)
        });
    }
    view
}

/// `view` with another run having read the wall second `peak` after the reading of it the View
/// holds, a second of 10% whose tick is `publication`.
fn hour_with_a_second_reading_of(
    mut view: viewer::store::View,
    peak: u64,
    publication: u64,
) -> viewer::store::View {
    let index = view
        .seconds
        .iter()
        .position(|second| second.wall_ns / 1_000_000_000 == peak)
        .unwrap();
    let mut other = viewer::store::MachineSecond::clone(&view.seconds[index]);
    other.cpu_percent = Some(10.0);
    other.run = [7, 8];
    other.publication = publication;
    other.segment += 1;
    let mut apps = apps_at(&other, &[]);
    let observation = apps.observation.as_mut().unwrap();
    observation.stamp.run = [7, 8];
    observation.stamp.publication = publication;
    view.seconds.insert(index + 1, Arc::new(other));
    view.apps.insert(index + 1, Arc::new(apps));
    view
}

#[gpui::test]
fn a_click_on_a_peaks_mark_ranks_the_short_lived_app_no_chart_draws(cx: &mut TestAppContext) {
    // Goal: an App that ran for the one second of a peak is found from the peak, though its mean
    // over the range puts it on no line of a chart by App.
    // Method: Overview of an hour whose CPU peaks at one second, whose Apps that second are led
    // by one no other second holds. Read the chart by App, click the mark, read the top Apps.
    let mut harness = InputHarness::open_route(cx, "overview");
    let mut view = hour_peaking_at(HELD);
    let at = view
        .seconds
        .iter()
        .position(|second| second.wall_ns / 1_000_000_000 == HELD)
        .unwrap();
    view.apps[at] = Arc::new(apps_at(
        &view.seconds[at],
        &[("Brief", 950_000_000), ("A", 10_000_000)],
    ));
    harness.driver.show_view_at_fifteen_minutes(view);
    let drawn = harness.driver.chart_line_names("live-cpu-apps");
    assert!(drawn.iter().any(|name| name == "A"), "{drawn:?}");
    assert!(!drawn.iter().any(|name| name == "Brief"), "{drawn:?}");
    let position = harness.driver.position_beside_peak("live-cpu-plot", HELD);

    harness.driver.hover_position(position);
    harness.driver.context.run_until_parked();
    harness
        .driver
        .context
        .simulate_click(position, Modifiers::none());
    harness.driver.context.run_until_parked();

    assert_eq!(harness.driver.ranked_second(), HELD);
    assert_eq!(harness.driver.leading_cpu_app(), "Brief");
    harness.teardown();
}

#[gpui::test]
fn a_peaks_mark_stays_a_target_through_a_hold_and_after_letting_go(cx: &mut TestAppContext) {
    // Goal: holding another second and letting go leave a peak's mark where it was, so a peak seen
    // before a hold can still be held during it and after it.
    // Method: Overview of an hour whose CPU peaks at one second. Hold another second, click beside
    // the mark, let go, and click beside it again, reading what the page asks the reader for.
    let mut harness = InputHarness::open_route(cx, "overview");
    harness
        .driver
        .show_view_at_fifteen_minutes(hour_peaking_at(HELD));
    let plot = "live-cpu-plot";

    harness.driver.click_second_on(plot, HELD + 200);
    assert_eq!(harness.driver.requested_second(), Some(HELD + 200));
    let during_hold = harness.driver.position_beside_peak(plot, HELD);
    harness.driver.hover_position(during_hold);
    harness.driver.context.run_until_parked();
    harness
        .driver
        .context
        .simulate_click(during_hold, Modifiers::none());
    harness.driver.context.run_until_parked();
    assert_eq!(harness.driver.requested_second(), Some(HELD));

    harness.driver.follow_latest();
    harness.driver.context.run_until_parked();
    assert_ne!(harness.driver.requested_second(), Some(HELD));
    let after_resume = harness.driver.position_beside_peak(plot, HELD);
    harness.driver.hover_position(after_resume);
    harness.driver.context.run_until_parked();
    harness
        .driver
        .context
        .simulate_click(after_resume, Modifiers::none());
    harness.driver.context.run_until_parked();

    assert_eq!(harness.driver.requested_second(), Some(HELD));
    harness.teardown();
}

#[gpui::test]
fn a_click_on_a_peaks_mark_holds_the_reading_that_read_it(cx: &mut TestAppContext) {
    // Goal: a peak's mark is a target of its own, so a click within reach of it holds the reading
    // the mark stands for, and that reading's own second, rather than the second under the pointer.
    // Method: Overview of an hour whose CPU peaks at one second, a click eight points before the
    // peak near the top of the plot, then what the page asks the reader for.
    let mut harness = InputHarness::open_route(cx, "overview");
    let view = hour_peaking_at(HELD);
    harness.driver.show_view_at_fifteen_minutes(view);
    let position = harness.driver.position_beside_peak("live-cpu-plot", HELD);
    assert_ne!(
        harness.driver.second_at_plot_x("live-cpu-plot", position.x),
        Some(HELD)
    );

    harness.driver.hover_position(position);
    harness.driver.context.run_until_parked();
    harness
        .driver
        .context
        .simulate_click(position, Modifiers::none());
    harness.driver.context.run_until_parked();

    assert_eq!(harness.driver.requested_second(), Some(HELD));
    assert_eq!(
        harness.driver.requested_identity(),
        Some(serde_json::json!({
            "run": identity_text(collector::ring::Identity([1, 2])).to_string(),
            "boot": identity_text(collector::ring::Identity([3, 4])).to_string(),
            "publication": HELD,
        }))
    );
    harness.teardown();
}

#[gpui::test]
fn a_click_on_a_peaks_mark_holds_its_run_where_another_read_the_same_second(
    cx: &mut TestAppContext,
) {
    // Goal: the peak's own reading is held where two runs read its wall second, though a plain
    // click on that second holds the newest of them.
    // Method: the hour with a second reading of the peak's wall second by another run, read after
    // the peak, a click on the mark, then the run and publication the page asks the reader for.
    let mut harness = InputHarness::open_route(cx, "overview");
    let view = hour_with_a_second_reading_of(hour_peaking_at(HELD), HELD, 900_000);
    harness.driver.show_view_at_fifteen_minutes(view);
    let position = harness.driver.position_beside_peak("live-cpu-plot", HELD);

    harness.driver.hover_position(position);
    harness.driver.context.run_until_parked();
    harness
        .driver
        .context
        .simulate_click(position, Modifiers::none());
    harness.driver.context.run_until_parked();

    let identity = harness.driver.requested_identity().unwrap();
    assert_eq!(identity["publication"], HELD);
    assert_eq!(
        identity["run"],
        identity_text(collector::ring::Identity([1, 2])).to_string()
    );
    harness.teardown();
}

impl InputDriver {
    /// Shows the page over the View of the hour and the worker's reply to its request, the
    /// readings of the hour's newest two seconds, following the newest second at the newest
    /// fifteen minutes.
    fn show_hour_at_fifteen_minutes(&mut self) {
        self.show_hour_at_fifteen_minutes_up_to(HOUR_NEWEST);
    }

    /// Shows the page over the View of the hour and the worker's reply to its request, the
    /// readings of the two seconds up to `newest`, following that second at the newest fifteen
    /// minutes.
    fn show_hour_at_fifteen_minutes_up_to(&mut self, newest: u64) {
        let reply = readings(&[reading_at(newest - 1, 0), reading_at(newest, 0)]);
        self.show_hour_at_fifteen_minutes_answered(reply);
    }

    /// Shows the page over the View of the hour and the worker's first reply to its request, the
    /// readings of the hour's newest two seconds without the App histories it reads next.
    fn show_hour_at_fifteen_minutes_before_app_histories(&mut self) {
        let mut reply = readings(&[reading_at(HOUR_NEWEST - 1, 0), reading_at(HOUR_NEWEST, 0)]);
        reply.histories_pending = true;
        self.show_hour_at_fifteen_minutes_answered(reply);
    }

    /// Shows the page over the View of the hour and `reply`, the worker's reply to its request,
    /// following the newest second of `reply` at the newest fifteen minutes.
    fn show_hour_at_fifteen_minutes_answered(&mut self, reply: Box<Snapshot>) {
        let (view, _) = hour();
        self.show_reply_at_fifteen_minutes(reply);
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.take_view(Arc::new(view)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Shows the page over `view` and the worker's reply to its request, the readings of the
    /// hour's newest two seconds, following the newest second at the newest fifteen minutes.
    fn show_view_at_fifteen_minutes(&mut self, view: viewer::store::View) {
        let reply = readings(&[reading_at(HOUR_NEWEST - 1, 0), reading_at(HOUR_NEWEST, 0)]);
        self.show_reply_at_fifteen_minutes(reply);
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.take_view(Arc::new(view)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// A place eight points before the mark of a peak at `second` on the chart whose plot is
    /// `plot`, close enough to its top to be on any peak that stands beyond its axis.
    fn position_beside_peak(
        &mut self,
        plot: &'static str,
        second: u64,
    ) -> gpui::Point<gpui::Pixels> {
        let at = self.held_second_position(plot, second);
        let bounds = self.plot_bounds(plot);

        gpui::point(at.x - gpui::px(8.0), bounds.top() + gpui::px(1.0))
    }

    /// The second a plain hover at `x` across the chart whose plot is `plot` shows.
    fn second_at_plot_x(&mut self, plot: &'static str, x: gpui::Pixels) -> Option<u64> {
        let bounds = self.plot_bounds(plot);
        let fraction = f32::from(x - bounds.left()) / f32::from(bounds.size.width);

        self.view_window()?.second_at(fraction)
    }

    /// Hands the page the reader's answer to the hover's read, its selection changed by `edit`.
    fn answer_hover(&mut self, edit: impl FnOnce(&mut serde_json::Value)) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let generation = dashboard.preview.as_ref().unwrap().generation.unwrap();
            let mut snapshot = super::accepted_snapshot(generation);
            edit(snapshot.selection.legacy_mut().unwrap());
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// The identity of the observation the page's request names.
    fn requested_identity(&self) -> Option<serde_json::Value> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.request.identity.clone()
        })
    }

    /// Shows the page of a saved recording over the worker's reply to its request, the readings
    /// of the recording's newest two seconds, following the newest second at the newest fifteen
    /// minutes.
    fn show_recording_at_fifteen_minutes(&mut self) {
        let reply = readings(&[reading_at(HOUR_NEWEST - 1, 0), reading_at(HOUR_NEWEST, 0)]);
        self.show_reply_at_fifteen_minutes(reply);
    }

    /// Shows the page over `reply`, the worker's reply to its request, following the newest
    /// second of `reply` at the newest fifteen minutes.
    fn show_reply_at_fifteen_minutes(&mut self, reply: Box<Snapshot>) {
        self.show_range(Range::History);
        self.show_reply(reply);
    }

    /// Shows the page over `reply`, following its newest second over the last hour.
    fn show_reply_at_the_hour(&mut self, reply: Box<Snapshot>) {
        self.show_range(Range::Last(3_600));
        self.show_reply(reply);
    }

    /// Hands the page `reply`, the worker's reply to its request, in a window that fits its charts.
    fn show_reply(&mut self, mut reply: Box<Snapshot>) {
        self.view.update(&mut self.context, |dashboard, cx| {
            reply.generation = dashboard.generation;
            assert!(dashboard.take_update(Update::Ready(reply)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));
        self.context.run_until_parked();
    }

    /// Shows the page over the View of the hour at the newest fifteen minutes, as a window opened
    /// at launch draws it before the worker's first reading.
    fn show_hour_before_the_first_reading(&mut self) {
        let (view, _) = hour();
        self.show_range(Range::History);
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.take_view(Arc::new(view)));
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));
        self.context.run_until_parked();
    }

    /// Shows the page over `view` and the worker's reply to its request, the readings of the two
    /// seconds 1,000 seconds before the hour's newest, as a reader that rested reads them.
    fn show_view_ahead_of_the_reader(&mut self, view: viewer::store::View) {
        let read = [
            reading_at(HOUR_NEWEST - 1_001, 0),
            reading_at(HOUR_NEWEST - 1_000, 0),
        ];
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.accept(readings(&read));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            assert!(dashboard.take_view(Arc::new(view)));
            cx.notify();
        });
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

    /// Hands the page the reader's answer to its newest command, the readings of the hour's
    /// newest two seconds with `second` among the older seconds of their context.
    fn answer_page_read_holding(&mut self, second: u64) {
        let older = reading_at(second, 0);
        let mut reply = readings(&[
            older,
            reading_at(HOUR_NEWEST - 1, 0),
            reading_at(HOUR_NEWEST, 0),
        ]);
        reply.generation = self.reader_commands();
        self.view.update(&mut self.context, |dashboard, cx| {
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

    /// Hands the page the reader's answer to its newest command, the live readings of the two
    /// seconds up to `newest`, which the page takes.
    fn answer_newest_command_up_to(&mut self, newest: u64) {
        let mut reply = readings(&[reading_at(newest - 1, 0), reading_at(newest, 0)]);
        reply.generation = self.reader_commands();
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.take_update(Update::Ready(reply)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Selects `second` as a time typed into the time field does.
    fn inspect_typed_second(&mut self, second: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.inspect(second, cx);
        });
        self.context.run_until_parked();
    }

    /// Reports the window hidden as macOS does, then runs the page's step for it, which rests
    /// the reader.
    fn hide_window(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.observe_visibility_callback(false, cx);
            dashboard.tick_presence(cx);
        });
        self.context.run_until_parked();
    }

    /// Reports the window shown again as macOS does, then runs the page's step for it.
    fn show_window(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.observe_visibility_callback(true, cx);
            dashboard.tick_presence(cx);
        });
        self.context.run_until_parked();
    }

    /// Closes the window and opens it again from what the app keeps of a closed window, its
    /// state and its resting reader.
    fn close_and_open_window(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            // The app hands the reader over before it records the window's state.
            let worker = dashboard.worker.hand_over().unwrap().unwrap();
            let state = dashboard.window_state();
            let native = std::mem::take(&mut dashboard.native);
            *dashboard = crate::live_dashboard::LiveDashboard::restore(worker, state, cx);
            dashboard.native = native;
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Hands the page `view`, the store's next View, as a tick of the page takes it, which the
    /// page draws from.
    fn take_next_view(&mut self, view: viewer::store::View) {
        assert!(self.offer_next_view(view));
    }

    /// Hands the page `view`, the store's next View, as a tick of the page takes it, and answers
    /// whether the page drew from it.
    fn offer_next_view(&mut self, view: viewer::store::View) -> bool {
        let drawn = self.view.update(&mut self.context, |dashboard, cx| {
            let drawn = dashboard.take_view(Arc::new(view));
            cx.notify();
            drawn
        });
        self.context.run_until_parked();
        drawn
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

    /// Hands the page the reader's answer to its last command, the read of a second, with that
    /// second's readings as the page's readings hold them.
    fn answer_held_read(&mut self) {
        let generation = self.reader_commands();
        self.view.update(&mut self.context, |dashboard, cx| {
            let request = dashboard.worker.sent_request().unwrap();
            let target = Target {
                second: request.second.unwrap(),
                identity: request.identity.clone(),
            };
            let snapshot = dashboard.snapshot.as_deref().unwrap();
            let mut reply = Preview::resident(snapshot, &request, target).reading;
            reply.generation = generation;
            assert!(dashboard.take_update(Update::Ready(reply)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Focuses the CPU page's chart by App with a right press, which selects no second, and
    /// steps it a second left.
    fn step_left_on_cpu_apps(&mut self) {
        self.step_on_cpu_apps("left");
    }

    /// Focuses the CPU page's chart by App with a right press, which selects no second, and
    /// steps it a second right.
    fn step_right_on_cpu_apps(&mut self) {
        self.step_on_cpu_apps("right");
    }

    /// Focuses the CPU page's chart by App with a right press, which selects no second, and
    /// presses the arrow `key` on it.
    fn step_on_cpu_apps(&mut self, key: &str) {
        let bounds = self.context.debug_bounds("live-cpu-apps-plot").unwrap();
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
        self.press(key);
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

    /// The name of each line of the chart `panel_id`.
    fn chart_line_names(&self, panel_id: &str) -> Vec<String> {
        self.view.read_with(&self.context, |dashboard, _| {
            let panel = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == panel_id)
                .unwrap();

            panel.lines.iter().map(|line| line.name.clone()).collect()
        })
    }

    /// The name of the App the page's CPU ranking leads with.
    fn leading_cpu_app(&self) -> String {
        self.view.read_with(&self.context, |dashboard, _| {
            let (rankings, _) = dashboard.ranking_reading().unwrap();
            let cpu = rankings
                .metrics
                .iter()
                .find(|ranking| ranking.metric == crate::live_rankings::Metric::Cpu)
                .unwrap();

            cpu.entries[0].app.name.clone()
        })
    }

    /// The second the page's top Apps read.
    fn ranked_second(&self) -> u64 {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.ranking_reading().unwrap().1.second
        })
    }

    /// The store's View the page keeps.
    fn kept_view(&self) -> Arc<viewer::store::View> {
        self.view.read_with(&self.context, |dashboard, _| {
            Arc::clone(dashboard.store_view.as_ref().unwrap())
        })
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

    /// Hands the page the reader's answer to its last request, the live readings of `held` and
    /// of the hour's newest two seconds.
    fn reply_to_page_reading_holding(&mut self, held: u64) {
        self.reply_to_last_request(readings(&[
            reading_at(held, 0),
            reading_at(HOUR_NEWEST - 1, 0),
            reading_at(HOUR_NEWEST, 0),
        ]));
    }

    /// Hands the page the reader's answer to its last request, the live readings of the hour's
    /// newest two seconds with the reading of the stretch from `older` in their older tail.
    fn reply_to_page_reading_over_older(&mut self, older: u64) {
        let mut reply = readings(&[reading_at(HOUR_NEWEST - 1, 0), reading_at(HOUR_NEWEST, 0)]);
        reply.selection.legacy_mut().unwrap()["earlier_trends"] =
            vec![trend_point(&reading_at(older, 0))].into();
        self.reply_to_last_request(reply);
    }

    /// Hands the page the reader's answer to its last request, the live readings of the hour's
    /// newest two seconds, whose App histories the reader still reads.
    fn reply_to_page_reading_before_app_histories(&mut self) {
        let mut reply = readings(&[reading_at(HOUR_NEWEST - 1, 0), reading_at(HOUR_NEWEST, 0)]);
        reply.histories_pending = true;
        self.reply_to_last_request(reply);
    }

    /// Shows the page's process table from row `offset`, as its paging does.
    fn page_process_table(&mut self, offset: u64) {
        self.view.update(&mut self.context, |dashboard, _| {
            dashboard.request.offset = offset;
        });
    }

    /// Hands the page `reply` as the reader's answer to its last request.
    fn reply_to_last_request(&mut self, mut reply: Box<Snapshot>) {
        reply.generation = self.reader_commands();
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.take_update(Update::Ready(reply)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Hands the page the reply to its request of `generation`, the live readings up to
    /// `newest`. Answers whether the page took it.
    fn reply_to_page_reading(&mut self, generation: u64, newest: u64) -> bool {
        let mut reply = readings(&[reading_at(newest - 1, 0), reading_at(newest, 0)]);
        reply.generation = generation;
        let taken = self.view.update(&mut self.context, |dashboard, cx| {
            let taken = dashboard.take_update(Update::Ready(reply));
            cx.notify();
            taken
        });

        self.context.run_until_parked();
        taken
    }
}
