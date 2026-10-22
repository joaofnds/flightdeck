use std::path::PathBuf;

use gpui::{App, AppContext, Pixels, Size, TestAppContext, px, size};

use super::*;
use crate::live_dashboard::LiveDashboard;
use crate::live_source_mode::SourceMode;
use crate::time_range::Range;

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
fn showing_after_the_window_closed_reads_through_the_reader_the_closed_window_kept(
    cx: &mut TestAppContext,
) {
    // Goal: the window opened again shows its readings at once, since it reads through the
    // reader the closed window had open rather than a new one that replays the recording.
    // Method: show and close the live window, show it again, and compare the threads the two
    // windows' readers run on.
    let mut harness = BackgroundHarness::setup(cx);
    harness.show();
    let closed = harness.reader(Worker::thread_id);
    harness.close_windows();

    harness.show();

    assert_eq!(closed, harness.reader(Worker::thread_id));
    harness.teardown();
}

#[gpui::test]
fn showing_after_a_window_on_a_saved_recording_closed_follows_the_live_recording(
    cx: &mut TestAppContext,
) {
    // Goal: a recording the user opened read-only in the window never takes the place of the
    // live readings the menu bar brings back.
    // Method: give the live window a saved recording's reader, close it, show the app again,
    // and read the source of the window it opened.
    let mut harness = BackgroundHarness::setup(cx);
    harness.show();
    harness.read_saved_recording();
    harness.close_windows();

    harness.show();

    assert_eq!(SourceMode::Live, harness.reader(Worker::mode));
    harness.teardown();
}

#[gpui::test]
fn showing_after_the_kept_reader_stopped_opens_the_window_over_a_new_reader(
    cx: &mut TestAppContext,
) {
    // Goal: a reader that stopped while the window was shut never leaves the window opened
    // again without readings.
    // Method: close the live window, stop the reader it kept, show the app again, and ask
    // whether the new window's reader runs.
    let mut harness = BackgroundHarness::setup(cx);
    harness.show();
    harness.close_windows();
    harness.stop_kept_reader();

    harness.show();

    assert!(harness.reader(Worker::running));
    harness.teardown();
}

