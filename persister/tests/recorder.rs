//! The production recorder supervises workers without owning their data locks.
#[cfg(test)]
#[path = "event_stream/source.rs"]
mod terminal_source;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use collector::recording::control::{Control, Role, State};
    use collector::ring::{Place, unlink_generation};

    struct Recording {
        root: PathBuf,
        place: Place,
        child: Option<Child>,
        ring: String,
        seconds: u64,
        memory: u64,
        sync_fault: Option<PathBuf>,
    }

    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }

    #[test]
    fn closed_tails_are_canonical_before_the_next_acquisition_registers() {
        // Goal: missed live collector observations cannot evict recoverable registered tails.
        // Method: hold commit ownership over two closed sources, then release the real recorder.
        use collector::recording::Registry;
        let mut recording = Recording::new("closed-tails", 2);
        Control::create(&recording.root).unwrap();
        let first = recording.fixture_closed();
        let second = recording.fixture_closed();
        let before = Registry::read(&recording.root).unwrap();
        assert_eq!(before.runs.len(), 2);
        let held = persistence::Store::open(&recording.root).unwrap();
        recording.launch(PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"));
        let control = recording.wait_for(|control| {
            let health = control.read(Role::Controller).unwrap();
            health.heartbeat_ns > 0
        });
        let began = control.read(Role::Controller).unwrap().heartbeat_ns;
        recording.wait_for(|control| {
            control.read(Role::Controller).unwrap().heartbeat_ns >= began + 2_000_000_000
        });
        let blocked = Registry::read(&recording.root).unwrap();
        let unchanged = blocked
            .runs
            .iter()
            .map(|run| run.identity)
            .collect::<Vec<_>>()
            == before
                .runs
                .iter()
                .map(|run| run.identity)
                .collect::<Vec<_>>();
        drop(held);
        assert!(
            unchanged,
            "replacement registered while prior tails had no commit owner"
        );
        let root = recording.root.clone();
        recording.wait_for(|_control| {
            Registry::read(&root).unwrap().runs.last().unwrap().identity != second
        });
        let after = Registry::read(&root).unwrap();
        assert_ne!(after.runs.last().unwrap().identity, second);
        for old in &before.runs {
            assert!(Registry::completed(&root, &old.identity, &old.ring_name).unwrap());
        }
        recording.wait_for_exit();
        recording.assert_saved_run(first, 1, 1);
        recording.assert_saved_run(second, 2, 2);
        eprintln!("closed tails canonical before new acquisition: first=1..2 second=2..3 loss=0");
    }

    #[test]
    fn a_native_managed_worker_reports_its_original_registration_refusal() {
        // Goal: worker startup preserves the ownership cause through the diagnostic boundary.
        // Method: hold the recording lock and launch its actual authorized collector directly.
        use collector::recording::{identity_text, private::FileLock};
        let mut recording = Recording::new("worker-refusal", 2);
        let control = Control::create(&recording.root).unwrap();
        let lock = FileLock::try_lock(&recording.root.join("collector.lock")).unwrap();
        let expected = FileLock::try_lock(&recording.root.join("collector.lock"))
            .err()
            .expect("the held worker lock must refuse another owner");
        assert_eq!(expected.kind(), std::io::ErrorKind::WouldBlock);
        let diagnostics = recording.root.join("diagnostics");
        recording.child = Some(
            Command::new(
                PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("collector"),
            )
            .arg("--recording")
            .arg(&recording.root)
            .arg("--ring")
            .arg(&recording.ring)
            .arg("--recorder-authority")
            .arg(identity_text(control.authority()).to_string())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&diagnostics).unwrap())
            .spawn()
            .unwrap(),
        );
        let status = recording.wait_for_status();
        drop(lock);
        assert_eq!(status.code(), Some(1));
        let output = fs::read_to_string(diagnostics).unwrap();
        assert!(output.contains(&expected.to_string()), "{output}");
        assert!(!output.contains("managed recording registration failed"));
        assert!(!output.contains("collection continues"));
        assert!(!recording.root.join("runs").exists());
        eprintln!("native managed registration refusal: {output}");
    }

    #[test]
    fn a_terminal_only_replacement_drains_despite_old_stopped_health() {
        // Goal: zero-publication event history needs its own canonical receipt before stop.
        // Method: register an event-before-observation replacement and launch the real recorder.
        use collector::recording::{Registry, control::Reporter};
        let mut recording = Recording::new("terminal-stop", 2);
        Control::create(&recording.root).unwrap();
        let mut first = recording.fixture_writer();
        let previous = first.identity().unwrap();
        first.finish();
        drop(first);
        Registry::retire(&recording.root, &previous).unwrap();
        let replacement = recording.fixture_terminal();
        for role in [Role::Collector, Role::Persister] {
            let mut reporter =
                Reporter::new(Control::open_writable(&recording.root).unwrap(), role).unwrap();
            reporter.update(State::Stopped, previous.run, 0).unwrap();
        }
        recording.launch(PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"));
        recording.wait_for_exit();
        let mut buffer: Box<[_; collector::recording::discovery::DIRECTORY_BUFFER_WORDS]> =
            vec![0; collector::recording::discovery::DIRECTORY_BUFFER_WORDS]
                .into_boxed_slice()
                .try_into()
                .unwrap();
        let mut discovery =
            persistence::event_recovery::Discovery::open(&recording.root, &mut buffer).unwrap();
        let page = discovery.next_page(2).unwrap();
        assert!(page.complete);
        assert_eq!(page.runs.len(), 1);
        assert_eq!(page.runs[0].run, replacement);
        assert_eq!(page.runs[0].rows, 1);
        assert_eq!(page.runs[0].losses, persistence::Losses::default());
        let events =
            persistence::event_recovery::read_page(&recording.root, &replacement, 1, 1).unwrap();
        assert_eq!(events.coverage, 1..2);
        assert_eq!(
            events.rows.column("event").unwrap().u64().unwrap().get(0),
            Some(1)
        );
        assert_eq!(events.rows.column("publication").unwrap().null_count(), 1);
        assert_eq!(events.next, None);
        let registry = Registry::read(&recording.root).unwrap();
        assert!(
            Registry::completed(&recording.root, &replacement, &registry.runs[1].ring_name)
                .unwrap()
        );
        assert!(
            persistence::Store::open(&recording.root)
                .unwrap()
                .progress(&replacement)
                .unwrap()
                .is_none()
        );
        eprintln!(
            "native terminal replacement exact_events=1..2 publications=0 loss=0 receipt=true"
        );
    }

    #[test]
    fn replaces_a_failed_persister_without_replacing_acquisition() {
        // Goal: a worker death cannot permanently stop saving a healthy collector's tail.
        // Method: kill the owned persister, observe replacement and drain a finite real run.
        let mut recording = Recording::start("replace", 8);
        let before = recording.wait_for(|control| {
            control.read(Role::Collector).unwrap().state == State::Running
                && control.read(Role::Persister).unwrap().alive().unwrap()
        });
        let collector = before.read(Role::Collector).unwrap();
        let persister = before.read(Role::Persister).unwrap();

        assert_eq!(0, unsafe { kill(persister.pid, 9) });
        let after = recording.wait_for(|control| {
            let current = control.read(Role::Persister).unwrap();
            current.alive().unwrap() && current.creation != persister.creation
        });

        assert_eq!(
            collector.creation,
            after.read(Role::Collector).unwrap().creation
        );
        recording.wait_for_exit();
        recording.assert_saved_publications(after.read(Role::Collector).unwrap().publication);
        assert_eq!(State::Stopped, after.read(Role::Controller).unwrap().state);
        assert!(after.read(Role::Persister).unwrap().publication > 0);
    }

    #[test]
    fn controller_death_preserves_workers_and_relaunch_adopts_survivors() {
        // Goal: loss of the controller leaves workers independent; relaunch recovers supervision.
        // Method: kill it, verify advancing acquisition, then kill persistence and relaunch.
        let mut recording = Recording::start("adopt", 30);
        let before = recording.wait_for(|control| {
            control.read(Role::Collector).unwrap().state == State::Running
                && control.read(Role::Persister).unwrap().alive().unwrap()
        });
        let collector = before.read(Role::Collector).unwrap();
        let persister = before.read(Role::Persister).unwrap();
        let mut dead = recording.child.take().unwrap();
        dead.kill().unwrap();
        dead.wait().unwrap();
        recording.wait_for(|control| {
            control.read(Role::Collector).unwrap().publication >= collector.publication + 2
        });
        assert_eq!(
            before.read(Role::Persister).unwrap().creation,
            persister.creation
        );
        assert!(before.read(Role::Persister).unwrap().alive().unwrap());
        assert_eq!(0, unsafe { kill(persister.pid, 9) });
        recording.wait_for(|control| !control.read(Role::Persister).unwrap().alive().unwrap());
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(
            before.read(Role::Persister).unwrap().creation,
            persister.creation
        );
        recording.launch(PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"));
        let after = recording.wait_for(|control| {
            let current = control.read(Role::Persister).unwrap();
            current.alive().unwrap() && current.creation != persister.creation
        });
        assert_eq!(before.authority(), after.authority());
        assert_eq!(
            collector.creation,
            after.read(Role::Collector).unwrap().creation
        );
        Control::open_writable(&recording.root)
            .unwrap()
            .request_stop()
            .unwrap();
        recording.wait_for_exit();
        recording.assert_saved_publications(after.read(Role::Collector).unwrap().publication);
    }

    #[test]
    fn a_relaunched_controller_recovers_a_subsequent_adopted_worker_exit() {
        // Goal: native exit status remains observable after adopting a non-child worker.
        // Method: relaunch around two live workers, then kill the adopted persister.
        let mut recording = Recording::start("adopt-exit", 30);
        let control = recording.wait_for(|control| {
            control.read(Role::Collector).unwrap().state == State::Running
                && control.read(Role::Persister).unwrap().alive().unwrap()
        });
        let acquisition = control.read(Role::Collector).unwrap();
        let persistence = control.read(Role::Persister).unwrap();
        let original = control.read(Role::Controller).unwrap();
        let mut dead = recording.child.take().unwrap();
        dead.kill().unwrap();
        dead.wait().unwrap();
        recording.launch(PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"));
        recording.wait_for(|control| {
            let current = control.read(Role::Controller).unwrap();
            current.creation != original.creation && current.state == State::Running
        });
        assert_eq!(0, unsafe { kill(persistence.pid, 9) });
        let began = Instant::now();
        recording.wait_for(|control| {
            let current = control.read(Role::Persister).unwrap();
            current.creation != persistence.creation && current.alive().unwrap()
        });
        assert!(began.elapsed() < Duration::from_secs(6));
        assert_eq!(
            acquisition.creation,
            control.read(Role::Collector).unwrap().creation
        );
        Control::open_writable(&recording.root)
            .unwrap()
            .request_stop()
            .unwrap();
        recording.wait_for_exit();
        recording.assert_saved_publications(control.read(Role::Collector).unwrap().publication);
    }

    #[test]
    fn a_competing_controller_cannot_replace_or_stop_the_owner() {
        // Goal: the recorder lock grants a single controller without disturbing a live cohort.
        // Method: launch a second real controller, then verify native worker identities.
        let mut recording = Recording::start("contend", 8);
        let control = recording.wait_for(|control| {
            control.read(Role::Collector).unwrap().state == State::Running
                && control.read(Role::Persister).unwrap().alive().unwrap()
        });
        let collector = control.read(Role::Collector).unwrap();
        let persister = control.read(Role::Persister).unwrap();
        let result =
            Command::new(PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"))
                .arg("--recording")
                .arg(&recording.root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
        assert!(!result.success());
        assert_eq!(
            collector.creation,
            control.read(Role::Collector).unwrap().creation
        );
        assert_eq!(
            persister.creation,
            control.read(Role::Persister).unwrap().creation
        );
        assert!(control.read(Role::Collector).unwrap().alive().unwrap());
        assert!(control.read(Role::Persister).unwrap().alive().unwrap());
        recording.wait_for_exit();
        recording.assert_saved_publications(control.read(Role::Collector).unwrap().publication);
    }

    #[test]
    fn a_failed_launch_backs_off_and_recovers_after_the_executable_returns() {
        // Goal: a missing sibling cannot permanently disable saving or replace acquisition.
        // Method: withhold only this fixture's persister executable, then restore it.
        let mut recording = Recording::new("launch", 30);
        let binaries = recording.root.join("bin");
        fs::DirBuilder::new().mode(0o700).create(&binaries).unwrap();
        let built = PathBuf::from(env!("CARGO_BIN_EXE_persister"));
        fs::copy(built.with_file_name("recorder"), binaries.join("recorder")).unwrap();
        symlink(
            built.with_file_name("collector"),
            binaries.join("collector"),
        )
        .unwrap();
        recording.launch(binaries.join("recorder"));
        let control =
            recording.wait_for(|control| control.read(Role::Controller).unwrap().retries >= 3);
        let delayed = control.read(Role::Controller).unwrap();
        assert_eq!(
            delayed.failure,
            collector::recording::control::Failure::Launch
        );
        assert_eq!(delayed.state, State::Retrying);
        assert!(delayed.next_attempt_ns > delayed.heartbeat_ns);
        assert!(delayed.next_attempt_ns - delayed.heartbeat_ns <= 5_000_000_000);
        let acquisition = control.read(Role::Collector).unwrap();
        std::thread::sleep(Duration::from_millis(2100));
        let refreshed = control.read(Role::Controller).unwrap();
        assert!(refreshed.heartbeat_ns > delayed.heartbeat_ns + 1_000_000_000);
        assert_eq!(refreshed.next_attempt_ns, delayed.next_attempt_ns);
        let cleared = Instant::now();
        symlink(&built, binaries.join("persister")).unwrap();
        recording.wait_for(|control| control.read(Role::Persister).unwrap().alive().unwrap());
        assert!(cleared.elapsed() < Duration::from_secs(6));
        assert_eq!(
            acquisition.creation,
            control.read(Role::Collector).unwrap().creation
        );
        Control::open_writable(&recording.root)
            .unwrap()
            .request_stop()
            .unwrap();
        recording.wait_for_exit();
        recording.assert_saved_publications(control.read(Role::Collector).unwrap().publication);
    }

    #[test]
    fn a_transient_write_failure_recovers_the_tail_after_permissions_return() {
        // Goal: a real storage refusal leaves structured retry state and recovers exact tail.
        // Method: make the isolated data destination read-only until the first commit fails.
        let mut recording = Recording::start("write", 6);
        let control =
            recording.wait_for(|control| control.read(Role::Persister).unwrap().alive().unwrap());
        let data = recording.root.join("data");
        fs::set_permissions(&data, fs::Permissions::from_mode(0o500)).unwrap();
        let delayed =
            recording.wait_for(|control| control.read(Role::Controller).unwrap().retries > 0);
        assert_eq!(delayed.read(Role::Persister).unwrap().publication, 0);
        assert_ne!(delayed.read(Role::Persister).unwrap().state, State::Running);
        let cleared = Instant::now();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
        recording.wait_for_exit();
        assert!(cleared.elapsed() < Duration::from_secs(30));
        recording.assert_saved_publications(control.read(Role::Collector).unwrap().publication);
    }

    #[test]
    fn a_native_sync_failure_recovers_without_advancing_durable_health_early() {
        // Goal: native full-sync EIO reaches the commit owner and the controller recovers its tail.
        // Method: interpose only this isolated persister's Parquet sync while an owned flag exists.
        if let Some(path) = std::env::var_os("FLIGHTDECK_FCNTL_PROBE") {
            verify_fcntl_forwarding(&PathBuf::from(path));
            return;
        }
        let mut recording = Recording::new("sync", 6);
        let library = recording.root.join("sync-fault.dylib");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let built = Command::new(rustc)
            .args(["--edition=2024", "--crate-type=cdylib"])
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/faults/sync.rs"))
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        for injected in [false, true] {
            let mut probe = fcntl_probe_command(&recording.root.join("forwarding"), "owner");
            if injected {
                probe.env("DYLD_INSERT_LIBRARIES", &library);
            }
            let output = probe.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            eprintln!(
                "fcntl_forwarding injected={injected} F_GETPATH=pass F_SETFD=pass \
                 F_SETLK=pass F_GETLK=pass"
            );
        }
        recording.sync_fault = Some(library);
        let diagnostic = recording.root.join("diagnostics");
        recording.launch_with(
            PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"),
            Stdio::null(),
            Stdio::from(fs::File::create_new(&diagnostic).unwrap()),
        );
        let control =
            recording.wait_for(|control| control.read(Role::Persister).unwrap().alive().unwrap());
        fs::write(recording.root.join("refuse-sync"), b"refuse").unwrap();
        recording.wait_for(|control| control.read(Role::Controller).unwrap().retries > 0);
        let path = fs::read_to_string(recording.root.join("sync-observed")).unwrap();
        assert!(PathBuf::from(&path).starts_with(recording.root.canonicalize().unwrap()));
        assert!(path.ends_with(".parquet"));
        assert!(path.contains(".pending/"));
        assert_eq!(control.read(Role::Persister).unwrap().publication, 0);
        assert_ne!(control.read(Role::Persister).unwrap().state, State::Running);
        let before = Instant::now();
        fs::remove_file(recording.root.join("refuse-sync")).unwrap();
        recording.wait_for_exit();
        assert!(before.elapsed() < Duration::from_secs(30));
        let output = fs::read_to_string(&diagnostic).unwrap();
        assert!(
            output.contains("Input/output error (os error 5)"),
            "{output}"
        );
        eprintln!(
            "native_sync_eio path={path} recovery_ns={} diagnostics={output}",
            before.elapsed().as_nanos()
        );
        recording.assert_saved_publications(control.read(Role::Collector).unwrap().publication);
    }

    #[repr(C)]
    struct FileRegion {
        start: i64,
        length: i64,
        pid: i32,
        kind: i16,
        whence: i16,
    }

    fn fcntl_probe_command(path: &std::path::Path, mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::a_native_sync_failure_recovers_without_advancing_durable_health_early",
                "--nocapture",
            ])
            .env("FLIGHTDECK_FCNTL_PROBE", path)
            .env("FLIGHTDECK_FCNTL_MODE", mode);
        command
    }

    fn verify_fcntl_forwarding(path: &std::path::Path) {
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn fcntl(descriptor: i32, command: i32, ...) -> i32;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap();
        let descriptor = file.as_raw_fd();
        let mut bytes = [0_u8; 1024];
        assert_eq!(unsafe { fcntl(descriptor, 50, bytes.as_mut_ptr()) }, 0);
        let resolved = std::ffi::CStr::from_bytes_until_nul(&bytes)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(PathBuf::from(resolved), path.canonicalize().unwrap());
        assert_eq!(unsafe { fcntl(descriptor, 2, 1) }, 0);
        assert_eq!(unsafe { fcntl(descriptor, 1) } & 1, 1);
        let mut region = FileRegion {
            start: 0,
            length: 1,
            pid: 0,
            kind: 3,
            whence: 0,
        };
        let mode = std::env::var("FLIGHTDECK_FCNTL_MODE").unwrap();
        if mode == "contended" {
            assert_eq!(unsafe { fcntl(descriptor, 8, &raw const region) }, -1);
            assert!(matches!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(13 | 35)
            ));
            assert_eq!(unsafe { fcntl(descriptor, 7, &raw mut region) }, 0);
            assert_eq!(region.kind, 3);
            assert!(region.pid > 0);
            return;
        }
        assert_eq!(unsafe { fcntl(descriptor, 8, &raw const region) }, 0);
        if mode == "owner" {
            let child = fcntl_probe_command(path, "contended").output().unwrap();
            assert!(
                child.status.success(),
                "{}",
                String::from_utf8_lossy(&child.stderr)
            );
        }
        region.kind = 2;
        assert_eq!(unsafe { fcntl(descriptor, 8, &raw const region) }, 0);
        if mode == "owner" {
            let child = fcntl_probe_command(path, "free").output().unwrap();
            assert!(
                child.status.success(),
                "{}",
                String::from_utf8_lossy(&child.stderr)
            );
        }
    }

    #[test]
    fn resource_pressure_has_bounded_retry_cadence_and_preserves_acquisition() {
        // Goal: a watchdog refusal cannot become a rapid restart loop or stop acquisition.
        // Method: force the isolated encoder below its startup footprint and observe retries.
        let mut recording = Recording::new("pressure", 45);
        recording.memory = 1;
        recording.launch(PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"));
        let control =
            recording.wait_for(|control| control.read(Role::Controller).unwrap().retries > 0);
        let acquisition = control.read(Role::Collector).unwrap();
        let began = Instant::now();
        let initial = control.read(Role::Controller).unwrap().retries;
        recording
            .wait_for(|control| control.read(Role::Controller).unwrap().retries >= initial + 3);
        let health = control.read(Role::Controller).unwrap();
        assert_eq!(
            health.failure,
            collector::recording::control::Failure::Resource
        );
        assert!(began.elapsed() >= Duration::from_secs(7));
        assert!(
            (4_900_000_000..=5_000_000_000)
                .contains(&(health.next_attempt_ns - health.heartbeat_ns))
        );
        assert_eq!(
            acquisition.creation,
            control.read(Role::Collector).unwrap().creation
        );
        assert!(control.read(Role::Collector).unwrap().publication > acquisition.publication + 5);
        eprintln!(
            "resource_backoff retries={} elapsed_ns={} next_delay_ns={}",
            health.retries,
            began.elapsed().as_nanos(),
            health.next_attempt_ns - health.heartbeat_ns
        );
        // Restore the fixture's explicit configuration through a controller relaunch so it
        // can drain and prove bounded cleanup; this is not an automatic pressure-clear claim.
        let mut controller = recording.child.take().unwrap();
        controller.kill().unwrap();
        controller.wait().unwrap();
        recording.memory = 256 * 1024 * 1024;
        recording.launch(PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"));
        recording.wait_for(|control| control.read(Role::Persister).unwrap().alive().unwrap());
        Control::open_writable(&recording.root)
            .unwrap()
            .request_stop()
            .unwrap();
        recording.wait_for_exit();
        recording.assert_saved_publications(control.read(Role::Collector).unwrap().publication);
    }

    #[test]
    fn stopping_a_replacement_before_its_first_timed_tick_drains_both_exact_tails() {
        // Goal: stopping before the replacement persister launches cannot skip the new tail.
        // Method: kill acquisition, stop at replacement registration, and query both exact runs.
        let mut recording = Recording::start("collector-exit", 30);
        let control = recording.wait_for(|control| {
            control.read(Role::Collector).unwrap().state == State::Running
                && control.read(Role::Persister).unwrap().alive().unwrap()
        });
        let first = control.read(Role::Collector).unwrap();
        let reader = collector::ring::RingReader::open(&recording.place.lock_path).unwrap();
        assert_eq!(0, unsafe { kill(first.pid, 9) });
        let root = recording.root.clone();
        recording.wait_for(|control| {
            collector::recording::Registry::read(&root)
                .unwrap()
                .runs
                .len()
                == 2
                && control.read(Role::Persister).unwrap().state == State::Stopped
        });
        let replacement = control.read(Role::Collector).unwrap();
        eprintln!("stop at replacement registration before timed tick: {replacement:?}");
        let first_newest = reader.publication_newest().unwrap();
        let registry = collector::recording::Registry::read(&recording.root).unwrap();
        assert_eq!(registry.runs.len(), 2);
        let replacement_source =
            collector::ring::RingReader::open_generation(&registry.runs[1].ring_name).unwrap();
        let replacement_first = replacement_source.publication_first();
        let replacement_newest = replacement_source.publication_newest().unwrap();
        assert!(replacement_newest <= replacement_first);
        eprintln!(
            "replacement before timed tick first={replacement_first} newest={replacement_newest}"
        );
        assert_ne!(registry.runs[0].identity.run, registry.runs[1].identity.run);
        assert!(
            collector::recording::Registry::completed(
                &recording.root,
                &registry.runs[0].identity,
                &registry.runs[0].ring_name
            )
            .unwrap()
        );
        Control::open_writable(&recording.root)
            .unwrap()
            .request_stop()
            .unwrap();
        recording.wait_for_exit();
        recording.assert_saved_run(registry.runs[0].identity, 1, first_newest);
        recording.assert_saved_run(
            registry.runs[1].identity,
            replacement_first,
            control.read(Role::Collector).unwrap().publication,
        );
    }

    #[test]
    fn an_invariant_abort_stops_restarts_and_still_allows_the_healthy_persister_to_drain() {
        // Goal: an invariant-class acquisition exit remains visible instead of looping.
        // Method: abort only this owned collector, observe terminal control and exact old tail.
        let mut recording = Recording::start("invariant", 30);
        let control = recording.wait_for(|control| {
            control.read(Role::Collector).unwrap().state == State::Running
                && control.read(Role::Persister).unwrap().alive().unwrap()
        });
        let acquisition = control.read(Role::Collector).unwrap();
        let reader = collector::ring::RingReader::open(&recording.place.lock_path).unwrap();
        assert_eq!(0, unsafe { kill(acquisition.pid, 6) });
        let mut controller = recording.child.take().unwrap();
        let mut exited = false;
        for _attempt in 0..1000 {
            if let Some(status) = controller.try_wait().unwrap() {
                assert!(!status.success());
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !exited {
            controller.kill().unwrap();
            controller.wait().unwrap();
        }
        assert!(
            exited,
            "controller did not classify invariant exit within ten steps"
        );
        recording
            .wait_for(|control| control.read(Role::Persister).unwrap().state == State::Stopped);
        let terminal = control.read(Role::Controller).unwrap();
        assert_eq!(terminal.state, State::Failed);
        assert_eq!(
            terminal.failure,
            collector::recording::control::Failure::Invariant
        );
        assert_eq!(
            control.read(Role::Collector).unwrap().creation,
            acquisition.creation
        );
        assert_eq!(
            collector::recording::Registry::read(&recording.root)
                .unwrap()
                .runs
                .len(),
            1
        );
        recording.assert_saved_publications(reader.publication_newest().unwrap());
    }

    #[test]
    fn blocked_diagnostics_do_not_block_drain_or_controller_lock_release() {
        // Goal: health and owned-worker progress survive a completely blocked diagnostic sink.
        // Method: prefill a bounded Unix stream, keep its reader open and never consume it.
        use std::io::Write;
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;
        let mut recording = Recording::new("blocked", 6);
        let (_reader, mut sink) = UnixStream::pair().unwrap();
        sink.set_nonblocking(true).unwrap();
        let mut full = false;
        for _attempt in 0..16384 {
            match sink.write(&[b'x'; 4096]) {
                Ok(count) => assert!(count > 0),
                Err(error) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
                    full = true;
                    break;
                }
            }
        }
        assert!(full);
        sink.set_nonblocking(false).unwrap();
        recording.launch_with(
            PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"),
            Stdio::from(OwnedFd::from(sink.try_clone().unwrap())),
            Stdio::from(OwnedFd::from(sink)),
        );
        let control = recording
            .wait_for(|control| control.read(Role::Collector).unwrap().state == State::Running);
        recording.wait_for_exit();
        assert_eq!(
            control.read(Role::Controller).unwrap().state,
            State::Stopped
        );
        recording.assert_saved_publications(control.read(Role::Collector).unwrap().publication);
        assert!(
            !collector::recording::private::FileLock::held(&recording.root.join("recorder.lock"))
                .unwrap()
        );
    }

    #[test]
    #[ignore = "opt-in native full awake-minute resource observation"]
    fn reports_all_recording_roles_over_an_ordinary_awake_minute() {
        // Goal: an ordinary recording accounts for controller and both worker roles.
        // Method: observe the release controller's actual full awake minute and final drain.
        resource_minute(false);
    }

    #[test]
    #[ignore = "opt-in native full awake-minute watchdog resource observation"]
    fn reports_all_restarted_workers_over_a_pressure_awake_minute() {
        // Goal: persistent watchdog exits remain included in the recording CPU interval.
        // Method: refuse only this fixture's memory admission for a complete awake minute.
        resource_minute(true);
    }

    fn resource_minute(pressure: bool) {
        let name = if pressure {
            "cost-pressure"
        } else {
            "cost-ordinary"
        };
        let mut recording = Recording::new(name, 70);
        if pressure {
            recording.memory = 1;
        }
        let output = recording.root.join("resources");
        recording.launch_with(
            PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"),
            Stdio::from(fs::File::create_new(&output).unwrap()),
            Stdio::from(fs::File::create_new(recording.root.join("diagnostics")).unwrap()),
        );
        let control = recording
            .wait_for(|control| control.read(Role::Collector).unwrap().state == State::Running);
        eprintln!(
            "resource_start pressure={pressure} root={} controller={:?} collector={:?} \
             persister={:?}",
            recording.root.display(),
            control.read(Role::Controller).unwrap(),
            control.read(Role::Collector).unwrap(),
            control.read(Role::Persister).unwrap()
        );
        let report = resource_minute_wait(&output);
        eprintln!("resource_report pressure={pressure}\n{report}");
        eprintln!(
            "resource_end controller={:?}",
            control.read(Role::Controller).unwrap()
        );
        if pressure {
            let mut controller = recording.child.take().unwrap();
            controller.kill().unwrap();
            controller.wait().unwrap();
            recording.memory = 256 * 1024 * 1024;
            recording
                .launch(PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"));
        }
        Control::open_writable(&recording.root)
            .unwrap()
            .request_stop()
            .unwrap();
        recording.wait_for_exit();
        let newest = control.read(Role::Collector).unwrap().publication;
        assert_eq!(control.read(Role::Persister).unwrap().publication, newest);
        let registry = collector::recording::Registry::read(&recording.root).unwrap();
        let store = persistence::Store::open(&recording.root).unwrap();
        assert_eq!(registry.runs.len(), 1);
        let final_window = store.progress(&registry.runs[0].identity).unwrap().unwrap();
        assert_eq!(final_window.publication_end, newest + 1);
        assert_eq!(final_window.losses, persistence::Losses::default());
        eprintln!(
            "resource_drained publications_end={} canonical_window={}",
            newest + 1,
            final_window.identity.window
        );
    }

    fn resource_minute_wait(output: &std::path::Path) -> String {
        for _attempt in 0..800 {
            let report = fs::read_to_string(output).unwrap();
            if let Some(minute) = report
                .lines()
                .find(|line| line.starts_with("recording minute "))
            {
                assert!(minute.contains("clock=awake_ns "));
                assert!(!minute.contains("unavailable"));
                let elapsed = minute
                    .split_ascii_whitespace()
                    .find_map(|word| word.strip_prefix("elapsed_ns="))
                    .unwrap()
                    .parse::<u64>()
                    .unwrap();
                assert!(elapsed >= 60_000_000_000);
                assert!(report.contains("recorder_footprint_bytes="));
                assert!(report.contains("acquisition_footprint_bytes="));
                return report;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("no full awake resource interval within eighty seconds");
    }

    impl Recording {
        fn fixture_writer(&self) -> collector::ring::Writer {
            let shape = collector::ring::Shape {
                slot_count: 2,
                cpu_capacity: 1,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: 1,
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity: 2,
            };
            let (writer, registered) =
                collector::recording::Registration::start(&self.root, &self.place, shape, 1)
                    .unwrap()
                    .into_parts();
            registered.unwrap();
            writer
        }

        fn fixture_closed(&self) -> collector::ring::RunIdentity {
            let mut writer = self.fixture_writer();
            let identity = writer.identity().unwrap();
            let memory = collector::machine::Memory::default();
            let readings = collector::ring::Readings {
                run_tick: 0,
                cpus: &[],
                memory: &memory,
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[collector::tracker::Event::Exec { pid: 41 }],
                dropped: collector::ring::Dropped::default(),
            };
            writer.publish_observed(
                &readings,
                &collector::ring::ObservationTimes {
                    wall_ns: 1_000_000_000,
                    continuous_ns: 1,
                    awake_ns: 1,
                },
            );
            writer.finish();
            drop(writer);
            identity
        }

        fn fixture_terminal(&self) -> collector::ring::RunIdentity {
            let identity = self.fixture_closed();
            let mut registry = collector::recording::Registry::read(&self.root).unwrap();
            let source = registry.runs.last().unwrap().ring_name.clone();
            let target = format!("{}.99", source.rsplit_once('.').unwrap().0);
            super::terminal_source::withhold(&source, &target, 1);
            assert_eq!(registry.runs.len(), 2);
            registry.runs[1].ring_name.clone_from(&target);
            registry.runs[1].collector_pid = 0;
            registry.write(&self.root).unwrap();
            unlink_generation(&source).unwrap();
            identity
        }
        fn start(name: &str, seconds: u64) -> Self {
            let mut recording = Self::new(name, seconds);
            recording
                .launch(PathBuf::from(env!("CARGO_BIN_EXE_persister")).with_file_name("recorder"));
            recording
        }

        fn new(name: &str, seconds: u64) -> Self {
            let root =
                std::env::temp_dir().join(format!("fd-recorder-{}-{name}", std::process::id()));
            assert!(!root.exists());
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let ring = format!("fd-recorder-{}-{name}", std::process::id());
            let place = Place::for_user(&ring).unwrap();
            Self {
                root,
                place,
                child: None,
                ring,
                seconds,
                memory: 256 * 1024 * 1024,
                sync_fault: None,
            }
        }

        fn launch(&mut self, executable: PathBuf) {
            self.launch_with(executable, Stdio::null(), Stdio::null());
        }

        fn launch_with(&mut self, executable: PathBuf, stdout: Stdio, stderr: Stdio) {
            assert!(self.child.is_none());
            let mut command = Command::new(executable);
            if let Some(library) = &self.sync_fault {
                command
                    .env("DYLD_INSERT_LIBRARIES", library)
                    .env("FLIGHTDECK_TEST_REFUSE_SYNC", self.root.join("refuse-sync"))
                    .env(
                        "FLIGHTDECK_TEST_SYNC_OBSERVED",
                        self.root.join("sync-observed"),
                    );
            }
            let child = command
                .args([
                    "--recording",
                    self.root.to_str().unwrap(),
                    "--ring",
                    &self.ring,
                    "--seconds",
                    &self.seconds.to_string(),
                    "--memory-bytes",
                    &self.memory.to_string(),
                ])
                .stdout(stdout)
                .stderr(stderr)
                .spawn()
                .unwrap();
            self.child = Some(child);
        }

        fn wait_for(&mut self, predicate: impl Fn(&Control) -> bool) -> Control {
            let start = Instant::now();
            for _attempt in 0..3000 {
                assert!(
                    self.child
                        .as_mut()
                        .is_none_or(|child| child.try_wait().unwrap().is_none()),
                    "recorder exited before readiness"
                );
                if let Ok(control) = Control::open(&self.root) {
                    if predicate(&control) {
                        return control;
                    }
                }
                assert!(start.elapsed() < Duration::from_secs(30));
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("recorder readiness timed out");
        }

        fn assert_saved_publications(&self, newest: u64) {
            let registry = collector::recording::Registry::read(&self.root).unwrap();
            assert_eq!(registry.runs.len(), 1);
            self.assert_saved_run(registry.runs[0].identity, 1, newest);
        }

        fn assert_saved_run(&self, run: collector::ring::RunIdentity, first: u64, newest: u64) {
            let store = persistence::Store::open(&self.root).unwrap();
            let manifest = store
                .existing(&persistence::WindowIdentity { run, window: 0 })
                .unwrap()
                .unwrap();
            assert_eq!(manifest.publication_first, first);
            assert_eq!(manifest.publication_end, newest + 1);
            assert_eq!(manifest.losses.publications, 0);
            assert_eq!(manifest.losses.events, 0);
            eprintln!(
                "exact_saved root={} run={run:?} publications={}..{} events={}..{} losses={:?}",
                self.root.display(),
                manifest.publication_first,
                manifest.publication_end,
                manifest.events_first,
                manifest.events_end,
                manifest.losses
            );
            let data = self
                .root
                .join(&manifest.data_directory)
                .join("ticks.parquet");
            let engine = persistence::engine::Engine::new();
            engine
                .register_lazy("ticks", persistence::engine::scan(&data).unwrap())
                .unwrap();
            let rows = engine
                .query("SELECT publication FROM ticks ORDER BY publication")
                .unwrap();
            let actual: Vec<_> = rows
                .column("publication")
                .unwrap()
                .u64()
                .unwrap()
                .into_no_null_iter()
                .collect();
            assert_eq!(actual, (first..=newest).collect::<Vec<_>>());
            let budget =
                std::sync::Arc::new(persistence::resources::ByteBudget::new(128 * 1024 * 1024));
            let relation = engine.event_source(&self.root, &manifest, &budget).unwrap();
            let events = engine
                .query(&format!("SELECT event FROM {relation} ORDER BY event"))
                .unwrap();
            let actual: Vec<_> = events
                .column("event")
                .unwrap()
                .u64()
                .unwrap()
                .into_no_null_iter()
                .collect();
            assert_eq!(
                actual,
                (manifest.events_first..manifest.events_end).collect::<Vec<_>>()
            );
        }

        fn wait_for_exit(&mut self) {
            assert!(self.wait_for_status().success());
        }

        fn wait_for_status(&mut self) -> std::process::ExitStatus {
            for _attempt in 0..3000 {
                if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                    return status;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("recorder failed to drain");
        }

        fn drop_report(&self) {
            if let Ok(output) = fs::read_to_string(self.root.join("diagnostics")) {
                eprintln!("failed fixture diagnostics: {output}");
            }
            if let Ok(control) = Control::open(&self.root) {
                eprintln!(
                    "failed fixture health: controller={:?} collector={:?} persister={:?}",
                    control.read(Role::Controller),
                    control.read(Role::Collector),
                    control.read(Role::Persister)
                );
            }
        }
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            if std::thread::panicking() {
                self.drop_report();
            }
            if self.root.join("refuse-sync").exists() {
                fs::remove_file(self.root.join("refuse-sync")).unwrap();
            }
            if self.root.join("data").exists() {
                fs::set_permissions(self.root.join("data"), fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            // Request stop even when the controller died: a surviving acquisition owns its
            // mapping, notices the request, and lets a surviving persister drain its tail.
            if let Ok(control) = Control::open_writable(&self.root) {
                control.request_stop().unwrap();
            }
            let mut stopped = false;
            for _attempt in 0..3000 {
                let controller_stopped = self
                    .child
                    .as_mut()
                    .is_none_or(|child| child.try_wait().unwrap().is_some());
                let workers_stopped = ["collector.lock", "persister.lock"].iter().all(|name| {
                    !collector::recording::private::FileLock::held(&self.root.join(name)).unwrap()
                });
                if controller_stopped && workers_stopped {
                    stopped = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if !stopped {
                if let Some(child) = &mut self.child {
                    if child.try_wait().unwrap().is_none() {
                        child.kill().unwrap();
                        child.wait().unwrap();
                    }
                }
                eprintln!(
                    "Owned recording retained for cleanup after stop timeout: {}",
                    self.root.display()
                );
                assert!(
                    std::thread::panicking(),
                    "owned workers remain alive after bounded cleanup"
                );
                return;
            }
            if let Ok(registry) = collector::recording::Registry::read(&self.root) {
                for run in registry.runs {
                    assert!(run.ring_name.starts_with(&self.place.name_prefix));
                    match unlink_generation(&run.ring_name) {
                        Ok(()) => {}
                        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
                    }
                }
            }
            if self.place.lock_path.exists() {
                let name = fs::read_to_string(&self.place.lock_path).unwrap();
                match unlink_generation(name.trim()) {
                    Ok(()) => {}
                    Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
                }
                fs::remove_file(&self.place.lock_path).unwrap();
            }
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
}
