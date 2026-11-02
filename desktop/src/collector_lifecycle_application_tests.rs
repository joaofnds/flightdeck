use gpui::TestAppContext;

use super::super::lifecycle::test_child;
use super::*;

#[gpui::test]
fn quitting_detaches_without_stopping_the_recorder(cx: &mut TestAppContext) {
    // Goal: recording survives an application quit until explicitly stopped.
    // Method: quit over a real child, read its termination observation at a barrier,
    // then explicitly stop and reap it through the session.
    let (session, root, pid) = test_child::session();
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

#[test]
fn dropping_the_viewer_session_leaves_its_recorder_running() {
    // Goal: destruction of the GUI lifecycle object does not stop recording.
    // Method: drop a session over an unreaped child, read its barrier observation, then clean up.
    let (session, root, pid) = test_child::session();
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
