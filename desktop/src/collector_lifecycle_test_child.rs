use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use collector::recording::control::Control;
use collector::recording::{identity_text, random_identity};

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

/// A real child owned by an explicit session with no control file, of the user's own window on
/// the default recording where `own_default_recording` says so.
pub(crate) fn session(own_default_recording: bool) -> (CollectorSession, PathBuf, u32) {
    let token = identity_text(random_identity().unwrap());
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
        own_default_recording,
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

pub(crate) fn observation(root: &std::path::Path) -> String {
    let path = root.join("observation");
    wait_ready(&path);
    std::fs::read_to_string(path).unwrap()
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

/// A recording with a control file and no run, whose lifetime locks a test holds itself.
pub(crate) struct Controlled {
    pub root: PathBuf,
}

impl Controlled {
    pub fn new() -> Self {
        let token = identity_text(random_identity().unwrap());
        let root = std::env::temp_dir().join(format!("flightdeck-controlled-{token}"));
        collector::recording::private::create_private_dir(&root).unwrap();
        drop(Control::create(&root).unwrap());
        Self { root }
    }

    /// Writes `value` at the control file's word `index`, as another build or layout would.
    pub fn change_word(&self, index: u64, value: u64) {
        use std::os::unix::fs::FileExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(self.root.join("recorder.control"))
            .unwrap();
        file.write_all_at(&value.to_ne_bytes(), index * 8).unwrap();
    }

    /// Writes `build` as the build that wrote the control file, as another build would.
    pub fn stamp_build(&self, build: u64) {
        self.change_word(6, build);
    }

    /// An automatic session on the recording, of the user's own window on the default recording
    /// where `own_default_recording` says so.
    pub fn session(&self, own_default_recording: bool) -> CollectorSession {
        CollectorSession::new(Configuration {
            root: self.root.clone(),
            binary: std::env::current_exe().unwrap(),
            isolated: true,
            mode: Mode::Auto,
            own_default_recording,
        })
    }

    pub fn hold(&self, lock: &str) -> collector::recording::private::FileLock {
        collector::recording::private::FileLock::try_lock(&self.root.join(lock)).unwrap()
    }
}

impl Drop for Controlled {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
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
    if !run_helper_wait(|| release.exists()) {
        std::process::exit(19);
    }
    let observed = if TERMINATED.load(Ordering::Relaxed) {
        "stopping"
    } else {
        "running"
    };
    let pending = release.with_file_name("observation.pending");
    std::fs::write(&pending, observed).unwrap();
    std::fs::rename(pending, release.with_file_name("observation")).unwrap();
    if !run_helper_wait(|| TERMINATED.load(Ordering::Relaxed)) {
        std::process::exit(18);
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
