#[cfg(test)]
#[path = "support/process.rs"]
mod process;

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::fs::{self, File};
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use super::process::OwnedChild;
    use collector::diagnostics::{self, Role};

    const ROLE: &str = "FLIGHTDECK_REPORTER_TEST_ROLE";
    const ROOT: &str = "FLIGHTDECK_REPORTER_TEST_ROOT";
    static COUNTING: AtomicBool = AtomicBool::new(false);
    static CALLS: AtomicU64 = AtomicU64::new(0);
    static IN_FLIGHT: AtomicU64 = AtomicU64::new(0);

    unsafe extern "C" {
        fn close(descriptor: i32) -> i32;
    }

    struct Counting;
    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            observe_allocator();
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            observe_allocator();
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            observe_allocator();
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    fn observe_allocator() {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        if COUNTING.load(Ordering::SeqCst) {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }

    #[test]
    fn observes_installed_writer_delivery_without_warm_rust_allocator_calls() {
        // Goal: producer and writer transport work allocate no Rust heap after startup.
        // Method: count all threads in an isolated process while observing actual file delivery.
        if std::env::var_os(ROLE).is_some() {
            allocation_child();
        }
        let root = root("allocation");
        let status = run_child(
            "tests::observes_installed_writer_delivery_without_warm_rust_allocator_calls",
            &root,
        );

        assert!(
            status.success(),
            "installed reporter child failed: {status}"
        );
        let error = fs::read_to_string(root.join("stderr")).unwrap();
        assert!(
            error.contains("warm_rust_allocator_calls_all_threads=0"),
            "{error}"
        );
        remove_successful_fixture(&root);
    }

    #[test]
    fn delivers_healthy_final_groups_before_normal_exit() {
        // Goal: the terminal reporter operation preserves final text on usable destinations.
        // Method: exit an installed child with a distinctive status and final records.
        if std::env::var_os(ROLE).is_some() {
            diagnostics::install(Role::Collector);
            let mut group = diagnostics::stdout_group();
            group.write_all(b"final stdout\n").unwrap();
            group.submit();
            diagnostics::error(format_args!("final stderr"));
            diagnostics::exit(7);
        }
        let root = root("normal");
        let status = run_child(
            "tests::delivers_healthy_final_groups_before_normal_exit",
            &root,
        );

        assert_eq!(Some(7), status.code());
        assert!(
            fs::read_to_string(root.join("stdout"))
                .unwrap()
                .contains("final stdout\n")
        );
        assert!(
            fs::read_to_string(root.join("stderr"))
                .unwrap()
                .contains("final stderr\n")
        );
        remove_successful_fixture(&root);
    }

    macro_rules! bounded_exit {
        ($($name:ident, $occupied:literal, $status:literal;)*) => {$(
            #[test]
            fn $name() {
                // Goal: a blocked writer or formatter cannot extend terminal draining.
                // Method: fill under supervision, then time a parent-released terminal exit.
                if std::env::var_os(ROLE).is_some() {
                    bounded_exit_child($occupied, $status);
                }
                bounded_exit_parent(concat!("tests::", stringify!($name)), $status);
            }
        )*};
    }

    bounded_exit! {
        bounds_normal_exit_with_a_full_undrained_sink, false, 19;
        bounds_normal_exit_with_occupied_diagnostic_staging, true, 23;
    }

    fn bounded_exit_child(occupied: bool, status: i32) -> ! {
        // Libtest may buffer its test-name prefix; empty it before the socket becomes full.
        std::io::stdout().flush().unwrap();
        fill_stdout();
        diagnostics::install(Role::Collector);
        let mut group = diagnostics::stdout_group();
        group
            .write_all(b"writer cannot drain this group\n")
            .unwrap();
        group.submit();
        if occupied {
            occupy_staging();
        }
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        fs::write(root.join("ready_to_exit"), b"ready to exit\n").unwrap();
        let mut release = [0];
        std::io::stdin().read_exact(&mut release).unwrap();
        assert_eq!([1], release);

        diagnostics::exit(status);
    }

    fn fill_stdout() {
        // The controller never writes into a possibly full sink. Only its owned child does.
        let mut sink = unsafe { UnixStream::from_raw_fd(1) };
        sink.set_nonblocking(true).unwrap();
        for _attempt in 0..16_384 {
            match sink.write(&[b'x'; 4_096]) {
                Ok(0) => panic!("sink unexpectedly accepted zero bytes"),
                Ok(_) => {}
                Err(error) => {
                    assert_eq!(std::io::ErrorKind::WouldBlock, error.kind());
                    sink.set_nonblocking(false).unwrap();
                    assert_eq!(1, sink.into_raw_fd());
                    return;
                }
            }
        }
        panic!("sink did not fill within its fixed write bound");
    }

    fn occupy_staging() {
        struct Occupied {
            entered: std::sync::mpsc::Sender<()>,
            release: std::sync::mpsc::Receiver<()>,
        }
        impl std::fmt::Display for Occupied {
            fn fmt(&self, _out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.entered.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            }
        }
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, receiver) = std::sync::mpsc::channel();
        let formatter = Occupied {
            entered,
            release: receiver,
        };
        let worker = std::thread::spawn(move || diagnostics::error(format_args!("{formatter}")));
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        // Process termination owns both threads; keep the release sender alive until then.
        std::mem::forget((release, worker));
    }

    fn bounded_exit_parent(test: &str, expected_status: i32) {
        let root = root(test);
        let (peer, sink) = UnixStream::pair().unwrap();
        let mut child = OwnedChild::spawn(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env(ROLE, "child")
                .env(ROOT, &root)
                .stdin(Stdio::piped())
                .stdout(Stdio::from(OwnedFd::from(sink)))
                .stderr(File::create(root.join("stderr")).unwrap()),
        );
        let mut release = child.child.stdin.take().unwrap();
        wait_exit_ready(&mut child, &root);

        let started = Instant::now();
        release.write_all(&[1]).unwrap();
        drop(release);
        let status = child.wait(Duration::from_millis(750));
        let terminal = started.elapsed();

        assert_eq!(Some(expected_status), status.code());
        // This scheduling allowance rejects a one-second drain; it is no real-time promise.
        assert!(
            terminal < Duration::from_millis(750),
            "terminal interval={terminal:?}"
        );
        eprintln!("bounded normal exit status={status} terminal_interval={terminal:?}");
        drop(peer);
        remove_successful_fixture(&root);
    }

    fn wait_exit_ready(child: &mut OwnedChild, root: &Path) {
        let start = Instant::now();
        for _attempt in 0..2_001 {
            if root.join("ready_to_exit").is_file() {
                return;
            }
            assert!(
                child.try_wait().is_none(),
                "child exited before its exit readiness witness"
            );
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "exit readiness witness was not produced"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("exit readiness exhausted its controller bound");
    }

    #[test]
    fn delivers_loss_without_an_admitted_group_before_normal_exit() {
        // Goal: a final rejected event wakes the idle writer and reaches a healthy sink.
        // Method: submit only an oversize group, then perform the terminal bounded drain.
        if std::env::var_os(ROLE).is_some() {
            diagnostics::install(Role::Collector);
            let mut group = diagnostics::stdout_group();
            group
                .write_all(&[b'x'; diagnostics::GROUP_BYTES + 1])
                .unwrap();
            group.submit();
            diagnostics::exit(0);
        }
        let root = root("loss");

        let status = run_child(
            "tests::delivers_loss_without_an_admitted_group_before_normal_exit",
            &root,
        );

        assert!(status.success());
        let output = fs::read_to_string(root.join("stdout")).unwrap();
        assert!(output.contains("oversize=1"), "{output}");
        assert!(!output.contains("xxx"), "{output}");
        remove_successful_fixture(&root);
    }

    macro_rules! closed_streams {
        ($($name:ident, $descriptor:expr, $survivor:literal, $affected:literal;)*) => {$(
            #[test]
            fn $name() {
                // Goal: a real duplication failure is retained without reusing the missing fd.
                // Method: close a stream after Rust startup and inspect the surviving output.
                if std::env::var_os(ROLE).is_some() {
                    assert_eq!(0, unsafe { close($descriptor) });
                    diagnostics::install(Role::Collector);
                    diagnostics::exit(0);
                }
                let root = root(stringify!($name));

                let status = run_child(concat!("tests::", stringify!($name)), &root);

                assert!(status.success());
                let output = fs::read_to_string(root.join($survivor)).unwrap();
                assert!(output.contains(concat!("stream=", $affected)), "{output}");
                assert!(output.contains("setup_status=9"), "{output}");
                remove_successful_fixture(&root);
            }
        )*};
    }

    closed_streams! {
        disables_stdout_when_its_duplication_fails, 1, "stderr", "stdout";
        keeps_stdout_duplication_out_of_a_missing_stderr_slot, 2, "stdout", "stderr";
    }

    #[test]
    fn starts_no_writer_when_both_streams_are_absent() {
        // Goal: no usable destination means no writer thread or deferred sink work.
        // Method: close both streams immediately before installation in an isolated child.
        if std::env::var_os(ROLE).is_some() {
            let timebase = collector::clock::Timebase::read().unwrap();
            let pid = collector::processes::own_pid().unwrap();
            let before = collector::processes::read(timebase, pid)
                .unwrap()
                .unwrap()
                .task
                .threads;
            assert_eq!(0, unsafe { close(1) });
            assert_eq!(0, unsafe { close(2) });

            diagnostics::install(Role::Collector);
            let after = collector::processes::read(timebase, pid)
                .unwrap()
                .unwrap()
                .task
                .threads;

            assert_eq!(before, after);
            diagnostics::exit(0);
        }
        let root = root("disabled");

        let status = run_child(
            "tests::starts_no_writer_when_both_streams_are_absent",
            &root,
        );

        assert!(status.success());
        remove_successful_fixture(&root);
    }

    fn allocation_child() -> ! {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        let output = root.join("stdout");
        diagnostics::install(Role::Collector);
        let mut group = diagnostics::stdout_group();
        let initial = fs::metadata(&output).unwrap().len();
        group.write_all(b"warm\n").unwrap();
        group.submit();
        wait_for_growth(&output, initial);

        COUNTING.store(true, Ordering::SeqCst);
        for index in 0..1_000 {
            let before = fs::metadata(&output).unwrap().len();
            group.write_all(b"transport\n").unwrap();
            group.submit();
            diagnostics::error(format_args!("numeric status={index}"));
            wait_for_growth(&output, before);
        }
        COUNTING.store(false, Ordering::SeqCst);
        finish_observation();
        let calls = CALLS.load(Ordering::SeqCst);

        eprintln!("warm_rust_allocator_calls_all_threads={calls}");
        assert_eq!(0, calls);
        diagnostics::exit(0);
    }

    fn finish_observation() {
        let start = Instant::now();

        for _attempt in 0..1_001 {
            if IN_FLIGHT.load(Ordering::SeqCst) == 0 {
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(1));
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("allocator observation did not become quiescent");
    }

    fn wait_for_growth(output: &Path, previous: u64) {
        let start = Instant::now();

        for _attempt in 0..1_001 {
            if fs::metadata(output).unwrap().len() > previous {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "writer delivery stalled"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("writer delivery exhausted its controller bound");
    }

    fn root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("flightdeck-reporter-{name}-{}", std::process::id()));
        collector::recording::private::create_private_dir(&root).unwrap();
        eprintln!("reporter controller root={}", root.display());

        root
    }

    fn run_child(test: &str, root: &Path) -> std::process::ExitStatus {
        let stderr = Stdio::from(File::create(root.join("stderr")).unwrap());

        run_child_with_stderr(test, root, stderr)
    }

    fn run_child_with_stderr(test: &str, root: &Path, stderr: Stdio) -> std::process::ExitStatus {
        let mut child = OwnedChild::spawn(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env(ROLE, "child")
                .env(ROOT, root)
                .stdout(File::create(root.join("stdout")).unwrap())
                .stderr(stderr),
        );

        child.wait(Duration::from_secs(10))
    }

    fn remove_successful_fixture(root: &Path) {
        if std::env::var_os("FLIGHTDECK_DIAGNOSTIC_RETAIN").is_none() {
            for entry in fs::read_dir(root).unwrap().take(4) {
                let name = entry.unwrap().file_name();
                assert!(
                    ["stdout", "stderr", "ready_to_exit"]
                        .iter()
                        .any(|allowed| name == *allowed)
                );
            }
            fs::remove_dir_all(root).unwrap();
        }
    }
}
