//! Tests of the ring across processes, which run the reader binary as the other process.

// Clippy lets a test unwrap only inside a module marked as a test's.
#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
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
        fn ftruncate(fd: c_int, length: i64) -> c_int;
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

    /// The system's allocator, counting the calls made to it on a thread while that thread
    /// asks it to, so a test can see whether its own thread allocates.
    struct Counting;

    #[global_allocator]
    static COUNTING: Counting = Counting;

    thread_local! {
        static COUNTING_THIS_THREAD: Cell<bool> = const { Cell::new(false) };
        static CALLS_THIS_THREAD: Cell<u64> = const { Cell::new(0) };
    }

    fn count_call() {
        if COUNTING_THIS_THREAD.get() {
            CALLS_THIS_THREAD.set(CALLS_THIS_THREAD.get() + 1);
        }
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count_call();
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count_call();
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            count_call();
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            count_call();
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    /// Returns how many times `work` called the allocator on this thread.
    fn allocator_calls_in(work: impl FnOnce()) -> u64 {
        CALLS_THIS_THREAD.set(0);
        COUNTING_THIS_THREAD.set(true);
        work();
        COUNTING_THIS_THREAD.set(false);
        CALLS_THIS_THREAD.get()
    }

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

    /// Header words the forging tests change, as the ring's layout places them.
    const HEADER_PUBLICATION_FIRST: usize = 3;
    const HEADER_PUBLICATION_NEWEST: usize = 4;

    /// Leaves in `place` a ring of generation 5 that another program of this user forged: a
    /// copy of a ring that published once, with the header words `edits` changed, which the
    /// lock file names. Only the ring's creator may write it, as with a ring a writer makes.
    fn forge(place: &TestPlace, edits: &[(usize, u64)]) {
        let spawns = spawns();
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        publish(&mut writer, 1);
        drop(writer);
        drop(spawns);
        let source = place.open_named(O_RDONLY).unwrap();
        let length = usize::try_from(
            File::from(source.try_clone().unwrap())
                .metadata()
                .unwrap()
                .len(),
        )
        .unwrap();
        let name = place.ring_name(5);
        let forged = unsafe { shm_open(name.as_ptr(), O_RDWR | O_CREAT | O_EXCL, 0o400) };
        assert!(forged >= 0);
        let forged = unsafe { OwnedFd::from_raw_fd(forged) };
        assert_eq!(0, unsafe {
            ftruncate(forged.as_raw_fd(), i64::try_from(length).unwrap())
        });

        let map = |fd: &OwnedFd, protection| unsafe {
            mmap(
                std::ptr::null_mut(),
                length,
                protection,
                MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        let from = map(&source, PROT_READ);
        let to = map(&forged, PROT_READ | PROT_WRITE);
        unsafe {
            std::ptr::copy_nonoverlapping(from.cast::<u8>(), to.cast::<u8>(), length);
            for &(word, value) in edits {
                to.cast::<u64>().add(word).write(value);
            }
            assert_eq!(0, munmap(from, length));
            assert_eq!(0, munmap(to, length));
        }
        std::fs::write(place.lock_path(), name.as_bytes()).unwrap();
    }

    #[test]
    fn a_reader_refuses_a_header_naming_a_publication_past_any_a_writer_numbers() {
        // Goal: a ring whose header names a publication number near the largest a word holds
        // is a broken ring, which the reader reports rather than crashing on.
        // Method: forge a ring whose first and newest publication are the largest number,
        // and open and read it.
        let place = TestPlace::new("huge");
        forge(
            &place,
            &[
                (HEADER_PUBLICATION_FIRST, u64::MAX),
                (HEADER_PUBLICATION_NEWEST, u64::MAX),
            ],
        );

        let opened = Reader::open(place.lock_path());

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            opened.err().unwrap().kind()
        );
    }

    #[test]
    fn a_reader_refuses_a_header_whose_numbering_starts_at_0() {
        // Goal: a ring whose header says its publications start at 0, a number no writer
        // hands out, is a broken ring.
        // Method: forge a ring whose first publication is 0, and open it.
        let place = TestPlace::new("zero");
        forge(&place, &[(HEADER_PUBLICATION_FIRST, 0)]);

        let opened = Reader::open(place.lock_path());

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            opened.err().unwrap().kind()
        );
    }

    #[test]
    fn a_reader_refuses_a_newest_publication_past_any_a_writer_numbers() {
        // Goal: a ring whose newest publication turns huge after the reader opened it is a
        // broken ring, which the reader reports rather than crashing on.
        // Method: forge a ring whose newest publication is the largest number, which open
        // takes as written past the reader's place, and read it.
        let place = TestPlace::new("newest");
        forge(&place, &[(HEADER_PUBLICATION_NEWEST, u64::MAX)]);

        let opened = Reader::open(place.lock_path());

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            opened.err().unwrap().kind()
        );
    }

    #[test]
    fn a_writer_after_a_ring_naming_a_huge_number_numbers_from_1() {
        // Goal: a writer continuing from a ring whose header names a number near the largest
        // a word holds starts the numbering again, rather than overflowing as it counts on.
        // Method: forge a ring whose newest publication is the largest number, start a writer
        // after it, publish, and read.
        let place = TestPlace::new("cont");
        forge(&place, &[(HEADER_PUBLICATION_NEWEST, u64::MAX)]);

        let spawns = spawns();
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);
        publish(&mut writer, 3);
        let mut reader = Reader::open(place.lock_path()).unwrap();

        assert_eq!(
            Found::Publication {
                publication: 1,
                tick: 3
            },
            reader.read().unwrap()
        );
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
        // Method: open a reader, then publish six events into a log of four, two a tick, and
        // read the events, with the number the reader reads next before and after, and then
        // the ticks.
        let place = TestPlace::new("evbehind");
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        let mut reader = Reader::open(place.lock_path()).unwrap();

        for tick in 1..=3 {
            let pid = i32::try_from(tick).unwrap();
            publish_events(&mut writer, tick, &[Event::Exec { pid }, exit(pid)]);
        }
        let next_before = reader.next_event();
        let events: Vec<FoundEvent> = (0..6).map(|_| reader.read_event().unwrap()).collect();
        let next_after = reader.next_event();
        let ticks: Vec<Found> = (0..4).map(|_| reader.read().unwrap()).collect();

        assert_eq!(
            vec![
                Found::Publication {
                    publication: 1,
                    tick: 1
                },
                Found::Publication {
                    publication: 2,
                    tick: 2
                },
                Found::Publication {
                    publication: 3,
                    tick: 3
                },
                Found::Nothing,
            ],
            ticks
        );
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
        assert_eq!((1, 7), (next_before, next_after));
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
    fn a_reader_that_follows_a_restarted_writer_learns_what_it_missed_in_between() {
        // Goal: a reader that moves to the newer ring learns the publications and events the
        // new writer overwrote before the reader got there, rather than skipping them.
        // Method: read the one publication and event of a ring, restart the writer and publish
        // six ticks of one event each into its four slots, follow it, and read.
        let place = TestPlace::new("follow");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        publish_events(&mut first, 1, &[exit(1)]);
        let mut reader = Reader::open(place.lock_path()).unwrap();
        reader.read().unwrap();
        reader.read_event().unwrap();

        let spawns = spawns();
        drop(first);
        let mut second = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);
        for run_tick in 1..=6 {
            publish_events(&mut second, run_tick, &[exit(2)]);
        }
        assert!(reader.follow(place.lock_path()).unwrap());

        assert_eq!(Found::Missed { first: 2, end: 4 }, reader.read().unwrap());
        assert_eq!(
            Found::Publication {
                publication: 4,
                tick: 3
            },
            reader.read().unwrap()
        );
        assert_eq!(
            FoundEvent::Missed { first: 2, end: 4 },
            reader.read_event().unwrap()
        );
    }

    #[test]
    fn a_reader_follows_a_restarted_writer_only_once_it_read_its_own_ring() {
        // Goal: a reader that finds the writer restarted reads the publications its own ring
        // still holds before it moves, and never stays on a ring the writer left.
        // Method: read a ring to its end, publish once more, restart the writer, follow, read,
        // and follow again.
        let place = TestPlace::new("drain");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        publish(&mut first, 1);
        let mut reader = Reader::open(place.lock_path()).unwrap();
        reader.read().unwrap();
        let stays_on_its_writer = reader.follow(place.lock_path()).unwrap();
        publish(&mut first, 2);

        let spawns = spawns();
        drop(first);
        let second = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);
        let follows_before_reading = reader.follow(place.lock_path()).unwrap();
        let found = reader.read().unwrap();
        let follows_after_reading = reader.follow(place.lock_path()).unwrap();

        assert!(!stays_on_its_writer);
        assert!(!follows_before_reading);
        assert_eq!(
            Found::Publication {
                publication: 2,
                tick: 2
            },
            found
        );
        assert!(follows_after_reading);
        drop(second);
    }

    #[test]
    fn a_reader_follows_a_restarted_writer_only_once_it_read_its_own_events() {
        // Goal: a reader that finds the writer restarted reads the events its own ring still
        // holds before it moves, rather than learning them as missed.
        // Method: read a ring's publication but not its event, restart the writer, follow,
        // read the event, and follow again.
        let place = TestPlace::new("evdrain");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        publish_events(&mut first, 1, &[exit(1)]);
        let mut reader = Reader::open(place.lock_path()).unwrap();
        reader.read().unwrap();
        let spawns = spawns();
        drop(first);
        let second = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);

        let follows_before_reading = reader.follow(place.lock_path()).unwrap();
        let found = reader.read_event().unwrap();
        let follows_after_reading = reader.follow(place.lock_path()).unwrap();

        assert!(!follows_before_reading);
        assert!(
            matches!(found, FoundEvent::Event { number: 1, .. }),
            "{found:?}"
        );
        assert!(follows_after_reading);
        drop(second);
    }

    #[test]
    fn a_reader_follows_a_writer_restarted_after_dying_between_a_ticks_events_and_its_slot() {
        // Goal: events a dead writer logged for a tick it never published, which no
        // publication names, do not keep a reader from following the restarted writer.
        // Method: read a ring's publication and its event, let the writer panic in the slot
        // store of a tick whose event it already logged, restart it, and follow.
        let place = TestPlace::new("orphan");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        publish_events(&mut first, 1, &[exit(1)]);
        let mut reader = Reader::open(place.lock_path()).unwrap();
        reader.read().unwrap();
        reader.read_event().unwrap();
        let memory = Memory::default();
        let processes = [process(2), process(3)];
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            first.publish(&Readings {
                events: &[exit(2)],
                processes: &processes,
                ..readings(2, &memory)
            });
        }));
        let spawns = spawns();
        drop(first);
        let second = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);

        let followed = reader.follow(place.lock_path()).unwrap();

        assert!(died.is_err());
        assert!(followed);
        drop(second);
    }

    #[test]
    fn a_reader_stays_put_when_the_ring_the_lock_file_names_is_gone() {
        // Goal: a reader that reads the lock file just before a starting writer removes the
        // ring it names looks again later, rather than failing.
        // Method: read a ring to its end, point the lock file at a ring that does not exist,
        // as a reader sees it in that moment, and follow.
        let place = TestPlace::new("gone");
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        publish(&mut writer, 1);
        let mut reader = Reader::open(place.lock_path()).unwrap();
        reader.read().unwrap();
        let named = std::fs::read(place.lock_path()).unwrap();
        std::fs::write(place.lock_path(), place.ring_name(7).as_bytes()).unwrap();

        let followed = reader.follow(place.lock_path()).unwrap();

        std::fs::write(place.lock_path(), named).unwrap();
        assert!(!followed);
        drop(writer);
    }

    #[test]
    fn a_reader_allocates_nothing_to_follow_or_read_a_newer_ring() {
        // Goal: a reader that follows a restarted writer of the same shape allocates nothing,
        // as Tiger Style's allocation at startup only asks of library code.
        // Method: read a ring, restart the writer and publish, then count the allocator calls
        // this thread makes to follow and read the newer ring.
        let place = TestPlace::new("nalloc");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        publish_events(&mut first, 1, &[exit(1)]);
        let mut reader = Reader::open(place.lock_path()).unwrap();
        reader.read().unwrap();
        reader.read_event().unwrap();
        let spawns = spawns();
        drop(first);
        let mut second = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);
        publish_events(&mut second, 1, &[exit(2)]);
        let lock_path = place.lock_path().clone();
        let mut followed = false;
        let mut found = Found::Nothing;

        let calls = allocator_calls_in(|| {
            followed = reader.follow(&lock_path).unwrap();
            found = reader.read().unwrap();
            reader.read_event().unwrap();
        });

        assert!(followed);
        assert_eq!(
            Found::Publication {
                publication: 2,
                tick: 1
            },
            found
        );
        assert_eq!(0, calls);
    }

    #[test]
    fn a_reader_follows_a_ring_of_other_slot_and_event_counts() {
        // Goal: a reader follows a restarted writer whose ring holds more publications and
        // events, since its room for a tick still fits.
        // Method: read a ring, restart the writer with twice the slots and events, publish,
        // follow, and read.
        let place = TestPlace::new("bigger");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        publish(&mut first, 1);
        let mut reader = Reader::open(place.lock_path()).unwrap();
        reader.read().unwrap();
        let spawns = spawns();
        drop(first);
        let shape = Shape {
            slot_count: 8,
            event_capacity: 8,
            ..SHAPE
        };
        let mut second = Writer::create(&place.place, shape).unwrap();
        drop(spawns);
        publish(&mut second, 1);

        let followed = reader.follow(place.lock_path()).unwrap();

        assert!(followed);
        assert_eq!(
            Found::Publication {
                publication: 2,
                tick: 1
            },
            reader.read().unwrap()
        );
    }

    #[test]
    fn a_reader_refuses_to_follow_a_ring_whose_ticks_hold_more() {
        // Goal: a reader whose room for a tick is too small for the newer ring's ticks says so,
        // without allocating, rather than growing its room mid-run or reading past it.
        // Method: read a ring, restart the writer with room for two processes, and count the
        // allocator calls this thread makes to follow.
        let place = TestPlace::new("wider");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        publish(&mut first, 1);
        let mut reader = Reader::open(place.lock_path()).unwrap();
        reader.read().unwrap();
        let spawns = spawns();
        drop(first);
        let shape = Shape {
            process_capacity: 2,
            ..SHAPE
        };
        let second = Writer::create(&place.place, shape).unwrap();
        drop(spawns);

        let mut followed = Ok(true);
        let calls = allocator_calls_in(|| followed = reader.follow(place.lock_path()));

        assert_eq!(
            std::io::ErrorKind::Unsupported,
            followed.unwrap_err().kind()
        );
        assert_eq!(0, calls);
        drop(second);
    }

    #[test]
    fn a_reader_that_follows_a_writer_numbering_from_1_learns_what_it_missed() {
        // Goal: a reader that moves to a ring whose numbering started again learns the
        // publications the new writer overwrote before the reader got there.
        // Method: read the three publications of a ring, spoil the lock file so the next
        // writer numbers from 1, publish six ticks into its four slots, follow it, and read.
        let place = TestPlace::new("renum");
        let mut first = Writer::create(&place.place, SHAPE).unwrap();
        for run_tick in 1..=3 {
            publish_events(&mut first, run_tick, &[exit(1)]);
        }
        let mut reader = Reader::open(place.lock_path()).unwrap();
        for _ in 1..=3 {
            reader.read().unwrap();
            reader.read_event().unwrap();
        }

        let spawns = spawns();
        drop(first);
        std::fs::write(place.lock_path(), [b'a'; 64]).unwrap();
        let mut second = Writer::create(&place.place, SHAPE).unwrap();
        drop(spawns);
        for run_tick in 1..=6 {
            publish_events(&mut second, run_tick, &[exit(2)]);
        }
        assert!(reader.follow(place.lock_path()).unwrap());

        assert_eq!(Found::Missed { first: 1, end: 3 }, reader.read().unwrap());
        assert_eq!(
            Found::Publication {
                publication: 3,
                tick: 3
            },
            reader.read().unwrap()
        );
        assert_eq!(
            FoundEvent::Missed { first: 1, end: 3 },
            reader.read_event().unwrap()
        );
    }

    #[test]
    fn a_restarted_writer_keeps_the_previous_ring_and_removes_older_ones() {
        // Goal: a restart keeps the ring before it for the persister to seal, and at most two
        // rings exist.
        // Method: make and drop three writers in one place, then look for each generation.
        let place = TestPlace::new("keep");

        let spawns = spawns();
        for run_tick in 1..=3 {
            let mut writer = Writer::create(&place.place, SHAPE).unwrap();
            publish(&mut writer, run_tick);
        }
        drop(spawns);

        assert!(!place.holds(1));
        assert!(place.holds(2));
        assert!(place.holds(3));
    }

    #[test]
    fn a_restart_keeps_the_last_ring_that_holds_publications() {
        // Goal: writers that die before their first publication never cost the ring that
        // holds the last run's publications, and leave no empty ring behind but the newest.
        // Method: make a writer that publishes, then two that publish nothing, as a
        // collector failing at startup under a supervisor would, and look for each generation.
        let place = TestPlace::new("crashes");

        let spawns = spawns();
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        publish(&mut writer, 1);
        drop(writer);
        for _ in 0..2 {
            drop(Writer::create(&place.place, SHAPE).unwrap());
        }
        drop(spawns);

        assert!(place.holds(1));
        assert!(!place.holds(2));
        assert!(place.holds(3));
    }

    #[test]
    fn a_restart_removes_the_rings_a_skipped_generation_left_behind() {
        // Goal: a ring stays removed once two newer rings exist, also where a start passed
        // over a generation another object held.
        // Method: make a writer, make an object under generation 2's name, then make three
        // more writers, which take generations 3 to 5, each publishing once, and look for
        // generations 1 and 2.
        let place = TestPlace::new("skip");
        let spawns = spawns();
        let mut writer = Writer::create(&place.place, SHAPE).unwrap();
        publish(&mut writer, 1);
        drop(writer);
        let squatter = unsafe {
            shm_open(
                place.ring_name(2).as_ptr(),
                O_RDWR | O_CREAT | O_EXCL,
                0o600,
            )
        };
        assert!(squatter >= 0);
        drop(unsafe { OwnedFd::from_raw_fd(squatter) });

        for run_tick in 2..=4 {
            let mut writer = Writer::create(&place.place, SHAPE).unwrap();
            publish(&mut writer, run_tick);
        }
        drop(spawns);

        assert!(!place.holds(1));
        assert!(!place.holds(2));
        assert!(place.holds(4));
        assert!(place.holds(5));
    }

    #[test]
    fn a_lock_file_naming_the_last_generation_is_an_error_and_not_a_crash() {
        // Goal: a lock file another program wrote never crashes the writer that reads it.
        // Method: write the largest generation into the lock file, under a prefix short enough
        // that the name fits a ring name, then make a writer.
        let mut place = TestPlace::new("last");
        place.place.name_prefix = format!("/f{}.", std::process::id());
        let name = format!("{}{}", place.place.name_prefix, u64::MAX);
        std::fs::write(place.lock_path(), name).unwrap();

        let created = Writer::create(&place.place, SHAPE);

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            created.err().unwrap().kind()
        );
    }

    #[test]
    fn a_lock_file_longer_than_a_ring_name_starts_the_numbering_again() {
        // Goal: a lock file another program filled past a ring name's length is read only as
        // far as a ring name goes, and the writer numbers from 1 rather than failing.
        // Method: write a kilobyte into the lock file, then make a writer and look for its
        // first generation.
        let place = TestPlace::new("long");
        std::fs::write(place.lock_path(), [b'a'; 1024]).unwrap();

        drop(Writer::create(&place.place, SHAPE).unwrap());

        assert!(place.holds(1));
    }

    #[test]
    fn a_ring_name_that_would_leave_the_users_directory_is_refused() {
        // Goal: a ring's name never puts its lock file outside this user's temporary
        // directory, where the writer would cut and rewrite a file it does not own.
        // Method: ask for the place of names with a slash, a parent step, a dot and nothing.
        let names = ["../x", "a/b", ".x", ""];

        for name in names {
            assert!(Place::for_user(name).is_err(), "{name:?}");
        }
        assert!(Place::for_user("flight-deck_2").is_ok());
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
        // A bound on the reads keeps a broken copy from hanging the test: the reader gives up
        // after this many tries rather than waiting forever for a whole tick.
        const ATTEMPTS: u64 = 10_000_000;
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
            // The guard stops the writer when this thread leaves the scope, a failed assertion
            // included, so a failure ends the test instead of leaving the writer running.
            let _stop = StopOnDrop(&done);
            let mut reader = Reader::open(place.lock_path()).unwrap();
            let (mut read, mut missed) = (0, 0);
            for _ in 0..ATTEMPTS {
                if read == READS {
                    break;
                }

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
            (read, missed)
        });

        assert_eq!(READS, read);
        assert!(missed > 0);
    }

    /// Stops a writer thread when dropped, so a test that fails still ends.
    struct StopOnDrop<'flag>(&'flag std::sync::atomic::AtomicBool);

    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
