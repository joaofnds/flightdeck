use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::fs::{self, File};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::recording::{
    RegisteredRun, Registry, new_run_identity, private, recording_identity,
};
use collector::ring::{
    DictionaryCapacity, Dropped, Found, IdentityObservation, IdentityPublication, ObservationTimes,
    Place, ProcessCreation, ProcessIdentity, ProgramBuffers, ProgramBytes, ProgramReference,
    Readings, RingReader, Shape, UsageAvailability, Writer,
};
use persistence::{Begin, Limits, Losses, Manifest, Observation, Window, WindowIdentity};

use super::FrozenRecording;

struct Fixture {
    root: PathBuf,
    target: PathBuf,
    writer: Writer,
    program: ProgramReference,
    events: usize,
    wall_ns: Option<u64>,
}

/// A ring of 120 slots with a log of one event more, so a tick of one event fits beside the
/// ticks the ring holds.
const SHAPE: Shape = Shape {
    slot_count: 120,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 1,
    coalition_capacity: 1,
    event_capacity: 121,
};

#[test]
fn reports_captured_publications_instead_of_unfilled_ring_capacity() {
    // Goal: input coverage reports count native facts rather than possible ring slots.
    // Method: capture both an almost-empty ring and a wrapped one through the native reader.
    for publications in [2, 1900] {
        let mut source = Fixture::new();
        for publication in 1..=publications {
            source.publish(publication);
        }
        let frozen = FrozenRecording::capture(&super::Directories {
            source: &source.root,
            target: &source.target,
        })
        .unwrap();

        assert_eq!(frozen.retained_publications(), publications.min(120));
    }
}

#[test]
fn retries_a_failed_eligible_seal_before_completing_replay() {
    // Goal: a recoverable seal failure cannot leave a hidden commit behind a successful replay.
    // Method: block the eligible marker's destination, observe refusal, restore it and retry.
    let mut source = Fixture::new();
    source.populate();
    let frozen = FrozenRecording::capture(&super::Directories {
        source: &source.root,
        target: &source.target,
    })
    .unwrap();
    let destination = marker(frozen.root(), source.writer.identity().unwrap().run, 30);
    frozen.replay_reset(1859).unwrap();
    fs::create_dir(&destination).unwrap();

    assert!(frozen.replay_run(1).is_err());
    assert_eq!(frozen.replay.get(), Some(1860));
    fs::remove_dir(&destination).unwrap();
    frozen.replay_run(1).unwrap();

    assert!(destination.is_file());
    assert_eq!(frozen.replay.get(), Some(1900));
    let registry = Registry::read(frozen.root()).unwrap();
    assert!(
        RingReader::open_generation(&registry.runs[0].ring_name)
            .unwrap()
            .completed()
    );
}

#[test]
fn follows_native_publication_order_after_wall_rollback_with_future_saved_points() {
    // Goal: frozen input expectations follow native order and clip future wall-clock readings.
    // Method: keep a later-clock sealed window, roll the live clock back, and compare the caller.
    let mut source = Fixture::new();
    source.populate();
    for (publication, second) in [(1901, 1000), (1902, 900), (1903, 900)] {
        source.wall_ns = Some(second * 1_000_000_000);
        source.publish(publication);
    }
    let frozen = FrozenRecording::capture(&super::Directories {
        source: &source.root,
        target: &source.target,
    })
    .unwrap();
    let path = source.root.join("rollback.json");
    frozen.expectation(&path).unwrap();
    let expected: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();

    assert_eq!(expected["selected"]["publication"], 1903);
    assert_eq!(expected["selected"]["wall_ns"], 900_000_000_000_u64);
    let mut session = viewer::LiveSession::open(frozen.root()).unwrap();
    session.poll().unwrap();
    let actual = session.follow_latest().unwrap();
    for field in ["run", "boot", "publication", "run_tick", "wall_ns"] {
        assert_eq!(actual["selected"][field], expected["selected"][field]);
    }
    let context: Vec<_> = actual["trends"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| {
            serde_json::json!({"run": point["run"], "boot": point["boot"],
            "publication": point["publication"], "run_tick": point["run_tick"],
            "wall_ns": point["wall_ns"]})
        })
        .collect();
    assert_eq!(context, *expected["observations"].as_array().unwrap());
    assert!(
        context
            .iter()
            .all(|point| point["wall_ns"].as_u64().unwrap() < 901_000_000_000)
    );
}

