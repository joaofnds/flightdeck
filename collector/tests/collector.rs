//! Tests of the collector binary, run as a process of its own.

// Clippy lets a test unwrap only inside a module marked as a test's.
#[cfg(test)]
mod tests {
    use std::ffi::{CString, c_char, c_int};
    use std::process::{Command, Output};

    use collector::ring::Place;

    unsafe extern "C" {
        fn shm_unlink(name: *const c_char) -> c_int;
    }

    /// A ring name only one test uses, whose lock file and ring are removed when the test
    /// ends, since a shared-memory object outlives the process that made it.
    struct TestRing {
        name: String,
        place: Place,
    }

    impl TestRing {
        fn new(test: &str) -> Self {
            let name = format!("fdc{}{test}", std::process::id());
            let place = Place::for_user(&name).unwrap();
            Self { name, place }
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
        }
    }

    fn stdout(output: &Output) -> &str {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::str::from_utf8(&output.stdout).unwrap()
    }

    /// What `ring_reader` printed of one publication.
    #[derive(Debug)]
    struct Published {
        publication: u64,
        tick: u64,
        processes: usize,
        coalitions: usize,
    }

    /// Each publication line `out` holds, in order.
    fn publications(out: &str) -> Vec<Published> {
        out.lines()
            .filter_map(|line| {
                let words: Vec<&str> = line.split(' ').collect();
                match words[..] {
                    [
                        "publication",
                        publication,
                        "tick",
                        tick,
                        "processes",
                        processes,
                        "coalitions",
                        coalitions,
                    ] => Some(Published {
                        publication: publication.parse().unwrap(),
                        tick: tick.parse().unwrap(),
                        processes: processes.parse().unwrap(),
                        coalitions: coalitions.parse().unwrap(),
                    }),
                    _ => None,
                }
            })
            .collect()
    }

    /// The last tick of the run the collector's summary `out` reports, from its line
    /// "whole run, ticks 1-N".
    fn last_tick(out: &str) -> u64 {
        let line = out
            .lines()
            .find(|line| line.starts_with("whole run, ticks 1-"))
            .unwrap();
        let ticks = line.trim_start_matches("whole run, ticks 1-");
        ticks.split(':').next().unwrap().parse().unwrap()
    }

    #[test]
    fn publishes_each_tick_to_the_ring() {
        // Goal: the collector hands each tick it reads to the ring, the warm-up tick 0
        // included, with the processes and coalitions it read.
        // Method: run the collector for two seconds under a ring name of this test's own, then
        // read as many publications of the ring it left as the run took ticks.
        let ring = TestRing::new("ticks");
        let collected = Command::new(env!("CARGO_BIN_EXE_collector"))
            .args(["--seconds", "2", "--ring", &ring.name])
            .output()
            .unwrap();
        let last_tick = last_tick(stdout(&collected));
        let count = (last_tick + 1).to_string();

        let read = Command::new(env!("CARGO_BIN_EXE_ring_reader"))
            .args(["--ring", &ring.name, "--publications", &count])
            .output()
            .unwrap();
        let found = publications(stdout(&read));

        let numbers: Vec<(u64, u64)> = found
            .iter()
            .map(|published| (published.publication, published.tick))
            .collect();
        let expected: Vec<(u64, u64)> = (0..=last_tick).map(|tick| (tick + 1, tick)).collect();
        assert_eq!(expected, numbers);
        assert!(
            found.iter().all(|published| published.processes > 0),
            "{found:?}"
        );
        assert!(
            found.iter().all(|published| published.coalitions > 0),
            "{found:?}"
        );
    }
}
