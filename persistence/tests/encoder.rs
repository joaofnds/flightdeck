#[cfg(test)]
#[path = "encoder_fixture/mod.rs"]
mod fixture;

#[cfg(test)]
mod tests {
    use super::fixture;

    use collector::processes::Unreadable;
    use collector::tracker::{Event, Noticed};
    use persistence::resources::{Footprint, WORKING_BYTES_MAX, Watchdog};
    use persistence::{Encoder, Limits, Observation, Window};
    use std::io::Write;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    struct Directory(PathBuf);
    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    impl Directory {
        fn new() -> Self {
            let unique = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir()
                .join(format!("task19-encoder-{}-{unique}", std::process::id()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            Self(directory)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn preserves_typed_values_and_unknown_availability() {
        // Goal: each source remains queryable with measured zero distinct from absence.
        // Method: encode a bounded complete fixture then independently query typed fields.
        let directory = Directory::new();
        let window = unavailable_window();

        let manifest = Encoder.encode(&window, &directory.0).unwrap();

        assert_eq!(manifest.len(), 10);
        assert_eq!(
            count(
                &directory,
                "ticks",
                "wall_ns IS NULL AND gpu_energy_nj IS NULL \
            AND power_watts IS NULL AND soc_celsius IS NULL AND dropped_starts=11 \
            AND dropped_execs=12 AND dropped_exits=13"
            ),
            1
        );
        assert_eq!(
            count(
                &directory,
                "processes",
                "pid=0 AND user_ns=0 AND system_ns=3 \
            AND energy_nj=4 AND instructions=5 AND super_instructions=6 AND cycles=7 \
            AND idle_wakeups=8 AND interrupt_wakeups=9 AND pageins=10 AND resident_bytes=11 \
            AND footprint_bytes=12 AND disk_read_bytes=13 AND disk_written_bytes=14 \
            AND threads=-1 AND faults=2 AND context_switches=3 AND mach_syscalls=2147483647 \
            AND unix_syscalls=-2147483648"
            ),
            1
        );
        assert_eq!(
            count(
                &directory,
                "events",
                "pid=42 AND availability=1 \
            AND user_ns IS NULL AND awake_ns_at_start IS NULL AND noticed=1"
            ),
            1
        );
        assert_eq!(
            count(
                &directory,
                "events",
                "pid=43 AND availability=0 AND user_ns=52"
            ),
            1
        );
        assert_eq!(
            count(
                &directory,
                "events",
                "pid=41 AND coalition IS NULL \
            AND awake_ns_at_start IS NULL AND kind=1"
            ),
            1
        );
        assert_eq!(
            count(&directory, "memory", "total_bytes=1 AND pressure=1"),
            1
        );
        assert_eq!(
            count(&directory, "interfaces", "interface=1 AND receive_drops=8"),
            1
        );
    }

    fn unavailable_window() -> Window {
        let shape = fixture::profile(false);
        let mut tick = fixture::tick(&shape);
        tick.run_tick = 99;
        tick.ioreport_read = false;
        tick.smc_read = false;
        tick.gpu_active_ns.clear();
        tick.fan_rpm.clear();
        tick.processes[0].usage.user_ns = 0;
        tick.events = 7..10;
        let events = [
            Event::Start {
                pid: 41,
                coalition: None,
            },
            Event::Exit {
                pid: 42,
                last: Err(Unreadable::Refused),
                noticed: Noticed::AlreadyExited,
            },
            Event::Exit {
                pid: 43,
                last: Ok(fixture::usage(50)),
                noticed: Noticed::OnExit,
            },
        ];
        let numbered: Vec<_> = events
            .iter()
            .enumerate()
            .map(|(index, event)| persistence::NumberedEvent {
                number: u64::try_from(index).unwrap() + 7,
                tick: 99,
                event,
            })
            .collect();
        let identity = fixture::window(false).identity;
        let mut window = Window::new(identity, Limits::from_shape(&shape).unwrap());
        window
            .push_tick(
                &Observation {
                    publication: 8,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                },
                &tick,
                &numbered,
            )
            .unwrap();

        window
    }

    fn count(directory: &Directory, table: &str, condition: &str) -> u64 {
        let connection = duckdb::Connection::open_in_memory().unwrap();
        let path = directory.0.join(format!("{table}.parquet"));
        connection
            .query_row(
                &format!("SELECT count(*) FROM read_parquet(?) WHERE {condition}"),
                [path.to_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn refuses_an_overfull_window_without_changing_its_staged_readings() {
        // Goal: delayed persistence cannot accumulate a second window in working memory.
        // Method: fill sixty-one publications, refuse the next, then check retained counts.
        let mut window = fixture::window(false);
        let shape = fixture::profile(false);
        let tick = fixture::tick(&shape);
        let before = window.staged_bytes();

        let refused = window.push_tick(
            &Observation {
                publication: 62,
                wall_ns: Some(1),
                continuous_ns: Some(60_000_000_000),
                awake_ns: Some(60_000_000_000),
            },
            &tick,
            &[],
        );

        assert!(matches!(
            refused,
            Err(persistence::Error::Bound("window ticks"))
        ));
        assert_eq!(window.tick_count(), 61);
        assert_eq!(window.staged_bytes(), before);
    }

    #[test]
    fn refuses_a_tick_that_outgrows_its_validated_source_capacity() {
        // Goal: a changed or corrupt source cannot grow a validated staging allocation.
        // Method: grow the process section past its bound and retain an empty window.
        let shape = fixture::profile(false);
        let mut tick = fixture::tick(&shape);
        tick.processes.push(tick.processes[0]);
        let mut window = Window::new(
            fixture::window(false).identity,
            Limits::from_shape(&shape).unwrap(),
        );
        let before = window.staged_bytes();

        let refused = window.push_tick(
            &Observation {
                publication: 1,
                wall_ns: None,
                continuous_ns: None,
                awake_ns: None,
            },
            &tick,
            &[],
        );

        assert!(matches!(
            refused,
            Err(persistence::Error::Bound("tick section rows"))
        ));
        assert_eq!(window.tick_count(), 0);
        assert_eq!(window.staged_bytes(), before);
    }

    #[test]
    fn bounds_a_fully_populated_sixty_one_tick_capacity_profile() {
        // Goal: the complete measured-capacity schema fits the proposed whole-worker budget.
        // Method: stage and encode 61 injected ticks in an isolated native-observed child.
        let output = child("full");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn releases_native_window_working_state_after_warmup() {
        // Goal: repeated encoding has bounded steady physical-footprint growth.
        // Method: observe 8 warmups then 64 complete small windows in an isolated child.
        let output = child("growth");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn rejects_a_native_megabyte_per_window_leak() {
        // Goal: the growth observation detects native allocation outside Rust GlobalAlloc.
        // Method: retain and touch 1 MiB malloc blocks after each encoded small window.
        let output = child("leak");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unstable native footprint"));
    }

    #[test]
    fn terminates_on_a_low_native_footprint_allowance() {
        // Goal: native memory refusal ends the worker without returning an encoder success.
        // Method: touch native pages above a baseline-relative limit and reap the exit.
        let output = child("watchdog");
        assert_eq!(output.status.code(), Some(74));
        assert!(String::from_utf8_lossy(&output.stderr).contains("memory limit current="));
    }

    #[test]
    fn terminates_a_blocked_operation_at_its_drain_deadline() {
        // Goal: native engine or durability hangs cannot hold the steady worker forever.
        // Method: block a guarded native call past a short configured drain deadline.
        let output = child("deadline");

        assert_eq!(output.status.code(), Some(75));
    }

    #[test]
    fn terminates_native_encoding_before_a_pending_window_can_publish() {
        // Goal: real engine allocations during encoding cannot publish beyond the guard.
        // Method: encode a full window above a small native allowance after creating pending.
        let run = child_fixture("pending-watchdog");

        assert_eq!(run.output.status.code(), Some(74));
        let stdout = String::from_utf8(run.output.stdout).unwrap();
        let pending = stdout
            .lines()
            .find_map(|line| line.strip_prefix("pending_path="))
            .map(PathBuf::from)
            .unwrap();
        assert!(pending.starts_with(&run.directory.0));
        assert!(std::fs::read_dir(pending).unwrap().take(11).any(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|value| value == "parquet")
        }));
        let store = persistence::Store::open(&run.directory.0).unwrap();
        let mut identity = fixture::window(false).identity;
        identity.run.recording = store.recording();
        assert!(store.existing(&identity).unwrap().is_none());
    }

    struct ResourceRun {
        output: Output,
        directory: Directory,
    }

    fn child(mode: &str) -> Output {
        child_fixture(mode).output
    }

    fn child_fixture(mode: &str) -> ResourceRun {
        if let Ok(selected) = std::env::var("FLIGHTDECK_ENCODER_RESOURCE_TEST") {
            assert_eq!(selected, mode);
            println!("resource_child_started={mode}");
            std::io::stdout().flush().unwrap();
            resource_child(mode);
            std::process::exit(0);
        }
        let test = match mode {
            "full" => "tests::bounds_a_fully_populated_sixty_one_tick_capacity_profile",
            "growth" => "tests::releases_native_window_working_state_after_warmup",
            "leak" => "tests::rejects_a_native_megabyte_per_window_leak",
            "watchdog" => "tests::terminates_on_a_low_native_footprint_allowance",
            "deadline" => "tests::terminates_a_blocked_operation_at_its_drain_deadline",
            "pending-watchdog" => {
                "tests::terminates_native_encoding_before_a_pending_window_can_publish"
            }
            _ => panic!("unknown resource child"),
        };
        let directory = Directory::new();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("FLIGHTDECK_ENCODER_RESOURCE_TEST", mode)
            .env("FLIGHTDECK_ENCODER_RESOURCE_ROOT", &directory.0)
            .output()
            .unwrap();
        println!("{}", String::from_utf8_lossy(&output.stdout));
        println!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == format!("resource_child_started={mode}"))
        );
        ResourceRun { output, directory }
    }

    fn resource_child(mode: &str) {
        if mode == "pending-watchdog" {
            pending_native_encode();
            panic!("native encoding watchdog did not terminate");
        }
        if mode == "deadline" {
            let guard = Watchdog::with_operation_limit(
                WORKING_BYTES_MAX,
                std::time::Duration::from_millis(100),
            )
            .unwrap();
            let operation = guard.operation().unwrap();
            assert_eq!(unsafe { usleep(500_000) }, 0);
            drop(operation);
            drop(guard);
            panic!("watchdog did not terminate blocked operation");
        }
        if mode == "watchdog" {
            assert_eq!(unsafe { atexit(unexpected_cleanup) }, 0);
            let baseline = Footprint::read().unwrap();
            let guard = Watchdog::start(baseline.lifetime_bytes_max + 4 * 1024 * 1024).unwrap();
            let native = NativeBlock::new(16 * 1024 * 1024);
            std::thread::sleep(std::time::Duration::from_secs(2));
            drop(native);
            drop(guard);
            panic!("watchdog did not terminate");
        }
        let guard = Watchdog::start(WORKING_BYTES_MAX).unwrap();
        if mode == "full" {
            full_window();
        } else {
            growth_windows(mode == "leak");
        }
        drop(guard);
    }

    fn pending_native_encode() {
        let root = PathBuf::from(std::env::var_os("FLIGHTDECK_ENCODER_RESOURCE_ROOT").unwrap());
        let store = persistence::Store::open(&root).unwrap();
        let mut window = fixture::window(true);
        window.identity.run.recording = store.recording();
        let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
            panic!("fresh resource window already committed");
        };
        println!("pending_path={}", pending.path().display());
        std::io::stdout().flush().unwrap();
        let baseline = Footprint::read().unwrap();
        let guard = Watchdog::start(baseline.lifetime_bytes_max + 32 * 1024 * 1024).unwrap();
        Encoder.encode(&window, pending.path()).unwrap();
        drop(guard);
    }

    fn full_window() {
        let directory = Directory::new();
        let start = Instant::now();
        let window = fixture::window(true);
        println!(
            "full staged_bytes={} stage_ns={}",
            window.staged_bytes(),
            start.elapsed().as_nanos()
        );
        let encoded = Encoder.encode(&window, &directory.0).unwrap();
        let footprint = Footprint::read().unwrap();
        println!(
            "full total_ns={} current_bytes={} peak_bytes={} tables={encoded:?}",
            start.elapsed().as_nanos(),
            footprint.current_bytes,
            footprint.lifetime_bytes_max
        );
        assert!(footprint.lifetime_bytes_max <= WORKING_BYTES_MAX);
        assert_eq!(count(&directory, "processes", "true"), 732_000);
        assert_eq!(count(&directory, "coalitions", "true"), 1_731_424);
        assert_eq!(count(&directory, "events", "true"), 999_424);
    }

    fn growth_windows(leak: bool) {
        let mut footprints = Vec::with_capacity(64);
        let mut leaks = Vec::with_capacity(72);
        for index in 0..72 {
            let directory = Directory::new();
            let window = fixture::window(false);
            Encoder.encode(&window, &directory.0).unwrap();
            drop(window);
            if leak {
                leaks.push(NativeBlock::new(1024 * 1024));
            }
            let footprint = Footprint::read().unwrap();
            println!(
                "window={index} current_bytes={} peak_bytes={}",
                footprint.current_bytes, footprint.lifetime_bytes_max
            );
            if index >= 8 {
                footprints.push(footprint.current_bytes);
            }
        }
        assert!(
            stable_growth(&footprints),
            "unstable native footprint: {footprints:?}"
        );
    }

    /// Evaluates the observed completed-window footprints after the warmup period.
    #[must_use]
    pub fn stable_growth(footprints: &[u64]) -> bool {
        if footprints.len() != 64 {
            return false;
        }
        let first = footprints[0];
        let Some(last) = footprints.last() else {
            return false;
        };
        if last.saturating_sub(first) > 63 * 128 * 1024 {
            return false;
        }
        let minimum = footprints.iter().min();
        let maximum = footprints.iter().max();
        match (minimum, maximum) {
            (Some(minimum), Some(maximum)) => maximum.saturating_sub(*minimum) <= 8 * 1024 * 1024,
            _ => false,
        }
    }

    struct NativeBlock(*mut std::ffi::c_void);

    impl NativeBlock {
        fn new(bytes: usize) -> Self {
            assert!(bytes <= 16 * 1024 * 1024);
            let pointer = unsafe { malloc(bytes) };
            assert!(!pointer.is_null());
            unsafe {
                pointer.cast::<u8>().write_bytes(0x7f, bytes);
            }
            Self(pointer)
        }
    }
    impl Drop for NativeBlock {
        fn drop(&mut self) {
            unsafe {
                free(self.0);
            }
        }
    }
    unsafe extern "C" {
        fn usleep(microseconds: u32) -> i32;
        fn atexit(callback: extern "C" fn()) -> i32;
        fn _exit(status: i32) -> !;
        fn malloc(bytes: usize) -> *mut std::ffi::c_void;
        fn free(pointer: *mut std::ffi::c_void);
    }

    extern "C" fn unexpected_cleanup() {
        // Immediate watchdog termination must not run C++ or C exit handlers.
        unsafe { _exit(99) }
    }
}