#[test]
fn starts_new_replay_readers_at_the_validated_retained_input() {
    // Goal: replay metadata never advertises overwritten publications or events to new readers.
    // Method: rewind a wrapped eventful capture and open a reader at its first visible tick.
    let mut source = Fixture::new();
    source.events = 1;
    for publication in 1..=1900 {
        source.publish(publication);
    }
    let frozen = FrozenRecording::capture(&super::Directories {
        source: &source.root,
        target: &source.target,
    })
    .unwrap();
    let before = source.root.join("before.ring");
    frozen.active_ring().export(&before).unwrap();
    frozen.replay_reset(1781).unwrap();
    frozen.replay_step(1781).unwrap();
    let registry = Registry::read(frozen.root()).unwrap();
    let mut reader = RingReader::open_generation(&registry.runs[0].ring_name).unwrap();
    let mut tick = persistence::tick_buffer(&SHAPE).unwrap();

    assert_eq!(reader.next_publication(), 1781);
    assert_eq!(
        reader.read_into(&mut tick).unwrap(),
        Found::Publication {
            publication: 1781,
            tick: 1780
        }
    );
    assert!(matches!(
        reader.read_event().unwrap(),
        collector::ring::FoundEvent::Event { number: 1781, .. }
    ));

    frozen.advance().unwrap();
    let after = source.root.join("after.ring");
    frozen.active_ring().export(&after).unwrap();
    assert_eq!(fs::read(before).unwrap(), fs::read(after).unwrap());
}

#[test]
fn completes_a_finite_paced_replay_and_exposes_its_seal_only_after_eligible_steps() {
    // Goal: the parent can drive arrivals and sealing without a second replay implementation.
    // Method: replay 42 tiny fixture slots at 1 ms, checking state and bounds, never speed.
    let mut source = Fixture::new();
    source.populate();
    let frozen = FrozenRecording::capture(&super::Directories {
        source: &source.root,
        target: &source.target,
    })
    .unwrap();
    let registry = Registry::read(frozen.root()).unwrap();
    let reader = RingReader::open_generation(&registry.runs[0].ring_name).unwrap();
    frozen.replay_reset(1859).unwrap();
    assert!(frozen.replay_run(10_000).is_err());
    assert!(!marker(frozen.root(), registry.runs[0].identity.run, 30).exists());

    frozen.replay_run(1).unwrap();

    assert_eq!(reader.publication_newest().unwrap(), 1900);
    assert!(reader.completed());
    assert!(marker(frozen.root(), registry.runs[0].identity.run, 30).exists());
}

#[test]
fn replays_only_monotone_retained_publications_and_eligible_seals() {
    // Goal: running readers never see a future endpoint, event or seal during replay.
    // Method: reset a captured real ring, step its retained tail, and inspect native readers.
    let mut source = Fixture::new();
    source.populate();
    let frozen = FrozenRecording::capture(&super::Directories {
        source: &source.root,
        target: &source.target,
    })
    .unwrap();
    let registry = Registry::read(frozen.root()).unwrap();
    let mut reader = RingReader::open_generation(&registry.runs[0].ring_name).unwrap();

    frozen.replay_reset(1781).unwrap();
    assert_eq!(reader.publication_newest().unwrap(), 1780);
    assert!(!reader.completed());
    assert!(!marker(frozen.root(), registry.runs[0].identity.run, 30).exists());
    frozen.replay_step(1800).unwrap();
    assert_eq!(reader.publication_newest().unwrap(), 1800);
    let expectation = source.root.join("step.json");
    frozen.expectation(&expectation).unwrap();
    let expectation: serde_json::Value =
        serde_json::from_slice(&fs::read(expectation).unwrap()).unwrap();
    assert_eq!(expectation["selected"]["publication"], 1800);
    assert_eq!(expectation["observations"].as_array().unwrap().len(), 20);
    assert!(frozen.replay_step(1799).is_err());
    assert!(frozen.replay_seal(30).is_err());
    frozen.replay_step(1860).unwrap();
    assert!(!marker(frozen.root(), registry.runs[0].identity.run, 30).exists());
    frozen.replay_seal(30).unwrap();
    assert!(marker(frozen.root(), registry.runs[0].identity.run, 30).exists());
    assert_eq!(reader.publication_newest().unwrap(), 1860);
    assert!(frozen.replay_complete().is_err());
    frozen.replay_step(1900).unwrap();
    frozen.replay_complete().unwrap();
    assert!(reader.completed());
    reader.seek_publication(1900).unwrap();
    assert_latest_row(reader.name(), &source.program);
}

