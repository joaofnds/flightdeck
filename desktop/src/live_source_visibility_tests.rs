use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{AppContext, TestAppContext, WindowHandle, WindowOptions, WindowVisibility};
use serde_json::{Value, json};
use viewer::store::MachineSecond;

use super::{SealedRecording, SnapshotReader};
use crate::live_dashboard::LiveDashboard;
use crate::live_hover::{Origin, Target};
use crate::live_source::{Received, Snapshot, Update};
use crate::live_store_fixtures::{reading_at, view};
use crate::live_store_page_fixtures::readings;

#[gpui::test]
fn a_hidden_window_leaves_its_reader_no_page_to_read(cx: &mut TestAppContext) {
    // Goal: a window macOS hides reads no page, since nobody sees the readings it would show.
    // Method: open a window over a reader the test steps, take its first request, hide the
    // window, and step the reader's mailbox.
    let mut window = VisibilityHarness::open(cx);

    window.change(WindowVisibility::Hidden);

    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

#[gpui::test]
fn a_window_opened_hidden_reads_its_first_page(cx: &mut TestAppContext) {
    // Goal: a newly opened window reads its first page though macOS reports it hidden, since
    // macOS reports every new window hidden until it puts it on the screen.
    // Method: open a window that macOS reports hidden before the dashboard observes it, step
    // its dashboard, and step the reader's mailbox.
    let mut window = VisibilityHarness::open_unobserved(cx);
    window.report(WindowVisibility::Hidden);

    window.observe();
    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, true));
}

#[gpui::test]
fn a_window_still_hidden_at_its_first_page_leaves_its_reader_no_page_to_read(
    cx: &mut TestAppContext,
) {
    // Goal: a window macOS hides from the start, behind other windows or on another Space,
    // reads no page past its first, though macOS reports no change of its visibility.
    // Method: open a window that macOS reports hidden before the dashboard observes it, answer
    // its first request, step its dashboard, and step the reader's mailbox.
    let mut window = VisibilityHarness::open_unobserved(cx);
    window.report(WindowVisibility::Hidden);
    window.observe();

    window.answer();
    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

#[gpui::test]
fn a_window_shown_before_its_first_page_keeps_reading_its_page(cx: &mut TestAppContext) {
    // Goal: a window that opens as macOS puts it on the screen keeps following the recording
    // past its first page.
    // Method: open a window that macOS reports hidden before the dashboard observes it, show
    // it, answer its first request, step its dashboard, and step the reader's mailbox.
    let mut window = VisibilityHarness::open_unobserved(cx);
    window.report(WindowVisibility::Hidden);
    window.observe();
    window.report(WindowVisibility::Visible);

    window.answer();
    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, true));
}

#[gpui::test]
fn a_window_shown_again_asks_its_reader_for_its_page(cx: &mut TestAppContext) {
    // Goal: a window shown again reads its page in the reader's next step, so it shows the
    // latest readings rather than those of the second it was hidden at.
    // Method: hide the window, show it again, and step the reader's mailbox.
    let mut window = VisibilityHarness::open(cx);
    window.change(WindowVisibility::Hidden);

    window.change(WindowVisibility::Visible);

    assert_eq!(window.reader.receive(), (Received::Taken, true));
}

#[gpui::test]
fn a_pinned_window_shown_again_keeps_its_reading_without_waking_its_reader(
    cx: &mut TestAppContext,
) {
    // Goal: showing a completed pinned view cannot trigger live ingestion or reread its page.
    // Method: answer an exact pin, hide and show the window, then compare request generations.
    let mut window = VisibilityHarness::open(cx);
    window.answer_readings(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    window.tick();
    window
        .window
        .update(&mut window.cx, |dashboard, _, cx| {
            let identity = dashboard.snapshot.as_ref().unwrap().selection["selected"].clone();
            dashboard.pin_target(
                Target {
                    second: 10_000,
                    identity: Some(identity),
                },
                cx,
            );
        })
        .unwrap();
    assert_eq!(window.reader.receive(), (Received::Continued, true));
    window.answer_readings(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    window.tick();
    window.change(WindowVisibility::Hidden);
    assert_eq!(window.reader.receive(), (Received::Continued, false));
    let generation = window.generation();

    window.change(WindowVisibility::Visible);

    assert_eq!(window.generation(), generation);
    assert!(!window.awaiting_reply());
}

#[gpui::test]
fn a_shown_window_keeps_reading_its_page(cx: &mut TestAppContext) {
    // Goal: only a hidden window stops reading, so a shown one follows the recording.
    // Method: step the window's dashboard with no visibility change, then the reader's mailbox.
    let mut window = VisibilityHarness::open(cx);

    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, true));
}

