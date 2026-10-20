use std::path::PathBuf;

use gpui::{App, AppContext, TestAppContext};

use super::*;
use crate::live_dashboard::LiveDashboard;

#[gpui::test]
fn closing_the_last_window_keeps_the_owned_collector_running(cx: &mut TestAppContext) {
    // Goal: a closed window leaves the recording going, so no reading is missed meanwhile.
    // Method: own a real collector child, open the live window, close it, and compare the
    // collector's status before and after.
    let mut harness = BackgroundHarness::setup(cx);
    harness.show();
    let status_open = harness.collector_status();

    harness.close_windows();

    assert_eq!(0, harness.window_count());
    assert_eq!(status_open, harness.collector_status());
    harness.teardown();
}

#[gpui::test]
fn showing_after_the_window_closed_opens_a_live_window(cx: &mut TestAppContext) {
    // Goal: the menu bar brings back the window over the readings collected while it was shut.
    // Method: close the live window, show the app again, and read the window it opened.
    let mut harness = BackgroundHarness::setup(cx);
    harness.show();
    harness.close_windows();

    harness.show();

    assert_eq!(1, harness.window_count());
    assert!(harness.shows_live_dashboard());
    harness.teardown();
}

#[gpui::test]
fn showing_with_the_window_open_opens_no_second_window(cx: &mut TestAppContext) {
    // Goal: the menu bar brings the open window forward rather than stacking another.
    // Method: show the app twice without closing its window, and count the windows.
    let mut harness = BackgroundHarness::setup(cx);
    harness.show();

    harness.show();

    assert_eq!(1, harness.window_count());
    harness.teardown();
}

struct BackgroundHarness {
    cx: TestAppContext,
    root: PathBuf,
}

impl BackgroundHarness {
    fn setup(cx: &TestAppContext) -> Self {
        let cx = cx.clone();
        let (session, root, _pid) = super::super::lifecycle::test_child::session();
        let reopening = Reopening {
            root: root.clone(),
            page: "overview".into(),
            hardware: false,
        };
        cx.update(|cx| {
            super::super::collection::install(session, cx);
            install(reopening, cx);
        });
        Self { cx, root }
    }

    fn show(&mut self) {
        self.cx.update(show);
        self.cx.run_until_parked();
    }

    fn close_windows(&mut self) {
        for window in self.cx.read(App::windows) {
            self.cx
                .update_window(window, |_, window, _| window.remove_window())
                .unwrap();
        }
        self.cx.run_until_parked();
    }

    fn collector_status(&self) -> Option<(String, bool)> {
        self.cx.read(super::super::collector_status)
    }

    fn window_count(&self) -> usize {
        self.cx.read(|cx| cx.windows().len())
    }

    fn shows_live_dashboard(&self) -> bool {
        self.cx.read(|cx| {
            cx.windows()
                .iter()
                .all(|window| window.downcast::<LiveDashboard>().is_some())
        })
    }

    fn teardown(mut self) {
        self.close_windows();
        self.cx.update(App::shutdown);
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
