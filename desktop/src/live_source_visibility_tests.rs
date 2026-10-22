use gpui::{AppContext, TestAppContext, WindowHandle, WindowOptions, WindowVisibility};

use super::SnapshotReader;
use crate::live_dashboard::LiveDashboard;
use crate::live_source::Received;

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

struct VisibilityHarness {
    cx: TestAppContext,
    window: WindowHandle<LiveDashboard>,
    reader: SnapshotReader,
}

impl VisibilityHarness {
    /// Opens a live window over a reader the test steps, and lets the reader take the window's
    /// first request, as a reader that reads the page does.
    fn open(cx: &TestAppContext) -> Self {
        let cx = cx.clone();
        let mut reader = SnapshotReader::new();
        let worker = reader.hand_over();
        let window = cx
            .update(|cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    let dashboard = cx.new(|_| LiveDashboard::new(worker, "overview".into(), None));
                    crate::live_dashboard::observe_visibility(&dashboard, window, cx);
                    dashboard
                })
            })
            .unwrap();
        assert_eq!(reader.receive(), (Received::Taken, true));

        Self { cx, window, reader }
    }

    /// Changes whether macOS presents the window, then steps its dashboard as its timer does.
    fn change(&mut self, visibility: WindowVisibility) {
        self.report(visibility);
        self.tick();
    }

    /// Changes whether macOS presents the window, with no step of its dashboard after it.
    fn report(&mut self, visibility: WindowVisibility) {
        self.cx
            .simulate_window_visibility_change(self.window.into(), visibility);
        self.cx.run_until_parked();
    }

    /// The generation of the last request the window sent its reader.
    fn generation(&self) -> u64 {
        self.window
            .read_with(&self.cx, |dashboard, _| dashboard.generation)
            .unwrap()
    }

    fn tick(&mut self) {
        self.window
            .update(&mut self.cx, |dashboard, _, cx| dashboard.tick(cx))
            .unwrap();
    }
}
