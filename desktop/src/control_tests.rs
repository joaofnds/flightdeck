use std::sync::mpsc;

use gpui::{Entity, TestAppContext, VisualTestContext, WindowOptions};

use super::{Command, HOLD_UNCHARTED, MEASUREMENTS_OFF, POLL, failure, install};
use crate::live_dashboard::LiveDashboard;
use crate::live_source::Worker;

macro_rules! measured_command_case {
    ($name:ident, $line:expr) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Goal: a measured command sent before profile-resume answers at once that
            // measurements are off, rather than after waiting for an epoch that never begins.
            // Method: open a controlled window, send the command without profile-resume, and
            // read the reply once the window has taken the command.
            let mut harness = ControlHarness::setup(cx);

            let reply = harness.driver.send($line);

            assert_eq!(reply, Some(failure(MEASUREMENTS_OFF)));
            harness.teardown();
        }
    };
}

measured_command_case!(
    profile_cycle_answers_that_measurements_are_off,
    "profile-cycle"
);
measured_command_case!(
    profile_navigate_answers_that_measurements_are_off,
    "profile-navigate cpu"
);
measured_command_case!(
    profile_range_answers_that_measurements_are_off,
    "profile-range 15"
);
measured_command_case!(
    profile_hold_answers_that_measurements_are_off,
    "profile-hold 100"
);

#[gpui::test]
fn a_measured_command_after_profile_resume_waits_for_its_frame(cx: &mut TestAppContext) {
    // Goal: profile-resume lets a measured command through to its epoch, which waits for a
    // frame. Method: send profile-resume, then profile-range, and read the reply once the window
    // has taken the command, before any frame is drawn.
    let mut harness = ControlHarness::setup(cx);
    harness.driver.send("profile-resume");

    let reply = harness.driver.send("profile-range 15");

    assert_eq!(reply, None);
    harness.teardown();
}

#[gpui::test]
fn profile_hold_refuses_a_second_past_the_last_second_the_charts_draw(cx: &mut TestAppContext) {
    // Goal: profile-hold times only a second a click on a chart could pick, since a hold past
    // the charts' window is one no person makes. Method: draw the live Mac's readings up to
    // second 10,000, then send profile-hold for the second after it.
    let mut harness = ControlHarness::setup(cx);
    harness.driver.send("profile-resume");
    harness.driver.draw_readings_to(10_000);

    let reply = harness.driver.send("profile-hold 10001");

    assert_eq!(
        reply,
        Some(failure(
            "second 10001 lies past the charts' last second 10000"
        ))
    );
    harness.teardown();
}

#[gpui::test]
fn profile_hold_refuses_a_page_that_draws_no_charts_of_the_live_mac(cx: &mut TestAppContext) {
    // Goal: profile-hold answers at once where no chart of the live Mac is drawn to click on,
    // rather than timing a selection of a saved second. Method: send profile-hold to a window
    // whose reader has read nothing.
    let mut harness = ControlHarness::setup(cx);
    harness.driver.send("profile-resume");

    let reply = harness.driver.send("profile-hold 100");

    assert_eq!(reply, Some(failure(HOLD_UNCHARTED)));
    harness.teardown();
}

struct ControlHarness {
    driver: ControlDriver,
}

impl ControlHarness {
    /// Opens a window on Overview that takes its commands from the driver, as a window
    /// launched with a control socket takes them from the socket.
    fn setup(cx: &mut TestAppContext) -> Self {
        let root = std::env::temp_dir().join(format!("flightdeck-control-{}", std::process::id()));
        let worker = Worker::start(root).unwrap();
        let (commands, receiver) = mpsc::channel();
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let dashboard =
                    crate::native_workflows::create_dashboard(worker, "overview".into(), None, cx);
                install(receiver, dashboard.clone(), window, cx);
                dashboard
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let context = VisualTestContext::from_window(window.into(), cx);
        Self {
            driver: ControlDriver {
                commands,
                view,
                context,
            },
        }
    }

    fn teardown(self) {
        drop(self);
    }
}

impl Drop for ControlHarness {
    fn drop(&mut self) {
        crate::live_dashboard::input_tests::close_dashboard_window(
            &mut self.driver.context,
            &self.driver.view,
        );
    }
}

struct ControlDriver {
    commands: mpsc::Sender<Command>,
    view: Entity<LiveDashboard>,
    context: VisualTestContext,
}

impl ControlDriver {
    /// Hands the window the live Mac's readings of the two seconds up to `last_second`, as the
    /// reader answers the page.
    fn draw_readings_to(&mut self, last_second: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.accept(crate::live_store_page_fixtures::readings(&[
                crate::live_store_fixtures::reading_at(last_second - 1, 0),
                crate::live_store_fixtures::reading_at(last_second, 0),
            ]));
            assert!(dashboard.error.is_none());
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Sends `line` as the socket does, then answers the window's reply once the window has
    /// taken the command, or none while the command still waits.
    fn send(&mut self, line: &str) -> Option<String> {
        let (reply, replies) = mpsc::channel();
        self.commands
            .send(Command {
                line: line.to_owned(),
                reply,
            })
            .unwrap();

        self.context.executor().advance_clock(POLL);
        self.context.run_until_parked();
        match replies.try_recv() {
            Ok(answer) => Some(answer),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => panic!("the window dropped the command"),
        }
    }
}
