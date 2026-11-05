//! Native lock inheritance across the short interval before a child executes.

use super::super::WriterLock;
use super::{LOCK_MODE, create_lock};
use std::ffi::{c_int, c_void};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, PipeWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const FD_CLOEXEC: c_int = 1;
const F_GETFD: c_int = 1;
const SIGKILL: c_int = 9;
const WNOHANG: c_int = 1;
const WAIT_ATTEMPTS_MAX: usize = 200;
/// The 10 ms waits the isolated probe gets, 30 seconds, longer than `WAIT_ATTEMPTS_MAX`
/// because it starts a whole test binary, which took over 2 seconds on a machine running
/// the workspace's other tests at a load average of 50.
const ISOLATED_WAIT_ATTEMPTS_MAX: usize = 3000;
const ISOLATED_MARKER: &str = "FLIGHTDECK_NATIVE_LOCK_PROBE_ISOLATED";
const COMPLETED_SENTINEL: &str = "flightdeck-native-lock-proof completed_cases=6";
const TEST_NAME: &str = concat!(
    "ring::object::lock_tests::",
    "a_paused_preexec_child_holds_the_lock_of_a_file_closed_without_unlocking"
);
const ECHILD: i32 = 10;

unsafe extern "C" {
    fn fork() -> c_int;
    fn kill(pid: c_int, signal: c_int) -> c_int;
    fn close(file: c_int) -> c_int;
    fn fcntl(file: c_int, operation: c_int, ...) -> c_int;
    fn read(file: c_int, buffer: *mut c_void, bytes: usize) -> isize;
    fn write(file: c_int, buffer: *const c_void, bytes: usize) -> isize;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn _exit(status: c_int) -> !;
}

/// A forked child that waits on a pipe, not yet executing anything, with a copy of a file's
/// descriptor. It waits on a pipe rather than stopping itself, since macOS left about one in a
/// hundred self-stopped children stopped for good when the continue followed the stop closely.
struct PausedChild {
    pid: i32,
    release: PipeWriter,
    reaped: bool,
}

impl PausedChild {
    fn start(file: &File, close_copy: bool) -> io::Result<Self> {
        let (mut ready_reader, ready_writer) = io::pipe()?;
        let (release_reader, release) = io::pipe()?;
        let pid = unsafe { fork() };
        if pid == 0 {
            start_child(
                close_copy.then_some(file.as_raw_fd()),
                ready_writer.as_raw_fd(),
                release_reader.as_raw_fd(),
            );
        }
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        drop(ready_writer);
        drop(release_reader);
        let child = Self {
            pid,
            release,
            reaped: false,
        };
        ready_reader.read_exact(&mut [0])?;

        Ok(child)
    }

    fn wait(&mut self) -> io::Result<c_int> {
        for _attempt in 0..WAIT_ATTEMPTS_MAX {
            let mut status = 0;
            let pid = unsafe { waitpid(self.pid, &raw mut status, WNOHANG) };
            if pid == self.pid {
                self.reaped = true;
                return Ok(status);
            }
            if pid < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(crate::kernel::EINTR) {
                    continue;
                }
                if error.raw_os_error() == Some(ECHILD) {
                    self.reaped = true;
                }
                return Err(error);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::from(io::ErrorKind::TimedOut))
    }

    fn finish(mut self) -> io::Result<c_int> {
        self.release.write_all(&[0])?;
        self.wait()
    }
}

/// The child's whole life after fork, so only async-signal-safe native calls run in it.
fn start_child(descriptor_to_close: Option<c_int>, ready: c_int, release: c_int) -> ! {
    if let Some(descriptor) = descriptor_to_close {
        if unsafe { close(descriptor) } != 0 {
            unsafe { _exit(1) };
        }
    }
    let mut byte = 0_u8;
    if unsafe { write(ready, (&raw const byte).cast(), 1) } != 1 {
        unsafe { _exit(2) };
    }
    if unsafe { read(release, (&raw mut byte).cast(), 1) } != 1 {
        unsafe { _exit(3) };
    }
    unsafe { _exit(0) };
}