#[test]
fn bounds_event_visibility_and_exports_reader_resolved_identity_bytes() {
    // Goal: replay hides future events and exports independent identities with backing hashes.
    // Method: capture 120 retained eventful slots, step twice, then inspect native event reads.
    let mut source = Fixture::new();
    source.events = 1;
    for publication in 1..=1900 {
        source.publish(publication);
    }
    let frozen = FrozenRecording::capture(&super::Directories {
        source: &source.root,
        target: &source.target,
    })
    .unwrap();
    let export = source.root.join("export");
    frozen.export(&export).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(export.join("input.json")).unwrap()).unwrap();
    let registry = Registry::read(frozen.root()).unwrap();
    frozen.replay_reset(1781).unwrap();
    let mut reader = RingReader::open_generation(&registry.runs[0].ring_name).unwrap();
    reader.seek_event(1781).unwrap();
    assert_eq!(
        reader.read_event().unwrap(),
        collector::ring::FoundEvent::Nothing
    );

    frozen.replay_step(1782).unwrap();

    let mut identity = IdentityObservation::EMPTY;
    assert!(matches!(
        reader.read_event_identity(&mut identity).unwrap(),
        collector::ring::FoundEvent::Event {
            number: 1781,
            tick: 1780,
            ..
        }
    ));
    assert_eq!(identity.pid, 23);
    assert_eq!(
        identity.program().unwrap().number(),
        source.program.number()
    );
    assert!(matches!(
        reader.read_event().unwrap(),
        collector::ring::FoundEvent::Event { number: 1782, .. }
    ));
    assert_eq!(
        reader.read_event().unwrap(),
        collector::ring::FoundEvent::Nothing
    );
    assert_exported_events(&manifest);
    assert_eq!(
        fs::metadata(export.join("0.ring")).unwrap().len(),
        manifest["rings"][0]["backing_bytes"].as_u64().unwrap()
    );
    assert!(
        manifest["rings"][0]["backing_bytes"].as_u64().unwrap()
            >= u64::try_from(reader.mapped_size_bytes()).unwrap()
    );
}

fn assert_exported_events(manifest: &serde_json::Value) {
    let publications = manifest["rings"][0]["publications"].as_array().unwrap();
    assert_eq!(publications.len(), 120);
    assert_eq!(publications[0]["events_first"], 1781);
    assert_eq!(
        publications[0]["events"][0]["identity"]["resolved_bytes"]["name_sha256"],
        "4064876fe25e33f80e717a645098fe5479b8d7119a47a0c9e4c0c696e0da8f09"
    );
}

#[test]
fn refuses_replay_when_retained_slots_reference_overwritten_events() {
    // Goal: an incomplete native sequence is refused rather than replayed with missing events.
    // Method: publish one event a tick, register a copy of the ring whose oldest retained slot
    // names an event the log no longer holds, as a writer that overwrote the events of ticks
    // its ring held left it, and attempt capture.
    let mut source = Fixture::new();
    source.events = 1;
    for publication in 1..=1900 {
        source.publish(publication);
    }
    let forged = ForgedRing::new(&source, 1781, 1779);

    let result = FrozenRecording::capture(&super::Directories {
        source: &source.root,
        target: &source.target,
    });

    assert_eq!(
        result.err().map(|error| error.to_string()),
        Some("native event overwritten or incomplete".to_owned())
    );
    drop(forged);
}

