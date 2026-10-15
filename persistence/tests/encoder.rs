#[cfg(test)]
#[path = "encoder_fixture/mod.rs"]
mod fixture;

#[cfg(test)]
mod tests {
    use super::fixture;

    use collector::processes::Unreadable;
    use collector::tracker::{Event, Noticed};
    use persistence::resources::{Footprint, WORKING_BYTES_MAX, Watchdog, stable_growth};
    use persistence::{Encoder, Limits, Observation, Window};
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
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

        let manifest = Encoder::new()
            .unwrap()
            .encode(&window, &directory.0)
            .unwrap();

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
        let mut window = Window::new(identity, Limits::from_shape(&shape).unwrap()).unwrap();
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
        // Method: fill sixty publications, refuse the next, then check retained counts.
        let mut window = fixture::window(false);
        let shape = fixture::profile(false);
        let tick = fixture::tick(&shape);
        let before = window.staged_bytes();

        let refused = window.push_tick(
            &Observation {
                publication: 61,
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
        assert_eq!(window.tick_count(), 60);
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
        )
        .unwrap();

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
        assert_eq!(window.staged_bytes(), 0);
    }

    #[test]
    fn bounds_a_fully_populated_sixty_tick_capacity_profile() {
        // Goal: the complete measured-capacity schema fits the proposed whole-worker budget.
        // Method: stage and encode 60 injected ticks in an isolated native-observed child.
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

    fn child(mode: &str) -> Output {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::resource_child", "--nocapture"])
            .env("FLIGHTDECK_ENCODER_RESOURCE_TEST", mode)
            .output()
            .unwrap();
        println!("{}", String::from_utf8_lossy(&output.stdout));
        println!("{}", String::from_utf8_lossy(&output.stderr));
        output
    }

    #[test]
    fn resource_child() {
        let Ok(mode) = std::env::var("FLIGHTDECK_ENCODER_RESOURCE_TEST") else {
            return;
        };
        if mode == "watchdog" {
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

    fn full_window() {
        let directory = Directory::new();
        let start = Instant::now();
        let window = fixture::window(true);
        println!(
            "full staged_bytes={} stage_ns={}",
            window.staged_bytes(),
            start.elapsed().as_nanos()
        );
        let encoded = Encoder::new()
            .unwrap()
            .encode(&window, &directory.0)
            .unwrap();
        let footprint = Footprint::read().unwrap();
        println!(
            "full total_ns={} current_bytes={} peak_bytes={} tables={encoded:?}",
            start.elapsed().as_nanos(),
            footprint.current_bytes,
            footprint.lifetime_bytes_max
        );
        assert!(footprint.lifetime_bytes_max <= WORKING_BYTES_MAX);
        assert_eq!(count(&directory, "processes", "true"), 720_000);
        assert_eq!(count(&directory, "coalitions", "true"), 1_703_040);
        assert_eq!(count(&directory, "events", "true"), 983_040);
    }

    fn growth_windows(leak: bool) {
        let mut footprints = Vec::with_capacity(64);
        let mut leaks = Vec::with_capacity(72);
        for index in 0..72 {
            let directory = Directory::new();
            let window = fixture::window(false);
            Encoder::new()
                .unwrap()
                .encode(&window, &directory.0)
                .unwrap();
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
        fn malloc(bytes: usize) -> *mut std::ffi::c_void;
        fn free(pointer: *mut std::ffi::c_void);
    }
}