#[gpui::test]
fn quitting_with_the_window_open_keeps_no_reader(cx: &mut TestAppContext) {
    // Goal: quitting stops the open window's reader rather than leaving it reading until the
    // process exits.
    // Method: show the live window, quit the app, and look for a reader the app kept.
    let mut harness = BackgroundHarness::setup(cx);
    harness.show();

    harness.quit();

    assert!(!harness.keeps_reader());
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

#[gpui::test]
fn showing_after_the_window_closed_opens_it_as_the_user_left_it(cx: &mut TestAppContext) {
    // Goal: closing the window keeps its size, the range picked and the series hidden.
    // Method: resize the window, pick a range and hide a series, close the window, show the
    // app again, and read the window it opened.
    let mut harness = BackgroundHarness::setup(cx);
    harness.keep_preferences();
    harness.show();
    harness.adjust_window();
    harness.close_windows();

    harness.show();

    assert_eq!(size(px(1000.0), px(700.0)), harness.window_size());
    assert_eq!(
        Range::Last(300),
        harness.dashboard(|dashboard| dashboard.range)
    );
    assert_eq!(
        vec!["cpu\tSafari".to_owned()],
        harness.dashboard(|dashboard| dashboard.hidden.clone())
    );
    harness.teardown();
}

#[gpui::test]
fn a_launch_opens_the_window_as_the_last_run_left_it(cx: &mut TestAppContext) {
    // Goal: the size, range and hidden series outlast quitting and launching again.
    // Method: resize the window, pick a range and hide a series, close the window, read the
    // file again as a launch does, show the app, and read the window it opened.
    let mut harness = BackgroundHarness::setup(cx);
    harness.keep_preferences();
    harness.show();
    harness.adjust_window();
    harness.close_windows();

    harness.keep_preferences();
    harness.show();

    assert_eq!(size(px(1000.0), px(700.0)), harness.window_size());
    assert_eq!(
        Range::Last(300),
        harness.dashboard(|dashboard| dashboard.range)
    );
    assert_eq!(
        vec!["cpu\tSafari".to_owned()],
        harness.dashboard(|dashboard| dashboard.hidden.clone())
    );
    harness.teardown();
}

#[gpui::test]
fn closing_the_window_saves_what_the_user_set_in_it(cx: &mut TestAppContext) {
    // Goal: the file keeps its field names, since a renamed field loses what an older build
    // saved.
    // Method: resize the window, pick a range and hide a series, close the window, and read
    // the file.
    let mut harness = BackgroundHarness::setup(cx);
    harness.keep_preferences();
    harness.show();
    harness.adjust_window();

    harness.close_windows();

    let saved = harness.saved_preferences();
    assert!(saved["window"]["display"].is_string());
    assert!(saved["window"]["x"].is_number());
    assert!(saved["window"]["y"].is_number());
    assert_eq!(serde_json::json!(1000.0), saved["window"]["width"]);
    assert_eq!(serde_json::json!(700.0), saved["window"]["height"]);
    assert_eq!("windowed", saved["window"]["state"]);
    assert_eq!(5, saved["range_minutes"]);
    assert_eq!("cpu\tSafari", saved["hidden_series"][0]);
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

    fn keep_preferences(&mut self) {
        let path = self.preferences_path();
        self.cx
            .update(|cx| super::super::preferences::install(path, cx));
    }

    fn preferences_path(&self) -> PathBuf {
        self.root.join("preferences.json")
    }

    fn saved_preferences(&self) -> serde_json::Value {
        let text = std::fs::read(self.preferences_path()).unwrap();
        serde_json::from_slice(&text).unwrap()
    }

    /// Resizes the open window to 1000 by 700 points, sets the range the menu's last five
    /// minutes set, and hides Safari's CPU series as its legend does.
    fn adjust_window(&mut self) {
        let window = self.cx.read(|cx| cx.windows()[0]);
        self.cx
            .simulate_window_resize(window, size(px(1000.0), px(700.0)));
        self.cx
            .update_window(window, |view, _, cx| {
                let dashboard = view.downcast::<LiveDashboard>().unwrap();
                dashboard.update(cx, |dashboard, cx| {
                    dashboard.set_range(Range::Last(300), cx);
                    dashboard.hidden.push("cpu\tSafari".into());
                });
            })
            .unwrap();
        self.cx.run_until_parked();
    }

    fn window_size(&mut self) -> Size<Pixels> {
        let window = self.cx.read(|cx| cx.windows()[0]);
        self.cx
            .update_window(window, |_, window, _| window.bounds().size)
            .unwrap()
    }

    /// Reads `read` off the open window's page.
    fn dashboard<R>(&self, read: impl FnOnce(&LiveDashboard) -> R) -> R {
        self.cx.read(|cx| {
            let window = cx.windows()[0].downcast::<LiveDashboard>().unwrap();
            read(window.read(cx).unwrap())
        })
    }

    /// Reads `read` off the open window's reader.
    fn reader<R>(&self, read: impl FnOnce(&Worker) -> R) -> R {
        self.cx.read(|cx| {
            let window = cx.windows()[0].downcast::<LiveDashboard>().unwrap();
            read(&window.read(cx).unwrap().worker)
        })
    }

    fn keeps_reader(&self) -> bool {
        self.cx.read(|cx| cx.global::<Background>().kept.is_some())
    }

    fn quit(&mut self) {
        self.cx.update(App::shutdown);
    }

    fn stop_kept_reader(&mut self) {
        self.cx.update(|cx| {
            let kept = cx.global_mut::<Background>().kept.as_mut().unwrap();
            kept.stop();
        });
    }

    /// Gives the open window a reader of the recording opened read-only, as Open Recording does.
    fn read_saved_recording(&mut self) {
        let worker = Worker::start_recorded(self.root.clone()).unwrap();
        let window = self.cx.read(|cx| cx.windows()[0]);
        self.cx
            .update_window(window, |view, _, cx| {
                let dashboard = view.downcast::<LiveDashboard>().unwrap();
                dashboard.update(cx, |dashboard, _| dashboard.worker = worker);
            })
            .unwrap();
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
        self.quit();
        super::super::lifecycle::test_child::release(&self.root);
        self.cx
            .update(super::super::collection::stop_and_wait)
            .unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
