use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{CollectorSession, Configuration, Mode};

const HELPER: &str = "live_launch::lifecycle::test_child::delayed_shutdown_helper";
const READY: &str = "FLIGHTDECK_LIFECYCLE_TEST_READY";
const RELEASE: &str = "FLIGHTDECK_LIFECYCLE_TEST_RELEASE";
static TERMINATED: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct SignalAction {
    handler: Option<extern "C" fn(libc::c_int)>,
    mask: u32,
    flags: libc::c_int,
}

const _: () = {
    assert!(size_of::<SignalAction>() == size_of::<libc::sigaction>());
    assert!(
        std::mem::offset_of!(SignalAction, mask) == std::mem::offset_of!(libc::sigaction, sa_mask)
    );
    assert!(
        std::mem::offset_of!(SignalAction, flags)
            == std::mem::offset_of!(libc::sigaction, sa_flags)
    );
};

pub(crate) fn session() -> (CollectorSession, PathBuf, u32) {
    let token =
        collector::recording::identity_text(collector::recording::random_identity().unwrap());
    let root = std::env::temp_dir().join(format!("flightdeck-quit-helper-{token}"));
    std::fs::create_dir(&root).unwrap();
    let ready = root.join("ready");
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", HELPER, "--nocapture"])
        .env(READY, &ready)
        .env(RELEASE, root.join("release"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut session = CollectorSession::new(Configuration {
        root: root.clone(),
        binary: std::env::current_exe().unwrap(),
        isolated: true,
        mode: Mode::Explicit,
    });
    session.0.child = Some(child);
    session.0.policy.launched(pid, Duration::ZERO);
    wait_ready(&ready);
    (session, root, pid)
}

/// Lets the child under `root` exit, which it does only once asked to stop and released.
pub(crate) fn release(root: &std::path::Path) {
    std::fs::write(root.join("release"), b"release").unwrap();
}

fn wait_ready(path: &std::path::Path) {
    for _attempt in 0..400 {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("Delayed exit helper did not become ready");
}

#[test]
fn delayed_shutdown_helper() {
    // Goal: provide a real child which keeps saving after it is asked to stop until the test
    // lets it exit, so a test sees whether quitting waited for it.
    // Method: the explicitly spawned role installs a TERM handler, and after TERM exits 0 once
    // released.
    run_helper();
}

fn run_helper() {
    let Some(path) = std::env::var_os(READY) else {
        return;
    };
    let release = PathBuf::from(std::env::var_os(RELEASE).unwrap());
    let action = SignalAction {
        handler: Some(terminate),
        mask: 0,
        flags: 0,
    };
    assert_eq!(0, unsafe {
        libc::sigaction(
            libc::SIGTERM,
            std::ptr::from_ref(&action).cast(),
            std::ptr::null_mut(),
        )
    });
    std::fs::write(path, b"ready").unwrap();
    // Each wait gives up after ten seconds, with a status naming the step that never came.
    if !run_helper_wait(|| TERMINATED.load(Ordering::Relaxed)) {
        std::process::exit(18);
    }
    if !run_helper_wait(|| release.exists()) {
        std::process::exit(19);
    }
    std::process::exit(0);
}

fn run_helper_wait(condition: impl Fn() -> bool) -> bool {
    for _attempt in 0..2000 {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

extern "C" fn terminate(_signal: libc::c_int) {
    TERMINATED.store(true, Ordering::Relaxed);
}