#[test]
fn captures_real_identity_rows_and_restores_covered_and_overwritten_gaps() {
    // Goal: all gaps reuse one real endpoint and leave the original recording untouched.
    // Method: seal two real windows, overwrite the ring, then checkpoint and advance copies.
    let mut source = Fixture::new();
    source.populate();
    let frozen = FrozenRecording::capture(&super::Directories {
        source: &source.root,
        target: &source.target,
    })
    .unwrap();
    assert_eq!(frozen.publication_newest(), 1900);
    assert_eq!(frozen.retained_publications(), 120);
    assert_eq!(frozen.committed_windows(), 2);
    let registry = Registry::read(frozen.root()).unwrap();
    let run = &registry.runs[0];
    let reader = RingReader::open_generation(&run.ring_name).unwrap();
    assert_eq!(reader.identity(), source.writer.identity());
    assert_eq!(reader.source_layout_version(), 4);
    assert_ne!(run.ring_name, source.writer.name());
    assert_eq!(
        reader.dictionary_usage().unwrap(),
        source.writer.dictionary_usage().unwrap()
    );
    source.publish(1901);
    assert_eq!(reader.publication_newest().unwrap(), 1900);
    for gap in [1, 30, 60, 1800] {
        assert_checkpoint(&frozen, run, gap);
        frozen.advance().unwrap();
        assert_eq!(reader.publication_newest().unwrap(), 1900);
        assert!(marker(frozen.root(), run.identity.run, 30).exists());
    }
    assert!(frozen.checkpoint(1901).is_err());
    assert_latest_row(&run.ring_name, &source.program);
    let original = marker(&source.root, run.identity.run, 0);
    let captured = marker(frozen.root(), run.identity.run, 0);
    assert_eq!(
        fs::metadata(&original).unwrap().ino(),
        fs::metadata(&captured).unwrap().ino()
    );
    frozen.checkpoint(1800).unwrap();
    drop(reader);
    drop(frozen);
    assert!(source.target.exists());
    assert!(RingReader::open_generation(&run.ring_name).is_err());
    assert_eq!(
        persistence::read_committed(&source.target, &captured)
            .unwrap()
            .identity
            .window,
        0
    );
    assert_eq!(
        RingReader::open_generation(source.writer.name())
            .unwrap()
            .publication_newest()
            .unwrap(),
        1901
    );
}

fn assert_checkpoint(frozen: &FrozenRecording, run: &RegisteredRun, gap: u64) {
    frozen.checkpoint(gap).unwrap();
    let mut reader = RingReader::open_generation(&run.ring_name).unwrap();
    assert_eq!(reader.publication_newest().unwrap(), 1900 - gap);
    assert_eq!(
        marker(frozen.root(), run.identity.run, 30).exists(),
        gap < 60
    );
    let prefix = marker(frozen.root(), run.identity.run, 0);
    assert_eq!(
        persistence::read_committed(frozen.root(), &prefix)
            .unwrap()
            .publication_end,
        61
    );
    if gap > frozen.retained_publications() {
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        assert!(matches!(
            reader.read_into(&mut tick).unwrap(),
            Found::Missed { .. }
        ));
    }
}

impl Fixture {
    fn new() -> Self {
        let nonce = collector::recording::random_identity().unwrap().0[0];
        let root = std::env::temp_dir().join(format!("flightdeck-frozen-{nonce:016x}"));
        private::create_private_dir(&root).unwrap();
        drop(persistence::Store::open(&root).unwrap());
        let target = root.join("captured");
        let identity = new_run_identity(recording_identity(&root).unwrap()).unwrap();
        let mut writer = Writer::create_with_identity(
            &Place {
                lock_path: root.join("ring.lock"),
                name_prefix: "/ftest".to_owned(),
            },
            SHAPE,
            DictionaryCapacity {
                entries: 1,
                arena_bytes: 128,
            },
            &identity,
            0,
        )
        .unwrap();
        Registry {
            runs: vec![RegisteredRun {
                identity,
                ring_name: writer.name().to_owned(),
                collector_pid: std::process::id(),
            }],
            evictions: 0,
        }
        .write(&root)
        .unwrap();
        let mut program = None;
        writer
            .intern_program(
                &ProgramBytes {
                    executable_name: b"recorded-helper",
                    app_path: Some(b"/Applications/Recorded.app"),
                },
                &mut program,
            )
            .unwrap();
        Self {
            root,
            target,
            writer,
            program: program.unwrap(),
            events: 0,
            wall_ns: None,
        }
    }

    fn populate(&mut self) {
        self.populate_window(0);
        for publication in 61..=1800 {
            self.publish(publication);
        }
        self.populate_window(30);
        for publication in 1861..=1900 {
            self.publish(publication);
        }
    }

