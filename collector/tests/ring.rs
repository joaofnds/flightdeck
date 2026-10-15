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
    use std::sync::{Barrier, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    use collector::coalitions::Coalition;
    use collector::machine::Memory;
    use collector::processes::{Process, Task, Usage};
    use collector::ring::{Dropped, Found, FoundEvent, Place, Reader, Readings, Shape, Writer};
    use collector::tracker::{Event, Noticed};

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

    /// A ring of four slots with room for one record of each kind.
    const SHAPE: Shape = Shape {
        slot_count: 4,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 1,
        coalition_capacity: 1,
        event_capacity: 4,
    };

    /// The readings of a tick of `run_tick` that read nothing else.
    fn readings(run_tick: u64, memory: &Memory) -> Readings<'_> {
        Readings {
            run_tick,
            cpus: &[],
            memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        }
    }

    /// Publishes a tick of `run_tick` that read nothing else.
    fn publish(writer: &mut Writer, run_tick: u64) {
        writer.publish(&readings(run_tick, &Memory::default()));
    }

    fn process(pid: i32) -> Process {
        Process {
            pid,
            usage: Usage {
                awake_ns_at_start: 1,
                user_ns: 2,
                system_ns: 3,
                energy_nj: 4,
                instructions: 5,
                super_instructions: 6,
                cycles: 7,
                idle_wakeups: 8,
                interrupt_wakeups: 9,
                pageins: 10,
                resident_bytes: 11,
                footprint_bytes: 12,
                disk_read_bytes: 13,
                disk_written_bytes: 14,
            },
            task: Task {
                threads: 1,
                faults: 2,
                context_switches: 3,
                mach_syscalls: 4,
                unix_syscalls: 5,
            },
        }
    }

    fn coalition(id: u64) -> Coalition {
        Coalition {
            id,
            tasks_started: 1,
            tasks_exited: 0,
            cpu_ns: 2,
            interrupt_wakeups: 3,
            idle_wakeups: 4,
            disk_read_bytes: 5,
            disk_written_bytes: 6,
            gpu_ns: 7,
            energy_nj: 8,
        }
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
        // number and its readings.
        // Method: publish tick 7 with two processes and a coalition in this process, and read
        // one publication with the reader binary.
        let place = TestPlace::new("first");
        let shape = Shape {
            process_capacity: 2,
            ..SHAPE
        };
        let mut writer = Writer::create(&place.place, shape).unwrap();
        let processes = [process(10), process(20)];
        let coalitions = [coalition(4)];

        writer.publish(&Readings {
            processes: &processes,
            coalitions: &coalitions,
            ..readings(7, &Memory::default())
        });
        let output = read_ring(&place, &["--publications", "1"]);

        assert_eq!(
            "publication 1 tick 7 processes 2 coalitions 1\n",
            stdout(&output)
        );
    }

    #[test]
    fn a_reader_behind_the_writer_learns_the_publications_it_missed() {
        // Goal: a reader that falls further behind than the ring holds learns which
        // publications it missed, and reads on from the oldest the ring still holds.
        // Method: open a reader, then publish six ticks into a ring of four slots before it
        // reads.
        let place = TestPlace::new("behind");
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        let mut reader = Reader::open(place.lock_path()).unwrap();

        for tick in 1..=6 {
            publish(&mut writer, tick * 10);
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
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();

        for tick in 1..=6 {
            publish(&mut writer, tick * 10);
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

    /// Publishes a tick of `run_tick` that drained `events`.
    fn publish_events(writer: &mut Writer, run_tick: u64, events: &[Event]) {
        writer.publish(&Readings {
            events,
            ..readings(run_tick, &Memory::default())
        });
    }

    fn exit(pid: i32) -> Event {
        Event::Exit {
            pid,
            last: Ok(process(pid).usage),
            noticed: Noticed::OnExit,
        }
    }

    fn found(number: u64, tick: u64, event: Event) -> FoundEvent {
        FoundEvent::Event {
            number,
            tick,
            event,
        }
    }

    #[test]
    fn events_reach_a_reader_in_order_under_their_ticks() {
        // Goal: process starts, execs and exits reach a reader in the order the tracker drained
        // them, each under its tick, and each tick names its events.
        // Method: publish a tick with a start and an exec and a tick with an exit, then read
        // each event and the second tick.
        let place = TestPlace::new("events");
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        let mut reader = Reader::open(place.lock_path()).unwrap();
        let start = Event::Start {
            pid: 30,
            coalition: Some(5),
        };

        publish_events(&mut writer, 1, &[start, Event::Exec { pid: 30 }]);
        publish_events(&mut writer, 2, &[exit(30)]);
        let events: Vec<FoundEvent> = (0..4).map(|_| reader.read_event().unwrap()).collect();
        reader.read().unwrap();
        reader.read().unwrap();

        assert_eq!(
            vec![
                found(
                    1,
                    1,
                    Event::Start {
                        pid: 30,
                        coalition: Some(5)
                    }
                ),
                found(2, 1, Event::Exec { pid: 30 }),
                found(3, 2, exit(30)),
                FoundEvent::Nothing,
            ],
            events
        );
        assert_eq!(3..4, reader.tick().events.clone());
    }

    #[test]
    fn a_reader_behind_the_event_log_learns_which_events_it_missed() {
        // Goal: events the log overwrote before a reader read them are missed and counted,
        // never read as later events, and the ticks keep every slot.
        // Method: open a reader, then publish six events into a log of four, two a tick.
        let place = TestPlace::new("evbehind");
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        let mut reader = Reader::open(place.lock_path()).unwrap();

        for tick in 1..=3 {
            let pid = i32::try_from(tick).unwrap();
            publish_events(&mut writer, tick, &[Event::Exec { pid }, exit(pid)]);
        }
        let events: Vec<FoundEvent> = (0..6).map(|_| reader.read_event().unwrap()).collect();

        assert_eq!(
            vec![
                FoundEvent::Missed { first: 1, end: 3 },
                found(3, 2, Event::Exec { pid: 2 }),
                found(4, 2, exit(2)),
                found(5, 3, Event::Exec { pid: 3 }),
                found(6, 3, exit(3)),
                FoundEvent::Nothing,
            ],
            events
        );
    }

    #[test]
    fn a_tick_carries_the_events_the_tracker_dropped() {
        // Goal: a reader learns how many starts, execs and exits the tracker dropped in a tick,
        // since those never reach the log.
        // Method: publish a tick that dropped one start, two execs and three exits, and read it.
        let place = TestPlace::new("dropped");
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        let mut reader = Reader::open(place.lock_path()).unwrap();
        let dropped = Dropped {
            starts: 1,
            execs: 2,
            exits: 3,
        };

        writer.publish(&Readings {
            dropped,
            ..readings(1, &Memory::default())
        });
        reader.read().unwrap();

        assert_eq!(dropped, reader.tick().dropped);
    }

    #[test]
    fn a_restarted_writer_continues_the_events_of_the_previous_ring() {
        // Goal: event numbers rise across a restart, as publication numbers do.
        // Method: publish two events, drop the writer, make another in the same place, publish
        // one event and read it.
        let place = TestPlace::new("evrestart");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        publish_events(
            &mut first,
            1,
            &[Event::Exec { pid: 1 }, Event::Exec { pid: 2 }],
        );
        let spawns = spawns();
        drop(first);
        let mut second = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);
        let mut reader = Reader::open(place.lock_path()).unwrap();

        publish_events(&mut second, 0, &[Event::Exec { pid: 3 }]);

        assert_eq!(
            found(3, 0, Event::Exec { pid: 3 }),
            reader.read_event().unwrap()
        );
    }

    #[test]
    fn a_waiting_reader_wakes_when_the_writer_publishes() {
        // Goal: a reader that waits for the next tick learns of it when the writer publishes,
        // rather than when its wait runs out.
        // Method: a thread opens a reader and waits up to 20 seconds on the empty ring, while
        // this thread publishes 100 milliseconds after the reader opened. The thread times its
        // wait.
        let place = TestPlace::new("wake");
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        let opened = Barrier::new(2);

        let (waited, found) = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                // A reader maps the ring into its own thread, since a mapping is not Send.
                let mut reader = Reader::open(place.lock_path()).unwrap();
                opened.wait();
                let start = Instant::now();
                reader.wait(Duration::from_secs(20)).unwrap();
                (start.elapsed(), reader.read().unwrap())
            });
            opened.wait();
            std::thread::sleep(Duration::from_millis(100));
            publish(&mut writer, 3);
            waiter.join().unwrap()
        });

        assert!(waited < Duration::from_secs(5), "waited {waited:?}");
        assert_eq!(
            Found::Publication {
                publication: 1,
                tick: 3
            },
            found
        );
    }

    #[test]
    fn a_waiting_reader_returns_when_its_wait_runs_out() {
        // Goal: a reader's wait ends at its timeout when the writer publishes nothing, so a
        // reader can look for a newer ring.
        // Method: wait 200 milliseconds on an empty ring, and time the wait.
        let place = TestPlace::new("timeout");
        let _writer = Writer::create(&place.place, SHAPE).unwrap();
        let reader = Reader::open(place.lock_path()).unwrap();

        let start = Instant::now();
        reader.wait(Duration::from_millis(200)).unwrap();
        let waited = start.elapsed();

        assert!(waited >= Duration::from_millis(150), "waited {waited:?}");
        assert!(waited < Duration::from_secs(5), "waited {waited:?}");
    }

    #[test]
    fn refuses_a_second_writer_while_the_first_writes() {
        // Goal: one writer at a time, so a second collector never touches the live ring.
        // Method: make a writer, try a second in the same place, then publish with the first and
        // read what it published.
        let place = TestPlace::new("second");
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();

        let second = Writer::create(&place.place, SHAPE);
        publish(&mut writer, 5);

        assert_eq!(std::io::ErrorKind::WouldBlock, second.err().unwrap().kind());
        let output = read_ring(&place, &["--publications", "1"]);
        assert_eq!(
            "publication 1 tick 5 processes 0 coalitions 0\n",
            stdout(&output)
        );
    }

    #[test]
    fn a_restarted_writer_continues_the_publications_of_the_previous_ring() {
        // Goal: publication numbers rise across a restart, so a reader tells a new ring's
        // publications from the previous ring's.
        // Method: publish three ticks, drop the writer, make another in the same place and
        // publish once, then read the new ring.
        let place = TestPlace::new("restart");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        for tick in 1..=3 {
            publish(&mut first, tick);
        }
        let spawns = spawns();
        drop(first);
        let mut second = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);

        publish(&mut second, 1);
        let output = read_ring(&place, &["--publications", "1"]);

        assert_eq!(
            "publication 4 tick 1 processes 0 coalitions 0\n",
            stdout(&output)
        );
    }

    #[test]
    fn a_reader_learns_that_a_restarted_writer_moved_to_a_newer_ring() {
        // Goal: a reader of a ring whose writer restarted learns that the lock file names a
        // newer ring, and reads on there from the publication after the last it read.
        // Method: read the one publication of a ring, ask whether the ring moved, restart the
        // writer and publish once, ask again, then open the newer ring and read.
        let place = TestPlace::new("moved");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        publish(&mut first, 1);
        let mut reader = Reader::open(place.lock_path()).unwrap();
        reader.read().unwrap();
        let moved_before = reader.moved(place.lock_path()).unwrap();

        let spawns = spawns();
        drop(first);
        let mut second = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);
        publish(&mut second, 1);
        let moved_after = reader.moved(place.lock_path()).unwrap();
        let mut newer = Reader::open(place.lock_path()).unwrap();

        assert!(!moved_before);
        assert!(moved_after);
        assert_eq!(
            Found::Publication {
                publication: 2,
                tick: 1
            },
            newer.read().unwrap()
        );
    }

    #[test]
    fn a_restarted_writer_keeps_the_previous_ring_and_removes_older_ones() {
        // Goal: a restart keeps the ring before it for the persister to seal, and at most two
        // rings exist.
        // Method: make and drop three writers in one place, then look for each generation.
        let place = TestPlace::new("keep");

        let spawns = spawns();
        for _ in 0..3 {
            drop(Writer::create(&place.place, SHAPE).unwrap());
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

        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        publish(&mut writer, 9);
        let output = read_ring(&place, &["--publications", "1"]);

        assert!(place.holds(2));
        assert_eq!(
            "publication 1 tick 9 processes 0 coalitions 0\n",
            stdout(&output)
        );
    }

    #[test]
    fn only_the_writers_user_reads_the_ring_and_nobody_writes_it() {
        // Goal: another user cannot open the ring, and no process opens it to write, the
        // writer's own user's included. No second user is reachable here, so the mode and the
        // owner stand for the first half.
        // Method: make a writer, read the ring's owner and mode, and open it to write.
        let place = TestPlace::new("mode");
        let _writer = Writer::create(&place.place, SHAPE).unwrap();

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
        let _writer = Writer::create(&place.place, SHAPE).unwrap();
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

    #[test]
    fn a_reader_never_returns_a_tick_the_writer_overwrote_during_the_copy() {
        // Goal: a slot the writer rewrites while a reader copies it is dropped, never returned
        // with words of two ticks.
        // Method: a thread publishes ticks as fast as it can into two slots, each tick's
        // processes all carrying the tick's number, while this thread reads until it has read
        // many ticks and missed some, checking every process of every tick it read.
        const PROCESSES: usize = 64;
        const READS: u64 = 200;
        let place = TestPlace::new("torn");
        let shape = Shape {
            slot_count: 2,
            process_capacity: 64,
            ..SHAPE
        };
        let done = std::sync::atomic::AtomicBool::new(false);
        let (created, writer_created) = std::sync::mpsc::channel();

        let (read, missed) = std::thread::scope(|scope| {
            scope.spawn(|| {
                // A writer maps the ring into its own thread, since a mapping is not Send.
                let mut writer = Writer::create(&place.place, shape).unwrap();
                created.send(()).unwrap();
                let memory = Memory::default();
                let mut run_tick = 0;
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    run_tick += 1;
                    let processes = [process(i32::try_from(run_tick).unwrap()); PROCESSES];
                    writer.publish(&Readings {
                        processes: &processes,
                        ..readings(run_tick, &memory)
                    });
                    // A writer that never pauses rewrites a slot faster than a reader copies
                    // it, and the reader then never finishes a copy.
                    std::thread::yield_now();
                }
            });
            writer_created.recv().unwrap();
            let mut reader = Reader::open(place.lock_path()).unwrap();
            let (mut read, mut missed) = (0, 0);
            while read < READS {
                match reader.read().unwrap() {
                    Found::Publication { tick, .. } => {
                        read += 1;
                        for process in &reader.tick().processes {
                            assert_eq!(i64::from(process.pid), i64::try_from(tick).unwrap());
                        }
                    }
                    Found::Missed { first, end } => missed += end - first,
                    Found::Nothing => {}
                }
            }
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            (read, missed)
        });

        assert_eq!(READS, read);
        assert!(missed > 0);
    }
}
