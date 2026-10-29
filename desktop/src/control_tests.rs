use std::sync::mpsc;

use gpui::{Entity, TestAppContext, VisualTestContext, WindowOptions};

use super::{Command, MEASUREMENTS_OFF, POLL, failure, install};
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