impl Drop for PausedChild {
    fn drop(&mut self) {
        if !self.reaped {
            if unsafe { kill(self.pid, SIGKILL) } != 0 {
                eprintln!(
                    "lock-probe cleanup kill refused: {}",
                    io::Error::last_os_error()
                );
            }
            if let Err(error) = self.wait() {
                eprintln!("lock-probe cleanup wait refused: {error}");
            }
        }
    }
}

fn open_probe(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .truncate(false)
        .append(false)
        .create_new(false)
        .mode(LOCK_MODE)
        .open(path)
        .unwrap()
}

#[test]
fn a_writers_lock_is_free_once_dropped_though_a_paused_child_holds_a_copy() {
    // Goal: a writer that has gone leaves its ring free while a child still starting holds a copy
    // of the lock's descriptor.
    // Method: fork a child that waits holding the copy, drop the writer's lock, then take the lock
    // through a fresh open while the child still waits.
    let path = std::env::temp_dir().join(format!("flightdeck-writer-lock-{}", std::process::id()));
    let lock = WriterLock(create_lock(&path).unwrap());
    let child = PausedChild::start(&lock.0, false).unwrap();

    drop(lock);

    open_probe(&path).try_lock().unwrap();
    assert_eq!(0, child.finish().unwrap());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_paused_preexec_child_holds_the_lock_of_a_file_closed_without_unlocking() {
    // Goal: identify whether pre-exec descriptor inheritance alone causes transient Busy.
    // Method: isolate by exec, then toggle only the paused child's inherited target fd.
    if std::env::var_os(ISOLATED_MARKER).as_deref() == Some(std::ffi::OsStr::new("1")) {
        run_native_probe();
    } else {
        run_isolated_probe();
    }
}

fn run_isolated_probe() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
        .env(ISOLATED_MARKER, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .unwrap();
    for _attempt in 0..ISOLATED_WAIT_ATTEMPTS_MAX {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            let mut text = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .take(4097)
                .read_to_string(&mut text)
                .unwrap();
            assert!(text.len() <= 4096);
            assert_eq!(
                1,
                text.lines()
                    .filter(|line| *line == COMPLETED_SENTINEL)
                    .count()
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(0, unsafe { kill(-child.id().cast_signed(), SIGKILL) });
    child.wait().unwrap();
    panic!("isolated native lock probe exceeded its bound");
}

fn run_native_probe() {
    let path = std::env::temp_dir().join(format!("flightdeck-native-lock-{}", std::process::id()));
    let mut completed = 0;
    for iteration in 0..3 {
        for close_copy in [false, true] {
            let lock = create_lock(&path).unwrap();
            let flags = unsafe { fcntl(lock.as_raw_fd(), F_GETFD) };
            assert!(flags >= 0);
            assert_eq!(FD_CLOEXEC, flags & FD_CLOEXEC);
            let child = PausedChild::start(&lock, close_copy).unwrap();
            let descriptor = lock.as_raw_fd();
            drop(lock);
            let probe = open_probe(&path);
            let busy = match probe.try_lock() {
                Ok(()) => false,
                Err(TryLockError::WouldBlock) => true,
                Err(TryLockError::Error(error)) => panic!("native lock probe refused: {error}"),
            };
            eprintln!(
                "lock-probe iteration={iteration} child={} fd={descriptor} \
                child_closed_copy={close_copy} parent_dropped=true busy={busy}",
                child.pid
            );
            assert_eq!(!close_copy, busy);
            drop(probe);
            assert_eq!(0, child.finish().unwrap());
            drop(create_lock(&path).unwrap());
            completed += 1;
        }
    }
    std::fs::remove_file(path).unwrap();
    assert_eq!(6, completed);
    println!("\n{COMPLETED_SENTINEL}");
}
