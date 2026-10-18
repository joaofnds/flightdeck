use gpui::TestAppContext;

use super::*;

#[gpui::test]
fn installed_quit_observer_reaps_delayed_child_and_observes_failure(cx: &mut TestAppContext) {
    // Goal: the actual installed observer retains ownership beyond GPUI's 200ms future budget.
    // Method: shut down TestPlatform with a real TERM-delayed child which finally exits 17.
    let (session, root, pid) = super::super::lifecycle::test_child::session();
    cx.update(|app| install(session, app));

    cx.update(App::shutdown);

    let finished_at_return = cx.read(|app| app.global::<Collection>().session.finished());
    let failure_at_return = cx.read(|app| {
        app.global::<Collection>()
            .session
            .failure()
            .map(str::to_owned)
    });
    // Always reap the helper before asserting, even when the observer returns early.
    let cleaned = cx.update(|app| app.global_mut::<Collection>().session.stop_and_wait());
    let mut status = 0;
    let waited =
        unsafe { libc::waitpid(i32::try_from(pid).unwrap(), &raw mut status, libc::WNOHANG) };
    let wait_error = std::io::Error::last_os_error().raw_os_error();
    std::fs::remove_dir_all(root).unwrap();

    assert!(finished_at_return);
    assert!(failure_at_return.is_some());
    assert!(cleaned.is_err());
    assert_eq!(-1, waited);
    assert_eq!(Some(libc::ECHILD), wait_error);
}
