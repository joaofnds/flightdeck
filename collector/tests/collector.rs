//! Tests of the collector binary, run as a process of its own.

// Clippy lets a test unwrap only inside a module marked as a test's.
#[cfg(test)]
mod tests {
    use std::ffi::{CString, c_char, c_int};
    use std::process::{Command, Output};

    use collector::ring::{Found, Place, Reader};

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
        let collected = Command::new(env!("CARGO_BIN_EXE_collector"))
            .args(["--seconds", "2", "--ring", &ring.name])
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
    }
}