    fn populate_window(&mut self, number: u64) {
        let identity = WindowIdentity {
            run: self.writer.identity().unwrap(),
            window: number,
        };
        let mut window = Window::new(identity, Limits::from_shape(&SHAPE).unwrap());
        let mut reader = RingReader::open_generation(self.writer.name()).unwrap();
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        let mut identities = Vec::with_capacity(1);
        reader.seek_publication(number * 60 + 1).unwrap();
        for publication in number * 60 + 1..=number * 60 + 60 {
            self.publish(publication);
            assert!(matches!(
                reader
                    .read_identity_into(&mut tick, &mut identities)
                    .unwrap(),
                Found::Publication { .. }
            ));
            window
                .push_tick_with_identity(
                    &Observation {
                        publication,
                        wall_ns: Some(publication * 1_000_000_000),
                        continuous_ns: Some(publication * 1_000_000_000),
                        awake_ns: Some(publication * 1_000_000_000),
                    },
                    &tick,
                    &[],
                    &persistence::IdentitySource {
                        reader: &reader,
                        processes: &identities,
                        events: &[],
                    },
                )
                .unwrap();
        }
        seal(&self.root, &window);
    }

    fn publish(&mut self, publication: u64) {
        let process = process(publication);
        let observation = IdentityObservation {
            pid: process.pid,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: 23,
                    pid_version: 7,
                },
                program: self.program,
            },
        };
        let events: [_; 2] = std::array::from_fn(|_| collector::tracker::Event::Start {
            pid: 23,
            coalition: Some(17),
        });
        let identities = [IdentityObservation {
            usage: UsageAvailability::NotApplicable,
            ..observation
        }; 2];
        self.writer
            .publish_with_identity(
                &Readings {
                    run_tick: publication - 1,
                    cpus: &[collector::machine::CPUTicks::default()],
                    memory: &Memory::default(),
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes: std::slice::from_ref(&process),
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &events[..self.events],
                    dropped: Dropped::default(),
                },
                Some(&ObservationTimes {
                    wall_ns: self.wall_ns.unwrap_or(publication * 1_000_000_000),
                    continuous_ns: publication * 1_000_000_000,
                    awake_ns: publication * 1_000_000_000,
                }),
                &IdentityPublication {
                    processes: &[observation],
                    events: &identities[..self.events],
                },
            )
            .unwrap();
    }
}

/// A copy of a fixture's ring, registered in its place, whose slot of one publication names an
/// earlier first event than the writer gave it.
struct ForgedRing {
    name: String,
}

impl ForgedRing {
    fn new(source: &Fixture, publication: u64, events_first: u64) -> Self {
        // A version 4 header, then slots of one record of every kind with its identity.
        const HEADER_WORDS: usize = 40;
        const SLOT_WORDS: usize = 43 + 2 + 10 + 1 + 2 + 1 + 18 + 10 + 6;
        const SLOT_SEQUENCE: usize = 0;
        const SLOT_EVENTS_FIRST: usize = 15;
        let name = format!("{}.99", source.writer.name().rsplit_once('.').unwrap().0);
        let bytes = RingReader::open_generation(source.writer.name())
            .unwrap()
            .mapped_size_bytes();
        let original = ForgedMapping::open(source.writer.name(), bytes);
        let copy = ForgedMapping::create(&name, bytes);
        // Owns the copy from here, so a failed check below unlinks it.
        let ring = Self { name };
        let slot = HEADER_WORDS + usize::try_from((publication - 1) % 120).unwrap() * SLOT_WORDS;
        for (target, word) in copy.words().iter().zip(original.words()) {
            target.store(word.load(Ordering::Acquire), Ordering::Release);
        }
        let forged = &copy.words()[slot..slot + SLOT_WORDS];
        assert_eq!(
            forged[SLOT_SEQUENCE].load(Ordering::Acquire),
            2 * publication
        );
        assert_eq!(
            forged[SLOT_EVENTS_FIRST].load(Ordering::Acquire),
            publication
        );
        forged[SLOT_EVENTS_FIRST].store(events_first, Ordering::Release);

        let mut registry = Registry::read(&source.root).unwrap();
        registry.runs[0].ring_name.clone_from(&ring.name);
        registry.write(&source.root).unwrap();
        ring
    }
}

impl Drop for ForgedRing {
    fn drop(&mut self) {
        collector::ring::unlink_generation(&self.name).unwrap();
    }
}

struct ForgedMapping {
    address: *mut c_void,
    bytes: usize,
}

