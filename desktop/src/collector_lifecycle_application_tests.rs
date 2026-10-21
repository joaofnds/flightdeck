use gpui::TestAppContext;

use super::*;

#[gpui::test]
fn quitting_leaves_the_owned_collector_saving_its_last_readings(cx: &mut TestAppContext) {
    // Goal: quitting asks the owned collector to stop and returns while it is still saving, since
    // the collector saves its last readings and exits on its own.
    // Method: shut down TestPlatform over a real child that exits 17 350 ms after SIGTERM and 18
    // after ten seconds without one, and watch the child without reaping it, which the session
    // alone does.
    let (session, root, pid) = super::super::lifecycle::test_child::session();
    cx.update(|app| install(session, app));

    cx.update(App::shutdown);

    let at_return = exit_status(pid, libc::WNOHANG);
    let at_exit = exit_status(pid, 0);
    let reaped = cx.update(|app| app.global_mut::<Collection>().session.stop_and_wait());
    std::fs::remove_dir_all(root).unwrap();

    assert_eq!(
        None, at_return,
        "the collector had exited when quitting returned"
    );
    assert_eq!(Some(17), at_exit, "the collector was not asked to stop");
    assert!(reaped.is_err());
}

/// The exit status of the child, or `None` while it runs under `WNOHANG`. The child stays
/// unreaped for its session to reap.
fn exit_status(pid: u32, options: libc::c_int) -> Option<libc::c_int> {
    let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            libc::id_t::from(pid),
            &raw mut information,
            libc::WEXITED | libc::WNOWAIT | options,
        )
    };
    assert_eq!(0, waited, "{}", std::io::Error::last_os_error());
    (information.si_pid != 0).then_some(information.si_status)
}
