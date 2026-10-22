use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use collector::ring::{Place, RingReader};

const ROLE: &str = "FLIGHTDECK_RUN_TEST_ROLE";
const ROOT: &str = "FLIGHTDECK_RUN_TEST_ROOT";
const RING: &str = "FLIGHTDECK_RUN_TEST_RING";
const EXPECTED: &str = "FLIGHTDECK_RUN_TEST_PARENT";
/// How often a test looks again at the processes it waits for.
const POLL_INTERVAL: Duration = Duration::from_millis(1);
/// How long the collector runs unless its parent's departure ends it first.
const RUN_SECONDS: u32 = 8;
/// How long a test waits for a process it starts to connect, and then for the collector to
/// publish its second tick. The collector promises no limit on its start, which full-syncs the
/// recording's metadata and so waits on every other process writing to the disk, and its
/// longest measured wait for this was 3.6 seconds.
const START_LIMIT_SECONDS: u32 = 120;
/// The alarm that ends a test's own processes once they outlive both of its start waits, the
/// run and a margin for finishing it, so a failed test leaves none behind.
const WATCHDOG_SECONDS: u32 = 2 * START_LIMIT_SECONDS + 60;
const _: () = assert!(2 * START_LIMIT_SECONDS + RUN_SECONDS < WATCHDOG_SECONDS);

macro_rules! ownership_runs {
    ($($name:ident, $bound:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: actual collector run obeys only explicitly requested parent ownership.
            // Method: release its real parent after publication2 and inspect completion/ticks.
            exercise(concat!("owner_run_tests::", stringify!($name)), $bound);
        }
    )*};
}

ownership_runs! {
    finishes_the_actual_run_when_its_bound_parent_departs, true;
    keeps_the_actual_unbound_run_collecting_after_parent_departure, false;
}