impl ForgedMapping {
    fn open(name: &str, bytes: usize) -> Self {
        let name = CString::new(name).unwrap();
        let descriptor = unsafe { shm_open(name.as_ptr(), O_RDONLY) };
        assert!(descriptor >= 0);
        let file = unsafe { File::from_raw_fd(descriptor) };
        Self::map(&file, bytes, PROT_READ)
    }

    fn create(name: &str, bytes: usize) -> Self {
        let name = CString::new(name).unwrap();
        let descriptor = unsafe { shm_open(name.as_ptr(), O_RDWR | O_CREAT | O_EXCL, RING_MODE) };
        assert!(descriptor >= 0);
        let file = unsafe { File::from_raw_fd(descriptor) };
        assert_eq!(
            unsafe { ftruncate(descriptor, i64::try_from(bytes).unwrap()) },
            0
        );
        Self::map(&file, bytes, PROT_READ | PROT_WRITE)
    }

    fn map(file: &File, bytes: usize, protection: c_int) -> Self {
        let address = unsafe {
            mmap(
                std::ptr::null_mut(),
                bytes,
                protection,
                MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(address.addr(), usize::MAX);
        Self { address, bytes }
    }

    fn words(&self) -> &[AtomicU64] {
        unsafe { std::slice::from_raw_parts(self.address.cast::<AtomicU64>(), self.bytes / 8) }
    }
}

impl Drop for ForgedMapping {
    fn drop(&mut self) {
        assert_eq!(unsafe { munmap(self.address, self.bytes) }, 0);
    }
}

const O_RDONLY: c_int = 0x0000;
const O_RDWR: c_int = 0x0002;
const O_CREAT: c_int = 0x0200;
const O_EXCL: c_int = 0x0800;
const PROT_READ: c_int = 0x01;
const PROT_WRITE: c_int = 0x02;
const MAP_SHARED: c_int = 0x0001;
const RING_MODE: c_uint = 0o400;

unsafe extern "C" {
    fn shm_open(name: *const c_char, flags: c_int, ...) -> c_int;
    fn ftruncate(descriptor: c_int, length: i64) -> c_int;
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        descriptor: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = collector::ring::unlink_generation(self.writer.name()) {
            eprintln!("test fixture: unlink failed: {error}");
        }
        if let Err(error) = fs::remove_dir_all(&self.root) {
            eprintln!("test fixture: remove failed: {error}");
        }
    }
}

fn seal(root: &std::path::Path, window: &Window) {
    let store = persistence::Store::open(root).unwrap();
    let Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
        panic!("test window already committed");
    };
    let tables = persistence::Encoder.encode(window, pending.path()).unwrap();
    let publications = window.publication_range();
    let events = window.event_range();
    store
        .commit(
            pending,
            Manifest {
                identity: window.identity,
                publication_first: publications.start,
                publication_end: publications.end,
                events_first: events.start,
                events_end: events.end,
                wall_ns: Some(persistence::TimeBounds {
                    min: publications.start * 1_000_000_000,
                    max: (publications.end - 1) * 1_000_000_000,
                }),
                continuous_ns: None,
                awake_ns: None,
                losses: Losses::default(),
                data_directory: String::new(),
                tables,
            },
        )
        .unwrap();
}

fn process(publication: u64) -> Process {
    Process {
        pid: 23,
        usage: Usage {
            awake_ns_at_start: 0,
            user_ns: 9_007_199_254_740_993 + publication,
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

fn marker(root: &std::path::Path, run: collector::ring::Identity, window: u64) -> PathBuf {
    persistence::layout::WindowKey { run, window }.marker(root)
}

fn assert_latest_row(name: &str, original_program: &ProgramReference) {
    let mut reader = RingReader::open_generation(name).unwrap();
    let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
    let mut identities = Vec::with_capacity(1);
    reader.seek_publication(1900).unwrap();
    assert_eq!(
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap(),
        Found::Publication {
            publication: 1900,
            tick: 1899
        }
    );
    assert_eq!(tick.processes, vec![process(1900)]);
    let program = identities[0].program().unwrap();
    assert_eq!(program.number(), original_program.number());
    let mut executable_name = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let mut app_path = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let (name_length, app_length) = reader
        .copy_program(
            program,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
        )
        .unwrap();
    assert_eq!(&executable_name[..name_length], b"recorded-helper");
    assert_eq!(&app_path[..app_length], b"/Applications/Recorded.app");
}
