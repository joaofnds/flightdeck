use gpui::TestAppContext;

use super::super::lifecycle::test_child;
use super::*;

#[gpui::test]
fn quitting_asks_the_owned_collector_to_stop_without_waiting_for_it(cx: &mut TestAppContext) {
    // Goal: quitting asks the owned collector to stop and returns while it still saves, since the
    // collector saves its last readings and exits on its own.
    // Method: shut down TestPlatform over a real child that exits only once asked to stop and
    // then released by the test, and watch the child without reaping it, which the session
    // alone does.
    let (session, root, pid) = test_child::session();
    cx.update(|app| install(session, app));

    cx.update(App::shutdown);

    let at_return = observe(pid, libc::WNOHANG);
    test_child::release(&root);
    let at_exit = observe(pid, 0);
    let reaped = cx.update(stop_and_wait);
    std::fs::remove_dir_all(root).unwrap();

    assert_eq!(
        Observed::Running,
        at_return,
        "quitting waited for the collector"
    );
    assert_eq!(
        Observed::Exited(0),
        at_exit,
        "the collector was not asked to stop"
    );
    reaped.unwrap();
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