fn exercise(test: &str, bound: bool) {
    match std::env::var(ROLE) {
        Ok(role) => {
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            match role.as_str() {
                "parent" => parent(test, &root),
                "collector" => collect(&root, bound),
                _ => panic!("unknown collector run test role"),
            }
            return;
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(error) => panic!("invalid collector run test role: {error}"),
    }
    let mut fixture = None;
    Fixture::start(&mut fixture, test, bound);
    let fixture = fixture.as_mut().unwrap();
    let mut reader = None;
    fixture.wait_publication(2, &mut reader);
    let reader = reader.as_ref().unwrap();
    std::fs::write(fixture.root.join("release"), b"exit").unwrap();
    fixture.wait_parent();
    let departed = Instant::now();
    let first = reader.publication_newest().unwrap();
    if !bound {
        // The run publishes nothing past its own end, so a longer wait would show nothing more.
        wait_condition(Duration::from_secs(u64::from(RUN_SECONDS)), || {
            reader.publication_newest().unwrap() >= first + 2
        });
        assert!(
            !reader.completed(),
            "unbound collection stopped with its parent"
        );
    }
    let returned_ok = fixture.finish().unwrap();
    let elapsed = departed.elapsed();

    assert!(returned_ok, "actual collector run returned an error");
    assert!(
        reader.completed(),
        "actual collector run did not orderly-complete its ring"
    );
    if bound {
        assert!(
            elapsed < Duration::from_secs(3),
            "parent departure must stop the actual run before its finite fallback: {elapsed:?}"
        );
    }
    eprintln!(
        "actual run bound={bound} parent_departure_to_exit_ns={} publications={} completed={}",
        elapsed.as_nanos(),
        reader.publication_newest().unwrap(),
        reader.completed()
    );
}

fn parent(test: &str, root: &Path) {
    assert_eq!(0, unsafe { alarm(WATCHDOG_SECONDS) });
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(ROLE, "collector")
        .env(EXPECTED, std::process::id().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    // The test releases this parent after both of its start waits.
    let limit = Duration::from_secs(u64::from(2 * START_LIMIT_SECONDS));
    let start = Instant::now();
    for _attempt in 0..attempts(limit) {
        if root.join("release").is_file() {
            // The collector is deliberately orphaned only after master observes a real tick.
            std::process::exit(0);
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("collector exited before parent release: {status}");
        }
        if start.elapsed() >= limit {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    panic!("collector parent was not released: {status}");
}

fn collect(root: &Path, bound: bool) {
    assert_eq!(0, unsafe { alarm(WATCHDOG_SECONDS) });
    let mut peer = UnixStream::connect(root.join("control")).unwrap();
    let options = super::Options {
        seconds: std::num::NonZeroU64::new(u64::from(RUN_SECONDS)),
        rescan_interval: super::DEFAULT_RESCAN_INTERVAL,
        ring_name: std::env::var(RING).unwrap(),
        recording: Some(root.join("recording")),
        parent_pid: if bound {
            Some(std::env::var(EXPECTED).unwrap().parse().unwrap())
        } else {
            None
        },
    };
    let returned_ok = match super::run(&options) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("actual ownership run failed: {error}");
            false
        }
    };
    peer.write_all(&[u8::from(returned_ok)]).unwrap();
    // EOF is emitted only by process teardown, after run returned and released its handles.
    std::process::exit(0);
}

struct Fixture {
    root: PathBuf,
    place: Place,
    parent: Option<Child>,
    peer: Option<UnixStream>,
    finished: bool,
}

impl Fixture {
    fn start(output: &mut Option<Box<Self>>, test: &str, bound: bool) {
        let suffix = if bound { "bound" } else { "unbound" };
        let ring = format!("fdownerrun{}{suffix}", std::process::id());
        let root = std::env::temp_dir().join(format!("flightdeck-owner-run-{ring}"));
        collector::recording::private::create_private_dir(&root).unwrap();
        *output = Some(Box::new(Self {
            root,
            place: Place::for_user(&ring).unwrap(),
            parent: None,
            peer: None,
            finished: false,
        }));
        let fixture = output.as_mut().unwrap();
        let listener = UnixListener::bind(fixture.root.join("control")).unwrap();
        listener.set_nonblocking(true).unwrap();
        fixture.parent = Some(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env(ROLE, "parent")
                .env(ROOT, &fixture.root)
                .env(RING, ring)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        fixture.peer = Some(accept(&listener));
    }

    fn wait_publication(&self, minimum: u64, reader: &mut Option<RingReader>) {
        wait_condition(start_limit(), || {
            match RingReader::open(&self.place.lock_path) {
                Ok(current) => {
                    if current.publication_newest().unwrap() >= minimum {
                        *reader = Some(current);
                        return true;
                    }
                }
                Err(error) => assert!(matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::WouldBlock
                )),
            }
            false
        });
    }

    fn wait_parent(&mut self) {
        let parent = self.parent.as_mut().unwrap();
        wait_condition(Duration::from_secs(2), || {
            match parent.try_wait().unwrap() {
                Some(status) => {
                    assert!(status.success());
                    true
                }
                None => false,
            }
        });
    }

    fn finish(&mut self) -> std::io::Result<bool> {
        let peer = self.peer.as_mut().unwrap();
        let mut result = [0; 1];
        peer.read_exact(&mut result)?;
        let mut extra = [0; 1];
        let extra_count = peer.read(&mut extra)?;
        if extra_count != 0 {
            return Err(std::io::Error::other(
                "unexpected extra collector result bytes",
            ));
        }
        self.finished = true;
        Ok(result == [1])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(parent) = &mut self.parent {
            if parent.try_wait().unwrap().is_none() {
                parent.kill().unwrap();
                let status = parent.wait().unwrap();
                eprintln!("owned intermediate cleanup status={status}");
            }
        }
        if !self.finished {
            if self.peer.is_some() {
                match self.finish() {
                    Ok(returned_ok) => eprintln!("collector cleanup returned_ok={returned_ok}"),
                    Err(error) => eprintln!("collector cleanup EOF unavailable: {error}"),
                }
            }
        }
        if !self.finished {
            eprintln!(
                "no collector EOF witness; retaining fixture {}",
                self.root.display()
            );
            return;
        }
        if self.place.lock_path.exists() {
            let reader = RingReader::open(&self.place.lock_path).unwrap();
            collector::ring::unlink_generation(reader.name()).unwrap();
            std::fs::remove_file(&self.place.lock_path).unwrap();
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn accept(listener: &UnixListener) -> UnixStream {
    let mut peer = None;
    wait_condition(start_limit(), || match listener.accept() {
        Ok((stream, _address)) => {
            peer = Some(stream);
            true
        }
        Err(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            false
        }
    });
    let peer = peer.unwrap();
    peer.set_nonblocking(false).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(u64::from(WATCHDOG_SECONDS) + 1)))
        .unwrap();
    peer
}

fn wait_condition(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    for _attempt in 0..attempts(timeout) {
        if condition() {
            return;
        }
        if start.elapsed() >= timeout {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!("ownership run condition exceeded {timeout:?}");
}

/// The polls a wait of `limit` takes at most, one more than fit in it.
fn attempts(limit: Duration) -> u128 {
    limit.as_millis().div_ceil(POLL_INTERVAL.as_millis()) + 1
}

fn start_limit() -> Duration {
    Duration::from_secs(u64::from(START_LIMIT_SECONDS))
}

unsafe extern "C" {
    fn alarm(seconds: u32) -> u32;
}
