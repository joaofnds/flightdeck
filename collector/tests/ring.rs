//! Tests of the ring across processes, which run the reader binary as the other process.

// Clippy lets a test unwrap only inside a module marked as a test's.
#[cfg(test)]
mod tests {
    use std::ffi::{CString, c_char, c_int};
    use std::path::PathBuf;
    use std::process::{Command, Output};

    use collector::ring::{Place, Shape, Writer};

    unsafe extern "C" {
        fn shm_unlink(name: *const c_char) -> c_int;
    }

    /// A ring's place that only one test uses, which removes the lock file and every ring it
    /// named when the test ends, since a shared-memory object outlives the process that made it.
    struct TestPlace {
        place: Place,
    }

    /// The most generations a test makes in one place.
    const TEST_GENERATIONS_MAX: u64 = 8;

    impl TestPlace {
        fn new(test: &str) -> Self {
            let pid = std::process::id();
            let lock_path = std::env::temp_dir().join(format!("flightdeck-ring-{pid}-{test}.lock"));
            Self {
                place: Place {
                    lock_path,
                    name_prefix: format!("/fdt{pid}{}.", &test[..test.len().min(6)]),
                },
            }
        }

        fn lock_path(&self) -> &PathBuf {
            &self.place.lock_path
        }
    }

    impl Drop for TestPlace {
        fn drop(&mut self) {
            for generation in 1..=TEST_GENERATIONS_MAX {
                let name = CString::new(format!("{}{generation}", self.place.name_prefix)).unwrap();
                // A generation the test never made is no object, so its unlink fails.
                if unsafe { shm_unlink(name.as_ptr()) } != 0 {
                    assert_eq!(
                        Some(2),
                        std::io::Error::last_os_error().raw_os_error(),
                        "unlink {name:?}"
                    );
                }
            }
            if let Err(error) = std::fs::remove_file(&self.place.lock_path) {
                assert_eq!(std::io::ErrorKind::NotFound, error.kind());
            }
        }
    }

    fn read_ring(place: &TestPlace, flags: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ring_reader"))
            .arg("--lock")
            .arg(place.lock_path())
            .args(flags)
            .output()
            .unwrap()
    }

    fn stdout(output: &Output) -> &str {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::str::from_utf8(&output.stdout).unwrap()
    }

    #[test]
    fn a_reader_in_another_process_reads_the_tick_a_writer_published() {
        // Goal: a process other than the writer reads a tick the writer published, with its tick
        // number.
        // Method: publish tick 7 in this process and read one publication with the reader binary.
        let place = TestPlace::new("first");
        let mut writer = Writer::create(&place.place, Shape { slot_count: 4 }).unwrap();

        writer.publish(7).unwrap();
        let output = read_ring(&place, &["--publications", "1"]);

        assert_eq!("publication 1 tick 7\n", stdout(&output));
    }
}