#[gpui::test]
fn a_window_shown_again_before_its_reader_rested_keeps_reading_its_page(cx: &mut TestAppContext) {
    // Goal: a window hidden for less than a tick neither rests its reader nor asks again for the
    // page the reader still reads.
    // Method: hide the window and show it again with no tick between, then step the dashboard and
    // the reader's mailbox.
    let mut window = VisibilityHarness::open(cx);
    let generation = window.generation();
    window.report(WindowVisibility::Hidden);

    window.change(WindowVisibility::Visible);

    assert_eq!(window.reader.receive(), (Received::Continued, true));
    assert_eq!(window.generation(), generation);
}

#[gpui::test]
fn a_window_hidden_again_before_it_asked_for_its_page_leaves_its_reader_resting(
    cx: &mut TestAppContext,
) {
    // Goal: a window shown for less than a tick asks its resting reader for no page.
    // Method: hide the window, show it and hide it again with no tick between, then step the
    // dashboard and the reader's mailbox.
    let mut window = VisibilityHarness::open(cx);
    window.change(WindowVisibility::Hidden);
    window.report(WindowVisibility::Visible);

    window.change(WindowVisibility::Hidden);

    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

#[gpui::test]
fn a_window_reported_hidden_polls_quickly_until_its_reader_rests(cx: &mut TestAppContext) {
    // Goal: a window macOS hides rests its reader within a frame rather than at the
    // quarter-second tick.
    // Method: answer the window's first request, hide the window with no step of its
    // dashboard after it, and ask whether the window still waits on its reader.
    let mut window = VisibilityHarness::open(cx);
    window.answer();
    window.tick();

    window.report(WindowVisibility::Hidden);

    assert!(window.awaiting_reply());
}

#[gpui::test]
fn a_hidden_window_waits_on_no_reply_from_its_resting_reader(cx: &mut TestAppContext) {
    // Goal: a hidden window stops the quick poll once its reader rests, since the reader
    // answers nothing until the window shows again.
    // Method: hide the window while its first request is unanswered, and ask whether the window
    // still waits on its reader.
    let mut window = VisibilityHarness::open(cx);

    window.change(WindowVisibility::Hidden);

    assert!(!window.awaiting_reply());
}

#[gpui::test]
fn a_window_hidden_before_its_histories_arrive_waits_on_no_reply(cx: &mut TestAppContext) {
    // Goal: a window hidden while its first readings leave their App histories to a second
    // read stops the quick poll, since its resting reader reads none until the window shows.
    // Method: open a window that macOS reports hidden, answer its first request with readings
    // whose App histories are still to come, step its dashboard, and ask whether the window
    // still waits on its reader.
    let mut window = VisibilityHarness::open_unobserved(cx);
    window.report(WindowVisibility::Hidden);
    window.observe();

    window.answer_leaving_histories();
    window.tick();

    assert!(!window.awaiting_reply());
}

#[gpui::test]
fn a_window_hidden_during_a_hover_ends_it(cx: &mut TestAppContext) {
    // Goal: hiding a window ends the hover it showed, whose read its resting reader drops.
    // Method: answer the window's first request, hover a second the page does not hold, hide
    // the window, and ask whether it still shows the hover.
    let mut window = VisibilityHarness::open(cx);
    window.answer();
    window.tick();
    window.hover();

    window.change(WindowVisibility::Hidden);

    assert!(!window.hovering());
}

#[gpui::test]
fn a_hidden_window_leaves_its_reader_no_hover_to_read(cx: &mut TestAppContext) {
    // Goal: a hover that starts while the window is hidden, as one ending on the hide shows the
    // selected second again, sends its resting reader no read until the window shows.
    // Method: answer the window's first request, hide the window, hover a second the page does
    // not hold, step the dashboard, and step the reader's mailbox.
    let mut window = VisibilityHarness::open(cx);
    window.answer();
    window.tick();
    window.change(WindowVisibility::Hidden);
    window.hover();

    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

#[gpui::test]
fn a_shown_overview_drawn_from_the_store_rests_its_reader(cx: &mut TestAppContext) {
    // Goal: a shown Overview whose charts follow the store's View leaves its reader no page to
    // read, since the View brings each second the page shows.
    // Method: answer the window's first request with readings of two seconds, give the window a
    // View of newer seconds, step its dashboard, and step the reader's mailbox.
    let mut window = VisibilityHarness::open(cx);
    window.answer_readings(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    window.tick();
    window.take_view(&[reading_at(10_000, 0), reading_at(10_001, 0)]);

    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

#[gpui::test]
fn a_rested_overview_leaves_its_resting_reader_alone(cx: &mut TestAppContext) {
    // Goal: an Overview that rested its reader cancels nothing on its later steps, since each
    // cancellation stops the resting reader's following of the recording.
    // Method: rest the reader of an Overview drawn from a View, let the reader take the rest,
    // step the dashboard, and compare the reader's cancellation epoch before and after.
    let mut window = VisibilityHarness::open_resting(cx);
    assert_eq!(window.reader.receive(), (Received::Continued, false));
    let epoch = window.reader.shared.cancellation.epoch();

    window.tick();

    assert_eq!(window.reader.shared.cancellation.epoch(), epoch);
}

#[gpui::test]
fn a_rested_overview_leaves_its_reader_idle(cx: &mut TestAppContext) {
    // Goal: an Overview drawn from the store's View rests its reader without following the
    // recording, since the View brings each second the page shows.
    // Method: rest the reader of an Overview drawn from a View, let the reader take the rest,
    // and step it as the worker steps a resting reader, over a recording it has not opened.
    let mut window = VisibilityHarness::open_resting(cx);
    let recording = SealedRecording::new();
    assert_eq!(window.reader.receive(), (Received::Continued, false));

    window.reader.step_resting(&recording.root);

    assert!(window.reader.reader.session.is_none());
}

#[gpui::test]
fn a_hidden_overview_drawn_from_the_store_leaves_its_reader_idle(cx: &mut TestAppContext) {
    // Goal: a hidden Overview drawn from the store's View rests its reader without following
    // the recording, since the View brings the seconds the page shows when it shows again.
    // Method: hide an Overview drawn from a View, let the reader take the rest, and step it as
    // the worker steps a resting reader, over a recording it has not opened.
    let mut window = VisibilityHarness::open_resting(cx);
    let recording = SealedRecording::new();
    window.change(WindowVisibility::Hidden);
    assert_eq!(window.reader.receive(), (Received::Continued, false));

    window.reader.step_resting(&recording.root);

    assert!(window.reader.reader.session.is_none());
}

#[gpui::test]
fn a_hidden_page_drawn_from_its_readings_leaves_its_reader_idle(cx: &mut TestAppContext) {
    // Goal: hiding a page stops background ingestion even when saved minutes give its readings.
    // Method: hide a page without a store View and step the resting reader over unopened input.
    let mut window = VisibilityHarness::open(cx);
    let recording = SealedRecording::new();
    window.change(WindowVisibility::Hidden);
    assert_eq!(window.reader.receive(), (Received::Continued, false));

    window.reader.step_resting(&recording.root);

    assert!(window.reader.reader.session.is_none());
}

#[gpui::test]
fn an_overview_shown_again_over_a_view_leaves_its_reader_resting(cx: &mut TestAppContext) {
    // Goal: an Overview shown again draws the store's newest View and asks its reader for no
    // page, since the View holds each second the page shows.
    // Method: hide an Overview drawn from a View, give it a newer View as it shows again, step
    // its dashboard, and step the reader's mailbox.
    let mut window = VisibilityHarness::open_resting(cx);
    window.change(WindowVisibility::Hidden);
    assert_eq!(window.reader.receive(), (Received::Continued, false));
    window.report(WindowVisibility::Visible);
    window.take_view(&[reading_at(10_001, 0), reading_at(10_002, 0)]);

    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

#[gpui::test]
fn an_overview_shown_again_draws_its_installed_stores_view_without_a_second_reader(
    cx: &mut TestAppContext,
) {
    // Goal: an Overview shown again, whose installed store hands it a View before its next step,
    // draws that View and rests its reader, so the page reads its range once, through the store.
    // Method: answer the window's first request, install a real store, hide the window until the
    // store parks, show it, wait until the store hands over a View, step the dashboard and step
    // the reader's mailbox.
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = recording.start_store();
    let mut window = VisibilityHarness::open(cx);
    window.answer_readings(&[reading_at(999, 0), reading_at(1000, 0)]);
    window.tick();
    window
        .cx
        .update(|cx| crate::live_dashboard::install_store(store, cx));
    window.hide_until_the_store_parks();
    let generation = window.generation();
    window.show_until_the_store_hands_over_a_view();

    window.tick();

    assert!(window.reader_rested());
    assert_eq!(window.generation(), generation);
    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

#[gpui::test]
fn reopening_a_completed_live_overview_waits_for_the_store_without_a_second_reader(
    cx: &mut TestAppContext,
) {
    // Goal: closing and reopening uses the same single-reader restore as showing a hidden page.
    // Method: transfer a completed page's actual retained state and idle reader into a new page.
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = recording.start_store();
    let mut window = VisibilityHarness::open(cx);
    window.answer_readings(&[reading_at(999, 0), reading_at(1000, 0)]);
    window.tick();
    window
        .cx
        .update(|cx| crate::live_dashboard::install_store(store, cx));
    let (worker, state) = window
        .window
        .update(&mut window.cx, |page, _, _| {
            let state = page.window_state();
            (page.worker.hand_over().unwrap().unwrap(), state)
        })
        .unwrap();
    let reopened = window
        .cx
        .update(|cx| cx.new(|cx| LiveDashboard::restore(worker, state, cx)));

    assert!(!window.cx.update(|cx| reopened.read(cx).reply_pending));
    assert!(window.cx.update(|cx| reopened.read(cx).awaiting_reply()));
    window.report(WindowVisibility::Hidden);
    window
        .window
        .update(&mut window.cx, |_, native, cx| {
            crate::live_dashboard::observe_visibility(&reopened, native, cx);
        })
        .unwrap();
    window.report(WindowVisibility::Visible);
    assert!(window.cx.update(|cx| reopened.read(cx).awaiting_reply()));
    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

#[gpui::test]
fn an_overview_opened_again_and_shown_again_draws_its_installed_stores_view_without_a_reader(
    cx: &mut TestAppContext,
) {
    // Goal: an Overview opened again, whose window macOS hides and shows again and whose
    // installed store hands it a View before its next step, draws that View and rests its reader,
    // since the View holds every chart, readout and top App the page draws. Method: answer the
    // window's first request, install a real store, open the CPU page and Overview again, hide
    // the window until the store parks, show it, wait until the store hands over a View, step
    // the dashboard and step the reader's mailbox.
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = recording.start_store();
    let mut window = VisibilityHarness::open(cx);
    window.answer_readings(&[reading_at(999, 0), reading_at(1000, 0)]);
    window.tick();
    window
        .cx
        .update(|cx| crate::live_dashboard::install_store(store, cx));
    window.open_overview_again();
    window.hide_until_the_store_parks();
    let generation = window.generation();
    window.show_until_the_store_hands_over_a_view();

    window.tick();

    assert!(window.reader_rested());
    assert_eq!(window.generation(), generation);
    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

#[gpui::test]
fn an_overview_opened_again_and_reopened_over_its_installed_store_reads_its_page(
    cx: &mut TestAppContext,
) {
    // Goal: an Overview opened again, whose window is closed and opened again before its next
    // reading, asks the reader for that reading rather than resume from the store, since the
    // reading it kept holds none of its charts' points and only the reader brings them. Method:
    // answer the window's first request, install a real store, open the CPU page and Overview
    // again, move the page's retained state and idle reader into a new page, then whether the
    // new page waits on a reply from the reader.
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = recording.start_store();
    let mut window = VisibilityHarness::open(cx);
    window.answer_readings(&[reading_at(999, 0), reading_at(1000, 0)]);
    window.tick();
    window
        .cx
        .update(|cx| crate::live_dashboard::install_store(store, cx));
    window.open_overview_again();

    let (worker, state) = window
        .window
        .update(&mut window.cx, |page, _, _| {
            let state = page.window_state();
            (page.worker.hand_over().unwrap().unwrap(), state)
        })
        .unwrap();
    let reopened = window
        .cx
        .update(|cx| cx.new(|cx| LiveDashboard::restore(worker, state, cx)));

    assert!(window.cx.update(|cx| reopened.read(cx).reply_pending));
}

#[gpui::test]
fn an_overview_awaiting_its_app_histories_keeps_its_reader_reading(cx: &mut TestAppContext) {
    // Goal: an Overview drawn from the store's View keeps its reader reading the App histories
    // its first readings leave pending, whose read a resting reader drops.
    // Method: answer the window's first request with readings whose App histories are still to
    // come, give the window a View of newer seconds, step its dashboard, and step the reader's
    // mailbox.
    let mut window = VisibilityHarness::open(cx);
    window.answer_readings_leaving_histories(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    window.tick();
    window.take_view(&[reading_at(10_000, 0), reading_at(10_001, 0)]);

    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, true));
}

#[gpui::test]
fn an_overview_opening_before_its_first_reading_keeps_its_reader_reading(cx: &mut TestAppContext) {
    // Goal: an Overview that draws the store's View before its reader's first reading arrives,
    // as at launch, keeps its reader reading that reading.
    // Method: give the window a View before answering its first request, step its dashboard,
    // and step the reader's mailbox.
    let mut window = VisibilityHarness::open(cx);
    window.take_view(&[reading_at(10_000, 0), reading_at(10_001, 0)]);

    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, true));
}

#[gpui::test]
fn an_overview_hovering_a_second_the_store_lacks_keeps_its_reader_reading(cx: &mut TestAppContext) {
    // Goal: an Overview drawn from the store's View keeps its reader reading a hovered second
    // the View does not hold, whose read a resting reader drops.
    // Method: answer the window's first request, give the window a View of newer seconds, hover
    // a second the View does not hold, step its dashboard, and step the reader's mailbox.
    let mut window = VisibilityHarness::open(cx);
    window.answer_readings(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    window.tick();
    window.take_view(&[reading_at(10_000, 0), reading_at(10_001, 0)]);
    window.hover();

    window.tick();

    assert_eq!(window.reader.receive(), (Received::Continued, true));
}

#[gpui::test]
fn a_hover_on_a_resting_overview_asks_for_the_page_first(cx: &mut TestAppContext) {
    // Goal: a hover of a second the store's View does not hold, on an Overview whose reader
    // rests, asks the reader for the page before the second, since a resting reader reads the
    // page as newly opened.
    // Method: rest the reader of an Overview drawn from a View, hover a second the View does not
    // hold, step the dashboard, and step the reader's mailbox.
    let mut window = VisibilityHarness::open_resting(cx);
    window.hover_with_pointer();

    window.tick();

    assert_eq!(window.reader.receive(), (Received::Taken, true));
    assert_eq!(window.reader_generation(), window.generation());
}

#[gpui::test]
fn a_hover_on_a_resting_overview_waits_for_the_page_it_asked_for(cx: &mut TestAppContext) {
    // Goal: a hover that asked a rested reader for the page sends the reader neither its own
    // read nor the page again until the page arrives, since either cancels the page's read.
    // Method: rest the reader of an Overview drawn from a View, hover a second the View does not
    // hold, step the dashboard twice, and step the reader's mailbox.
    let mut window = VisibilityHarness::open_resting(cx);
    window.hover_with_pointer();
    window.tick();
    let generation = window.generation();

    window.tick();

    assert_eq!(window.generation(), generation);
    assert_eq!(window.reader.receive(), (Received::Taken, true));
    assert_eq!(window.reader_generation(), generation);
}

#[gpui::test]
fn a_hover_on_a_resting_overview_reads_its_second_once_the_page_arrives(cx: &mut TestAppContext) {
    // Goal: the page a hover asked a rested reader for lets the hover read its second once the
    // page's readings arrive.
    // Method: rest the reader of an Overview drawn from a View, hover a second the View does not
    // hold, step the dashboard, answer the page's request, step the dashboard again, and step
    // the reader's mailbox.
    let mut window = VisibilityHarness::open_resting(cx);
    window.hover_with_pointer();
    window.tick();
    window.answer_readings(&[reading_at(10_000, 0), reading_at(10_001, 0)]);

    window.tick();

    assert_eq!(window.reader.receive(), (Received::Taken, true));
    assert_eq!(Some(window.reader_generation()), window.hover_read());
}

macro_rules! page_shown_again_over_a_view_cases {
    ($($name:ident, $route:expr;)*) => {$(
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Goal: a followed page shown again draws the store's newest View and asks its reader
            // for no page, as Overview does, since the View holds each second the page shows.
            // Method: hide the page drawn from a View, give it a newer View as it shows again,
            // step its dashboard, then the second the page shows and the reader's mailbox.
            assert_page_shown_again_resumes_from_the_view(cx, $route);
        }
    )*};
}

page_shown_again_over_a_view_cases! {
    // Every row asserts the same of its page, so the rows differ only in the route.
    a_memory_page_shown_again_over_a_view_resumes_from_it_with_its_reader_resting, "memory";
    a_network_page_shown_again_over_a_view_resumes_from_it_with_its_reader_resting, "network";
    a_storage_page_shown_again_over_a_view_resumes_from_it_with_its_reader_resting, "storage";
    a_gpu_page_shown_again_over_a_view_resumes_from_it_with_its_reader_resting, "gpu";
}

/// Asserts that the page `route`, drawn from a View with its reader resting, hidden and shown
/// again over a newer View, shows that View's newest second without asking its reader for a page.
fn assert_page_shown_again_resumes_from_the_view(cx: &TestAppContext, route: &str) {
    let mut window = VisibilityHarness::open_resting_on(cx, route);
    assert!(window.reader_rested());
    window.change(WindowVisibility::Hidden);
    assert_eq!(window.reader.receive(), (Received::Continued, false));
    let generation = window.generation();
    window.report(WindowVisibility::Visible);
    window.take_view(&[reading_at(10_001, 0), reading_at(10_002, 0)]);

    window.tick();

    assert_eq!(window.shown_second(), Some(10_002));
    assert!(window.reader_rested());
    assert_eq!(window.generation(), generation);
    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

macro_rules! hover_wakes_a_resting_page_cases {
    ($($name:ident, $route:expr, $hover_reads_its_second:expr;)*) => {$(
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Goal: a hover of a second the View does not hold wakes the reader of a page that
            // rested it, as on Overview, and letting go of the hover rests the reader again once
            // the page it asks for then arrives. Method: rest the reader of the page drawn from a
            // View, hover a second the View does not hold, step the dashboard, answer the page's
            // request, step it again and step the reader's mailbox, and whether the hover read a
            // second of its own as the row says, then end the hover, step the dashboard, answer
            // the page it asked for, step it again and step the reader's mailbox.
            assert_hover_wakes_a_resting_page(cx, $route, $hover_reads_its_second);
        }
    )*};
}

hover_wakes_a_resting_page_cases! {
    // Memory: the page ranks Apps, so the hover reads its second.
    a_hover_wakes_a_resting_memory_page_s_reader_and_letting_go_rests_it_again, "memory", true;
    // Network: the hover reads no second of its own, since the page it asked for holds all
    // Network shows of that second.
    a_hover_wakes_a_resting_network_page_s_reader_and_letting_go_rests_it_again, "network",
        false;
    // Storage: the page ranks Apps, so the hover reads its second.
    a_hover_wakes_a_resting_storage_page_s_reader_and_letting_go_rests_it_again, "storage", true;
    // GPU: the page ranks Apps, so the hover reads its second.
    a_hover_wakes_a_resting_gpu_page_s_reader_and_letting_go_rests_it_again, "gpu", true;
}

/// Asserts that a hover of a second the View lacks wakes the resting reader of the page `route`,
/// reading a second of its own where `hover_reads_its_second`, and that letting go rests the
/// reader again once the page it asks for arrives.
fn assert_hover_wakes_a_resting_page(
    cx: &TestAppContext,
    route: &str,
    hover_reads_its_second: bool,
) {
    let mut window = VisibilityHarness::open_resting_on(cx, route);
    window.hover_with_pointer();
    window.tick();
    window.answer_readings(&[reading_at(10_000, 0), reading_at(10_001, 0)]);
    window.tick();
    assert!(!window.reader_rested());
    assert_eq!(window.reader.receive(), (Received::Taken, true));
    assert_eq!(
        window.hover_read(),
        hover_reads_its_second.then_some(window.reader_generation())
    );

    window.leave();
    window.tick();
    assert!(!window.hovering());
    window.answer_readings(&[reading_at(10_000, 0), reading_at(10_001, 0)]);

    window.tick();

    assert!(window.reader_rested());
    assert_eq!(window.reader.receive(), (Received::Continued, false));
}

struct VisibilityHarness {
    cx: TestAppContext,
    window: WindowHandle<LiveDashboard>,
    reader: SnapshotReader,
}

impl VisibilityHarness {
    /// Opens a live window over a reader the test steps, and lets the reader take the window's
    /// first request, as a reader that reads the page does.
    fn open(cx: &TestAppContext) -> Self {
        Self::open_on(cx, "overview")
    }

    /// Opens a live window on `route` as `open` opens Overview.
    fn open_on(cx: &TestAppContext, route: &str) -> Self {
        let mut harness = Self::open_unobserved_on(cx, route);
        harness.observe();

        harness
    }

    /// Opens a live window whose dashboard does not yet observe its visibility, and lets the
    /// reader take the window's first request.
    fn open_unobserved(cx: &TestAppContext) -> Self {
        Self::open_unobserved_on(cx, "overview")
    }

    /// Opens a live window on `route` as `open_unobserved` opens Overview.
    fn open_unobserved_on(cx: &TestAppContext, route: &str) -> Self {
        let cx = cx.clone();
        let mut reader = SnapshotReader::new();
        let worker = reader.hand_over();
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |_, cx| {
                    cx.new(|_| LiveDashboard::new(worker, route.into(), None))
                })
            })
            .unwrap();
        assert_eq!(reader.receive(), (Received::Taken, true));

        Self { cx, window, reader }
    }

    /// Opens an Overview drawn from the store's View, which rests its reader.
    fn open_resting(cx: &TestAppContext) -> Self {
        Self::open_resting_on(cx, "overview")
    }

    /// Opens the page `route` drawn from the store's View, which rests its reader.
    fn open_resting_on(cx: &TestAppContext, route: &str) -> Self {
        let mut harness = Self::open_on(cx, route);
        harness.answer_readings(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        harness.tick();
        harness.take_view(&[reading_at(10_000, 0), reading_at(10_001, 0)]);
        harness.tick();

        harness
    }

    /// Lets the window's dashboard observe whether macOS presents the window.
    fn observe(&mut self) {
        let dashboard = self.window.entity(&self.cx).unwrap();
        self.cx
            .update_window(self.window.into(), |_, window, cx| {
                crate::live_dashboard::observe_visibility(&dashboard, window, cx);
            })
            .unwrap();
    }

    /// Changes whether macOS presents the window, then steps its dashboard as its timer does.
    fn change(&mut self, visibility: WindowVisibility) {
        self.report(visibility);
        self.tick();
    }

    /// Hides the window and steps its dashboard, then waits until the installed store parks, as
    /// it does once the window stays hidden past the store's wait.
    fn hide_until_the_store_parks(&mut self) {
        self.change(WindowVisibility::Hidden);
        let deadline = Instant::now() + Duration::from_secs(5);
        for _ in 0..5_000 {
            if self.cx.update(|cx| crate::live_dashboard::store_paused(cx)) {
                return;
            }
            assert!(Instant::now() < deadline, "the store never parked");
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the store never parked");
    }

    /// Shows the window and waits until the installed store hands over a View, with no step of
    /// the dashboard between, so the View is at hand at the dashboard's next step. The test's
    /// waker takes the place of the page's, since the test steps the dashboard itself.
    fn show_until_the_store_hands_over_a_view(&mut self) {
        let handed = Arc::new(AtomicBool::new(false));
        let woken = Arc::clone(&handed);
        let waker = Box::new(move || woken.store(true, Ordering::Release));
        let installed = self
            .cx
            .update(|cx| crate::live_dashboard::wake_store_with(waker, cx));
        assert!(installed);
        self.report(WindowVisibility::Visible);
        let deadline = Instant::now() + Duration::from_secs(5);
        for _ in 0..5_000 {
            if handed.load(Ordering::Acquire) {
                return;
            }
            assert!(Instant::now() < deadline, "the store handed over no View");
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the store handed over no View");
    }

    /// Changes whether macOS presents the window, with no step of its dashboard after it.
    fn report(&mut self, visibility: WindowVisibility) {
        self.cx
            .simulate_window_visibility_change(self.window.into(), visibility);
        self.cx.run_until_parked();
    }

    /// Answers the window's last request with a page, as the reader publishes one.
    fn answer(&mut self) {
        self.publish(false);
    }

    /// Answers the window's last request with readings whose App histories a second read
    /// brings, as the reader publishes a newly opened page.
    fn answer_leaving_histories(&mut self) {
        self.publish(true);
    }

    /// Answers the window's last request with the readings of `trends`, following the newest.
    fn answer_readings(&mut self, trends: &[MachineSecond]) {
        self.publish_readings(trends, false);
    }

    /// Answers the window's last request with the readings of `trends`, following the newest,
    /// whose App histories a second read brings.
    fn answer_readings_leaving_histories(&mut self, trends: &[MachineSecond]) {
        self.publish_readings(trends, true);
    }

    fn publish_readings(&mut self, trends: &[MachineSecond], histories_pending: bool) {
        let mut page = readings(trends);
        page.generation = self.generation();
        page.histories_pending = histories_pending;
        crate::live_source::worker_publish(
            &self.reader.shared,
            page.generation,
            Update::Ready(page),
        );
    }

    /// Gives the window the store's View of the live span `seconds`, as the store hands one out,
    /// and checks that the window draws it.
    fn take_view(&mut self, seconds: &[MachineSecond]) {
        let view = Arc::new(view(1, &[], seconds));
        let drawn = self
            .window
            .update(&mut self.cx, |dashboard, _, _| dashboard.take_view(view))
            .unwrap();
        assert!(drawn);
    }

    fn publish(&mut self, histories_pending: bool) {
        let page = Box::new(Snapshot {
            generation: self.generation(),
            catalog: Value::Null,
            selection: crate::live_source::Presentation::Legacy(viewer::Selection::from_json(
                json!({}),
            )),
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
            app_ranks: None,
            app_parts: None,
            histories_pending,
            points_keeping: crate::live_source::PointsKeeping::Kept,
            app_chart_source: crate::live_source::AppChartSource::Reading,
            read_ms: 0.0,
        });
        crate::live_source::worker_publish(
            &self.reader.shared,
            page.generation,
            Update::Ready(page),
        );
    }

    /// Hovers a second the window's page does not hold from the keyboard, so the hover waits on
    /// a read of it.
    fn hover(&mut self) {
        self.hover_from(Origin::Keyboard);
    }

    /// Hovers a second the window's page does not hold with the pointer, so the hover waits on a
    /// read of it.
    fn hover_with_pointer(&mut self) {
        self.hover_from(Origin::Pointer);
    }

    fn hover_from(&mut self, origin: Origin) {
        let identity = json!({"run":"00000000000000010000000000000002",
            "boot":"00000000000000030000000000000004","publication":1});
        let target = Target::exact(1000, &identity).unwrap();
        self.window
            .update(&mut self.cx, |dashboard, _, cx| {
                dashboard.set_reading_target("visibility".into(), target, origin, cx);
            })
            .unwrap();
    }

    /// Leaves Overview for the CPU page and opens Overview again, whose next reading is still to
    /// come.
    fn open_overview_again(&mut self) {
        self.window
            .update(&mut self.cx, |dashboard, _, cx| {
                dashboard.navigate("cpu", cx);
                dashboard.navigate("overview", cx);
            })
            .unwrap();
    }

    /// Whether the window shows a hover.
    fn hovering(&self) -> bool {
        self.window
            .read_with(&self.cx, |dashboard, _| dashboard.preview.is_some())
            .unwrap()
    }

    /// Whether the window waits on its reader, which keeps its quick poll running.
    fn awaiting_reply(&self) -> bool {
        self.window
            .read_with(&self.cx, |dashboard, _| dashboard.awaiting_reply())
            .unwrap()
    }

    /// The generation of the last request the window sent its reader.
    fn generation(&self) -> u64 {
        self.window
            .read_with(&self.cx, |dashboard, _| dashboard.generation)
            .unwrap()
    }

    /// The generation of the hover's read of its second, or none before the hover sends it.
    fn hover_read(&self) -> Option<u64> {
        self.window
            .read_with(&self.cx, |dashboard, _| {
                dashboard.preview.as_ref()?.generation
            })
            .unwrap()
    }

    /// The generation of the request the reader reads.
    fn reader_generation(&self) -> u64 {
        self.reader.command.generation
    }

    /// Ends the pointer's hover `hover_with_pointer` began, as the pointer leaving the chart
    /// does. A hover from the keyboard outlasts it, as on the page.
    fn leave(&mut self) {
        self.window
            .update(&mut self.cx, |dashboard, _, cx| {
                dashboard.clear_reading_target(&"visibility".into(), cx);
            })
            .unwrap();
    }

    /// The second the window's readings show.
    fn shown_second(&self) -> Option<u64> {
        self.window
            .read_with(&self.cx, |dashboard, _| dashboard.shown_second())
            .unwrap()
    }

    /// Whether the window rested its reader.
    fn reader_rested(&self) -> bool {
        self.window
            .read_with(&self.cx, |dashboard, _| dashboard.reader_rested())
            .unwrap()
    }

    fn tick(&mut self) {
        self.window
            .update(&mut self.cx, |dashboard, _, cx| dashboard.tick(cx))
            .unwrap();
    }
}
