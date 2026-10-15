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

    /// The ticks of a run the collector's summary reports.
    struct RunTicks {
        /// The span's first tick, the first past the warm-up tick 0.
        first: u64,
        last: u64,
        /// The ticks of the span that never ran, since the run fell behind them.
        slips: u64,
    }

    /// The ticks the collector's summary `out` reports for its whole run, from its line
    /// "whole run, ticks F-L: ..., S slips".
    fn run_ticks(out: &str) -> RunTicks {
        let line = out
            .lines()
            .find(|line| line.starts_with("whole run, ticks "))
            .unwrap();
        let span = line.trim_start_matches("whole run, ticks ");
        let (first, rest) = span.split_once('-').unwrap();
        let (last, rest) = rest.split_once(':').unwrap();
        let slips = rest.trim_end_matches(" slips").rsplit(' ').next().unwrap();
        RunTicks {
            first: first.parse().unwrap(),
            last: last.parse().unwrap(),
            slips: slips.parse().unwrap(),
        }
    }

    #[test]
    fn publishes_each_tick_to_the_ring() {
        // Goal: the collector hands each tick it runs to the ring, the warm-up tick 0
        // included, with the processes and coalitions it read, and numbers the publications
        // without a gap whatever ticks the run slipped.
        // Method: run the collector for two seconds under a ring name of this test's own, then
        // read as many publications of the ring it left as the run ran ticks.
        let ring = TestRing::new("ticks");
        let collected = Command::new(env!("CARGO_BIN_EXE_collector"))
            .args(["--seconds", "2", "--ring", &ring.name])
            .output()
            .unwrap();
        let run = run_ticks(stdout(&collected));
        let ran = 1 + run.last - run.first + 1 - run.slips;

        let read = Command::new(env!("CARGO_BIN_EXE_ring_reader"))
            .args(["--ring", &ring.name, "--publications", &ran.to_string()])
            .output()
            .unwrap();
        let found = publications(stdout(&read));

        let numbers: Vec<u64> = found
            .iter()
            .map(|published| published.publication)
            .collect();
        let ticks: Vec<u64> = found.iter().map(|published| published.tick).collect();
        assert_eq!((1..=ran).collect::<Vec<u64>>(), numbers);
        assert_eq!(Some(&0), ticks.first());
        assert_eq!(Some(&run.last), ticks.last());
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
    }
}
