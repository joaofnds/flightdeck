//! Tests of the ring across processes, which run the reader binary as the other process.

// Clippy lets a test unwrap only inside a module marked as a test's.
#[cfg(test)]
mod tests {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::{Mutex, MutexGuard};

    use collector::ring::{Found, Place, Reader, Shape, Writer};

    unsafe extern "C" {
        fn shm_open(name: *const c_char, oflag: c_int, ...) -> c_int;
        fn shm_unlink(name: *const c_char) -> c_int;
        fn mmap(
            address: *mut c_void,
            length: usize,
            protection: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
        fn mprotect(address: *mut c_void, length: usize, protection: c_int) -> c_int;
        fn munmap(address: *mut c_void, length: usize) -> c_int;
        fn getuid() -> u32;
    }

    const O_RDONLY: c_int = 0x0000;
    const O_RDWR: c_int = 0x0002;
    const O_CREAT: c_int = 0x0200;
    const O_EXCL: c_int = 0x0800;
    const ENOENT: i32 = 2;
    const EPERM: i32 = 1;
    const EACCES: i32 = 13;
    const PROT_READ: c_int = 0x01;
    const PROT_WRITE: c_int = 0x02;
    const MAP_SHARED: c_int = 0x0001;

    /// A ring's place that only one test uses, which removes the lock file and every ring it
    /// named when the test ends, since a shared-memory object outlives the process that made it.
    struct TestPlace {
        place: Place,
    }

    /// Held while a test spawns a reader, and while a test drops a writer and makes the next.
    /// macOS's `posix_spawn` holds the spawning process's descriptors until the child's exec
    /// closes them, so a reader spawned on another test's thread could hold a dropped writer's
    /// lock for that moment.
    static SPAWNS: Mutex<()> = Mutex::new(());

    fn spawns() -> MutexGuard<'static, ()> {
        SPAWNS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

        fn ring_name(&self, generation: u64) -> CString {
            CString::new(format!("{}{generation}", self.place.name_prefix)).unwrap()
        }

        /// The ring the lock file names, opened with `oflag`, or the error that refused it.
        fn open_named(&self, oflag: c_int) -> Result<OwnedFd, i32> {
            let name = CString::new(std::fs::read(self.lock_path()).unwrap()).unwrap();
            let object = unsafe { shm_open(name.as_ptr(), oflag) };
            if object < 0 {
                return Err(std::io::Error::last_os_error().raw_os_error().unwrap());
            }
            Ok(unsafe { OwnedFd::from_raw_fd(object) })
        }

        /// Whether a ring of `generation` exists in the place.
        fn holds(&self, generation: u64) -> bool {
            let object = unsafe { shm_open(self.ring_name(generation).as_ptr(), O_RDONLY) };
            if object < 0 {
                assert_eq!(Some(ENOENT), std::io::Error::last_os_error().raw_os_error());
                return false;
            }
            drop(unsafe { OwnedFd::from_raw_fd(object) });
            true
        }
    }

