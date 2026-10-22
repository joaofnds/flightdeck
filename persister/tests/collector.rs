//! Tests of the collector binary, run as a process of its own beside the persister it starts.
//! They live in the persister's package, since cargo builds a package's own binaries for its
//! tests and the collector starts the persister from its own directory.

// Clippy lets a test unwrap only inside a module marked as a test's.
#[cfg(test)]
mod tests {
    use std::ffi::{CString, c_char, c_int};
    use std::os::unix::fs::{DirBuilderExt, symlink};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Output, Stdio};
    use std::sync::OnceLock;
    use std::time::Duration;

    use collector::recording::STOP_LIMIT_SECONDS;
    use collector::ring::{Found, Place, Reader};

    unsafe extern "C" {
        fn shm_unlink(name: *const c_char) -> c_int;
        fn kill(pid: c_int, signal: c_int) -> c_int;
    }

    /// A ring name only one test uses, whose lock file and ring are removed when the test
    /// ends, since a shared-memory object outlives the process that made it.
    struct TestRing {
        name: String,
        place: Place,
        recording: PathBuf,
    }

    impl TestRing {
        fn new(test: &str) -> Self {
            let name = format!("fdc{}{test}", std::process::id());
            let place = Place::for_user(&name).unwrap();
            let recording =
                std::env::temp_dir().join(format!("fdc-recording-{}{test}", std::process::id()));
            assert!(!recording.exists());
            Self {
                name,
                place,
                recording,
            }
        }
    }

    impl Drop for TestRing {
        fn drop(&mut self) {
            // The lock file names the ring, as the writer recorded it, so a change to how rings
            // are named cannot leave one behind unseen.
            let name = match std::fs::read(&self.place.lock_path) {
                Ok(name) => name,
                Err(error) => {
                    assert_eq!(std::io::ErrorKind::NotFound, error.kind());
                    return;
                }
            };
            if !name.is_empty() {
                let ring = CString::new(name).unwrap();
                assert_eq!(0, unsafe { shm_unlink(ring.as_ptr()) }, "unlink {ring:?}");
            }
            std::fs::remove_file(&self.place.lock_path).unwrap();
            if self.recording.exists() {
                std::fs::remove_dir_all(&self.recording).unwrap();
            }
        }
    }

    /// The collector binary, built by cargo into the directory of the persister this package's
    /// tests run, once for all of them, so the collector starts this persister.
    fn collector() -> &'static Path {
        static BUILT: OnceLock<PathBuf> = OnceLock::new();
        BUILT.get_or_init(|| {
            let persister = Path::new(env!("CARGO_BIN_EXE_persister"));
            let profile_directory = persister.parent().unwrap();
            let target_directory = profile_directory.parent().unwrap();
            // Cargo names the dev profile's directory debug and every other after its profile.
            let profile = match profile_directory.file_name().unwrap().to_str().unwrap() {
                "debug" => "dev",
                other => other,
            };

            let built = Command::new(env!("CARGO"))
                .args(["build", "--locked"])
                .args(["--package", "collector", "--bin", "collector"])
                .args(["--profile", profile])
                .arg("--target-dir")
                .arg(target_directory)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap();

            assert!(
                built.status.success(),
                "{}",
                String::from_utf8_lossy(&built.stderr)
            );
            let collector = profile_directory.join("collector");
            assert!(collector.is_file(), "{collector:?}");
            collector
        })
    }

    fn stdout(output: &Output) -> &str {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::str::from_utf8(&output.stdout).unwrap()
    }

    struct RunningCollector(Option<Child>);

    impl RunningCollector {
        fn start(ring: &TestRing) -> Self {
            let child = Command::new(collector())
                .arg("--ring")
                .arg(&ring.name)
                .arg("--recording")
                .arg(&ring.recording)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap();
            Self(Some(child))
        }

        fn interrupt_group(&self) {
            const SIGINT: c_int = 2;
            let pid = i32::try_from(self.0.as_ref().unwrap().id()).unwrap();
            assert_eq!(0, unsafe { kill(-pid, SIGINT) });
        }

        /// Sends SIGTERM to the collector and to each child it started, as macOS does to every
        /// process at logout or shutdown.
        fn terminate_with_children(&self) {
            const SIGTERM: c_int = 15;
            let pid = self.0.as_ref().unwrap().id();
            let children = Command::new("/usr/bin/pgrep")
                .args(["-P", &pid.to_string()])
                .output()
                .unwrap();
            let children = String::from_utf8(children.stdout).unwrap();
            assert_eq!(1, children.lines().count(), "{children}");

            for child in children.lines() {
                assert_eq!(0, unsafe { kill(child.parse().unwrap(), SIGTERM) });
            }
            assert_eq!(0, unsafe { kill(i32::try_from(pid).unwrap(), SIGTERM) });
        }

        /// Waits for the collector to exit, for as long as it may take once asked to stop and a
        /// margin for the alarm that ends it there. Its drain waits on the disk, and so on every
        /// other process writing to it, so a shorter wait fails a collector that keeps its limit.
        fn finish(&mut self) -> Output {
            const FINISH_POLLS: u32 = (STOP_LIMIT_SECONDS + 30) * 100; // 10 ms apart

            for _attempt in 0..FINISH_POLLS {
                if self.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                    return self.0.take().unwrap().wait_with_output().unwrap();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("collector did not finish its interrupted recording within its stop limit");
        }
    }

    impl Drop for RunningCollector {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                if child.try_wait().unwrap().is_none() {
                    child.kill().unwrap();
                    child.wait().unwrap();
                }
            }
        }
    }

    fn wait_for_publications(ring: &TestRing) {
        for _attempt in 0..2_000 {
            match collector::ring::RingReader::open(&ring.place.lock_path) {
                Ok(reader) => {
                    if reader.publication_newest().unwrap() >= 2 {
                        return;
                    }
                }
                Err(error) => assert_eq!(std::io::ErrorKind::NotFound, error.kind()),
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("collector did not publish its first scheduled tick");
    }

    fn only_entry(path: &Path) -> PathBuf {
        let mut entries = std::fs::read_dir(path).unwrap();
        let path = entries.next().unwrap().unwrap().path();
        assert!(entries.next().is_none());
        path
    }

    fn committed_range(root: &Path) -> std::ops::Range<u64> {
        let run = only_entry(&root.join("commits"));
        let day = only_entry(&run);
        let marker = only_entry(&day);
        let bytes = std::fs::read(marker).unwrap();
        assert_eq!(&bytes[..8], b"FDSEAL01");
        let first = u64::from_le_bytes(bytes[80..88].try_into().unwrap());
        let end = u64::from_le_bytes(bytes[88..96].try_into().unwrap());
        first..end
    }

    /// What the test read of one publication.
    #[derive(Debug)]
    struct Published {
        publication: u64,
        tick: u64,
        processes: usize,
        coalitions: usize,
    }

    /// The line "whole run, ticks F-L: ..., N slips" of the collector's summary `out`.
    fn whole_run(out: &str) -> &str {
        out.lines()
            .find(|line| line.starts_with("whole run, ticks "))
            .unwrap()
    }

    /// The last tick of the run the collector's summary `out` reports.
    fn last_tick(out: &str) -> u64 {
        let span = whole_run(out).trim_start_matches("whole run, ticks ");
        let (_, rest) = span.split_once('-').unwrap();
        let (last, _) = rest.split_once(':').unwrap();
        last.parse().unwrap()
    }

    /// The ticks the run the collector's summary `out` reports slipped, and so never ran.
    fn slips(out: &str) -> u64 {
        let line = whole_run(out).trim_end_matches(" slips");
        let (_, count) = line.rsplit_once(' ').unwrap();
        count.parse().unwrap()
    }

    /// Every publication the ring of `place` holds, read until the reader finds none, at most
    /// `count_max` of them, with none missed.
    fn read_all(place: &Place, count_max: u64) -> Vec<Published> {
        let mut reader = Reader::open(&place.lock_path).unwrap();
        let mut found = Vec::new();
        for _ in 0..count_max {
            match reader.read().unwrap() {
                Found::Publication { publication, tick } => found.push(Published {
                    publication,
                    tick,
                    processes: reader.tick().processes.len(),
                    coalitions: reader.tick().coalitions.len(),
                }),
                Found::Missed { first, end } => panic!("missed publications {first}..{end}"),
                Found::Nothing => break,
            }
        }
        found
    }

    #[test]
    fn publishes_each_tick_to_the_ring() {
        // Goal: the collector hands each tick it runs to the ring, the warm-up tick 0
        // included and every tick it did not slip, with the processes and coalitions it read,
        // and numbers the publications without a gap whatever ticks the run slipped.
        // Method: run the collector for two seconds under a ring name of this test's own, then
        // read every publication of the ring it left, in this process.
        let ring = TestRing::new("ticks");
        let collected = Command::new(collector())
            .args(["--seconds", "2", "--ring", &ring.name])
            .arg("--recording")
            .arg(&ring.recording)
            .output()
            .unwrap();
        let last_tick = last_tick(stdout(&collected));
        let slips = slips(stdout(&collected));

        let found = read_all(&ring.place, last_tick + 2);

        let numbers: Vec<u64> = found
            .iter()
            .map(|published| published.publication)
            .collect();
        let ticks: Vec<u64> = found.iter().map(|published| published.tick).collect();
        assert_eq!(
            (1..=u64::try_from(found.len()).unwrap()).collect::<Vec<u64>>(),
            numbers
        );
        assert_eq!(last_tick + 1 - slips, u64::try_from(found.len()).unwrap());
        assert_eq!(Some(&0), ticks.first());
        assert_eq!(Some(&last_tick), ticks.last());
        assert!(
            ticks.is_sorted_by(|earlier, later| earlier < later),
            "{ticks:?}"
        );
        assert!(
            found.iter().all(|published| published.processes > 0),
            "{found:?}"
        );
        assert!(
            found.iter().all(|published| published.coalitions > 0),
            "{found:?}"
        );
        assert!(stdout(&collected).contains("persister_status=0"));
        assert_eq!(1..last_tick + 2, committed_range(&ring.recording));
    }

    #[test]
    fn launches_its_real_sibling_when_invoked_through_a_symlink() {
        // Goal: an invocation directory cannot substitute a different persistence executable.
        // Method: place a failing sibling beside a collector symlink and inspect the seal.
        let ring = TestRing::new("symlink");
        let launch = ring.recording.join("launch");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&launch)
            .unwrap();
        let launch_collector = launch.join("collector");
        symlink(collector(), &launch_collector).unwrap();
        symlink("/usr/bin/false", launch.join("persister")).unwrap();

        let output = Command::new(launch_collector)
            .args(["--seconds", "1", "--ring", &ring.name])
            .arg("--recording")
            .arg(&ring.recording)
            .output()
            .unwrap();

        assert!(stdout(&output).contains("persister_status=0"));
        let reader = collector::ring::RingReader::open(&ring.place.lock_path).unwrap();
        assert_eq!(
            reader.publication_first()..reader.publication_newest().unwrap() + 1,
            committed_range(&ring.recording)
        );
    }

    #[test]
    fn drains_its_partial_recording_when_its_process_group_is_interrupted() {
        // Goal: an ordinary terminal stop seals the tail instead of killing both processes.
        // Method: interrupt a real collector's group after publications, then inspect disk.
        let ring = TestRing::new("interrupt");
        let mut collector = RunningCollector::start(&ring);
        wait_for_publications(&ring);

        collector.interrupt_group();
        let output = collector.finish();

        assert!(output.status.success(), "{output:?}");
        let reader = collector::ring::RingReader::open(&ring.place.lock_path).unwrap();
        assert!(reader.completed());
        assert_eq!(
            reader.publication_first()..reader.publication_newest().unwrap() + 1,
            committed_range(&ring.recording)
        );
        assert!(stdout(&output).contains("persister_status=0"));
    }

    #[test]
    fn drains_its_partial_recording_when_every_process_is_terminated() {
        // Goal: the tail is sealed when macOS terminates every process at logout or shutdown,
        // which the persister outside the collector's group meets too.
        // Method: terminate a real collector and its persister after publications, then inspect
        // disk.
        let ring = TestRing::new("terminate");
        let mut collector = RunningCollector::start(&ring);
        wait_for_publications(&ring);

        collector.terminate_with_children();
        let output = collector.finish();

        assert!(output.status.success(), "{output:?}");
        assert!(stdout(&output).contains("persister_status=0"), "{output:?}");
        let reader = collector::ring::RingReader::open(&ring.place.lock_path).unwrap();
        assert!(reader.completed());
        assert_eq!(
            reader.publication_first()..reader.publication_newest().unwrap() + 1,
            committed_range(&ring.recording)
        );
    }
}
