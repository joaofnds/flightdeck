use collector::recording::control::Control;
use gpui::TestAppContext;

use super::super::lifecycle::test_child;
use super::*;

#[gpui::test]
fn quitting_the_own_window_asks_its_recording_to_stop(cx: &mut TestAppContext) {
    // Goal: a Quit of the user's own window stops its recording, so no helper outlives the app.
    // Method: quit over the user's own window on a real control file, then read its stop.
    let fixture = test_child::Controlled::new();
    cx.update(|app| install(fixture.session(true), app));

    cx.update(App::shutdown);

    let written = Control::read_across_builds(&fixture.root).unwrap();
    assert!(written.stopping);
}

#[gpui::test]
fn quitting_another_window_leaves_its_recording_unasked(cx: &mut TestAppContext) {
    // Goal: a window that only observes a recording never stops it at its quit.
    // Method: quit over a window that is not the user's own on a real control file, then read
    // its stop.
    let fixture = test_child::Controlled::new();
    cx.update(|app| install(fixture.session(false), app));

    cx.update(App::shutdown);

    let written = Control::read_across_builds(&fixture.root).unwrap();
    assert!(!written.stopping);
}

#[gpui::test]
fn quitting_another_window_detaches_without_stopping_the_recorder(cx: &mut TestAppContext) {
    // Goal: the recording of a window other than the user's own on the default recording
    // survives an application quit until explicitly stopped.
    // Method: quit over a real child, read its termination observation at a barrier,
    // then explicitly stop and reap it through the session.
    let (session, root, pid) = test_child::session(false);
    cx.update(|app| install(session, app));

    cx.update(App::shutdown);

    let at_return = observe(pid, libc::WNOHANG);
    test_child::release(&root);
    let after_quit = test_child::observation(&root);
    let reaped = cx.update(stop_and_wait);
    std::fs::remove_dir_all(root).unwrap();

    assert_eq!(
        Observed::Running,
        at_return,
        "quitting waited for the collector"
    );
    assert_eq!("running", after_quit, "quitting asked the recorder to stop");
    reaped.unwrap();
}

#[gpui::test]
fn quitting_the_own_window_without_a_control_file_stops_its_recorder(cx: &mut TestAppContext) {
    // Goal: a Quit of the user's own window stops the recorder it started even where no control
    // file this build reads carries the stop, and still does not wait for the recorder.
    // Method: quit over a real child of the user's own window on a recording with no control
    // file, look at the child without reaping it, release it, then reap it here, since a child
    // exits 0 once released only after its stop arrived, and 18 ten seconds later otherwise.
    // The child's observation at its release is not read, since the stop may arrive after it.
    let (session, root, pid) = test_child::session(true);
    cx.update(|app| install(session, app));

    cx.update(App::shutdown);

    let at_return = observe(pid, libc::WNOHANG);
    test_child::release(&root);
    let exited = observe(pid, 0);
    let pid = i32::try_from(pid).unwrap();
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
    std::fs::remove_dir_all(root).unwrap();

    assert_eq!(
        Observed::Running,
        at_return,
        "quitting waited for the collector"
    );
    assert_eq!(
        Observed::Exited(0),
        exited,
        "quitting left the recorder running"
    );
}

#[test]
fn dropping_the_viewer_session_leaves_its_recorder_running() {
    // Goal: destruction of the GUI lifecycle object does not stop recording.
    // Method: drop a session over an unreaped child, read its barrier observation, then clean up.
    let (session, root, pid) = test_child::session(false);
    drop(session);
    test_child::release(&root);
    let after_drop = test_child::observation(&root);
    let pid = i32::try_from(pid).unwrap();
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(after_drop, "running");
    assert_eq!(status, 0);
}

/// What became of the child, seen without reaping it so that its session still can.
#[derive(Debug, PartialEq)]
enum Observed {
    Running,
    Exited(libc::c_int),
    Signaled(libc::c_int),
    /// The child was reaped already, which only its session does.
    Reaped,
}

/// Waits for the child to exit, or under `WNOHANG` only looks.
fn observe(pid: u32, options: libc::c_int) -> Observed {
    let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            libc::id_t::from(pid),
            &raw mut information,
            libc::WEXITED | libc::WNOWAIT | options,
        )
    };
    if waited != 0 {
        let error = std::io::Error::last_os_error();
        assert_eq!(Some(libc::ECHILD), error.raw_os_error(), "{error}");
        return Observed::Reaped;
    }

    if information.si_pid == 0 {
        return Observed::Running;
    }
    if information.si_code == libc::CLD_EXITED {
        Observed::Exited(information.si_status)
    } else {
        Observed::Signaled(information.si_status)
    }
}
