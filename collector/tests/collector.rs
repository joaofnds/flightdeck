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

    const ENOENT: i32 = 2;

    /// A ring name only one test uses, whose lock file and first ring are removed when the
    /// test ends, since a shared-memory object outlives the process that made it.
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
            let ring = CString::new(format!("{}1", self.place.name_prefix)).unwrap();
            if unsafe { shm_unlink(ring.as_ptr()) } != 0 {
                assert_eq!(Some(ENOENT), std::io::Error::last_os_error().raw_os_error());
            }
            if let Err(error) = std::fs::remove_file(&self.place.lock_path) {
                assert_eq!(std::io::ErrorKind::NotFound, error.kind());
            }
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

    /// The tick and the process count of each publication line `out` holds, in order.
    fn publications(out: &str) -> Vec<(u64, u64, usize)> {
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
                        ..,
                    ] => Some((
                        publication.parse().unwrap(),
                        tick.parse().unwrap(),
                        processes.parse().unwrap(),
                    )),
                    _ => None,
                }
            })
            .collect()
    }

    #[test]
    fn publishes_each_tick_to_the_ring() {
        // Goal: the collector hands each tick it reads to the ring, the warm-up tick 0
        // included, with the processes it read.
        // Method: run the collector for two seconds under a ring name of this test's own, then
        // read the first two publications of the ring it left.
        let ring = TestRing::new("ticks");
        let collected = Command::new(env!("CARGO_BIN_EXE_collector"))
            .args(["--seconds", "2", "--ring", &ring.name])
            .output()
            .unwrap();
        stdout(&collected);

        let read = Command::new(env!("CARGO_BIN_EXE_ring_reader"))
            .args(["--ring", &ring.name, "--publications", "2"])
            .output()
            .unwrap();
        let found = publications(stdout(&read));

        assert_eq!(2, found.len(), "{found:?}");
        assert_eq!((1, 0), (found[0].0, found[0].1));
        assert_eq!(2, found[1].0);
        assert!(found[1].1 >= 1, "{found:?}");
        assert!(
            found.iter().all(|&(_, _, processes)| processes > 0),
            "{found:?}"
        );
    }
}