    impl Drop for TestPlace {
        fn drop(&mut self) {
            for generation in 1..=TEST_GENERATIONS_MAX {
                let name = self.ring_name(generation);
                // A generation the test never made is no object, so its unlink fails.
                if unsafe { shm_unlink(name.as_ptr()) } != 0 {
                    assert_eq!(
                        Some(ENOENT),
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
        let _spawns = spawns();
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

    #[test]
    fn a_reader_behind_the_writer_learns_the_publications_it_missed() {
        // Goal: a reader that falls further behind than the ring holds learns which
        // publications it missed, and reads on from the oldest the ring still holds.
        // Method: open a reader, then publish six ticks into a ring of four slots before it
        // reads.
        let place = TestPlace::new("behind");
        let mut writer = Writer::create(&place.place, Shape { slot_count: 4 }).unwrap();
        let mut reader = Reader::open(place.lock_path()).unwrap();

        for tick in 1..=6 {
            writer.publish(tick * 10).unwrap();
        }
        let found: Vec<Found> = (0..6).map(|_| reader.read().unwrap()).collect();

        assert_eq!(
            vec![
                Found::Missed { first: 1, end: 3 },
                Found::Publication {
                    publication: 3,
                    tick: 30
                },
                Found::Publication {
                    publication: 4,
                    tick: 40
                },
                Found::Publication {
                    publication: 5,
                    tick: 50
                },
                Found::Publication {
                    publication: 6,
                    tick: 60
                },
                Found::Nothing,
            ],
            found
        );
    }

    #[test]
    fn a_reader_opened_late_starts_at_the_oldest_publication_held() {
        // Goal: a reader that opens a ring the writer has filled reads from the oldest
        // publication it holds, and reports no publication missed, since it was not there.
        // Method: publish six ticks into a ring of four slots, then open a reader.
        let place = TestPlace::new("late");
        let mut writer = Writer::create(&place.place, Shape { slot_count: 4 }).unwrap();

        for tick in 1..=6 {
            writer.publish(tick * 10).unwrap();
        }
        let mut reader = Reader::open(place.lock_path()).unwrap();

        assert_eq!(
            Found::Publication {
                publication: 3,
                tick: 30
            },
            reader.read().unwrap()
        );
    }

    #[test]
    fn refuses_a_second_writer_while_the_first_writes() {
        // Goal: one writer at a time, so a second collector never touches the live ring.
        // Method: make a writer, try a second in the same place, then publish with the first and
        // read what it published.
        let place = TestPlace::new("second");
        let mut writer = Writer::create(&place.place, Shape { slot_count: 4 }).unwrap();

        let second = Writer::create(&place.place, Shape { slot_count: 4 });
        writer.publish(5).unwrap();

        assert_eq!(std::io::ErrorKind::WouldBlock, second.err().unwrap().kind());
        let output = read_ring(&place, &["--publications", "1"]);
        assert_eq!("publication 1 tick 5\n", stdout(&output));
    }

    #[test]
    fn a_restarted_writer_continues_the_publications_of_the_previous_ring() {
        // Goal: publication numbers rise across a restart, so a reader tells a new ring's
        // publications from the previous ring's.
        // Method: publish three ticks, drop the writer, make another in the same place and
        // publish once, then read the new ring.
        let place = TestPlace::new("restart");
        let mut first = Writer::create(&place.place, Shape { slot_count: 4 }).unwrap();
        for tick in 1..=3 {
            first.publish(tick).unwrap();
        }
        let spawns = spawns();
        drop(first);
        let mut second = Writer::create(&place.place, Shape { slot_count: 4 }).unwrap();
        drop(spawns);

        second.publish(1).unwrap();
        let output = read_ring(&place, &["--publications", "1"]);

        assert_eq!("publication 4 tick 1\n", stdout(&output));
    }

    #[test]
    fn a_restarted_writer_keeps_the_previous_ring_and_removes_older_ones() {
        // Goal: a restart keeps the ring before it for the persister to seal, and at most two
        // rings exist.
        // Method: make and drop three writers in one place, then look for each generation.
        let place = TestPlace::new("keep");

        let spawns = spawns();
        for _ in 0..3 {
            drop(Writer::create(&place.place, Shape { slot_count: 4 }).unwrap());
        }
        drop(spawns);

        assert!(!place.holds(1));
        assert!(place.holds(2));
        assert!(place.holds(3));
    }

    #[test]
    fn a_writer_passes_over_a_name_it_did_not_make() {
        // Goal: an object under the next ring's name, another user's or another program's,
        // never becomes the ring.
        // Method: make an object under generation 1's name first, then make a writer and read
        // what it publishes.
        let place = TestPlace::new("squat");
        let squatter = unsafe {
            shm_open(
                place.ring_name(1).as_ptr(),
                O_RDWR | O_CREAT | O_EXCL,
                0o600,
            )
        };
        assert!(squatter >= 0);
        drop(unsafe { OwnedFd::from_raw_fd(squatter) });

        let mut writer = Writer::create(&place.place, Shape { slot_count: 4 }).unwrap();
        writer.publish(9).unwrap();
        let output = read_ring(&place, &["--publications", "1"]);

        assert!(place.holds(2));
        assert_eq!("publication 1 tick 9\n", stdout(&output));
    }

    #[test]
    fn only_the_writers_user_reads_the_ring_and_nobody_writes_it() {
        // Goal: another user cannot open the ring, and no process opens it to write, the
        // writer's own user's included. No second user is reachable here, so the mode and the
        // owner stand for the first half.
        // Method: make a writer, read the ring's owner and mode, and open it to write.
        let place = TestPlace::new("mode");
        let _writer = Writer::create(&place.place, Shape { slot_count: 4 }).unwrap();

        let ring = File::from(place.open_named(O_RDONLY).unwrap())
            .metadata()
            .unwrap();
        let lock = std::fs::metadata(place.lock_path()).unwrap();

        assert_eq!(unsafe { getuid() }, ring.uid());
        assert_eq!(0o400, ring.mode() & 0o777);
        assert_eq!(Err(EACCES), place.open_named(O_RDWR).map(drop));
        assert_eq!(0o600, lock.permissions().mode() & 0o777);
    }

    #[test]
    fn a_reader_cannot_map_the_ring_to_write() {
        // Goal: a reader cannot write the ring through its own mapping.
        // Method: open the ring as a reader does, then map it writable, and map it read-only
        // and ask for write access to the mapping.
        let place = TestPlace::new("rdonly");
        let _writer = Writer::create(&place.place, Shape { slot_count: 4 }).unwrap();
        let object = place.open_named(O_RDONLY).unwrap();
        let map = |protection| unsafe {
            mmap(
                std::ptr::null_mut(),
                4096,
                protection,
                MAP_SHARED,
                object.as_raw_fd(),
                0,
            )
        };

        let writable = map(PROT_READ | PROT_WRITE);
        let writable_error = std::io::Error::last_os_error().raw_os_error();
        let readable = map(PROT_READ);
        let protect = unsafe { mprotect(readable, 4096, PROT_READ | PROT_WRITE) };
        let protect_error = std::io::Error::last_os_error().raw_os_error();

        assert_eq!(usize::MAX, writable.addr());
        assert_eq!(Some(EPERM), writable_error);
        assert_ne!(usize::MAX, readable.addr());
        assert_eq!(-1, protect);
        assert_eq!(Some(EACCES), protect_error);
        assert_eq!(0, unsafe { munmap(readable, 4096) });
    }

    #[test]
    fn a_reader_refuses_a_ring_that_anyone_may_write() {
        // Goal: a reader never takes an object for the ring that its mode would let another
        // process have written.
        // Method: write the name of an object of mode 600 into a lock file, then open a reader.
        let place = TestPlace::new("refuse");
        let name = place.ring_name(1);
        let object = unsafe { shm_open(name.as_ptr(), O_RDWR | O_CREAT | O_EXCL, 0o600) };
        assert!(object >= 0);
        drop(unsafe { OwnedFd::from_raw_fd(object) });
        std::fs::write(place.lock_path(), name.as_bytes()).unwrap();

        let opened = Reader::open(place.lock_path());

        assert_eq!(
            std::io::ErrorKind::PermissionDenied,
            opened.err().unwrap().kind()
        );
    }
}
