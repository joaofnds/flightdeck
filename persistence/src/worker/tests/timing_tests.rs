use super::*;
use crate::allocation_tests::{Observation as Allocations, observe};
use crate::resources::ByteBudget;
use collector::ring::{ObservationTimes, Place, Readings, Writer};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn waits_for_malformed_current_payload_then_refuses_it_when_complete() {
    // Goal: open timing does not certify payload, and staging refuses that same payload.
    // Method: corrupt a real current publication, wait, finish without publishing, then drain.
    let mut source = Scenario::new();
    source.publish(0);
    source.corrupt_payload(1);

    assert!(matches!(source.pass().unwrap(), Progress::Waiting));
    source.ring.0.finish();
    assert_invalid_payload(source.pass());

    assert!(
        source
            .store
            .progress(&source.registered.identity)
            .unwrap()
            .is_none()
    );
    assert_eq!(source.state.losses, Losses::default());
}

#[test]
fn refuses_malformed_boundary_before_sealing_the_earlier_minute() {
    // Goal: full lookahead validation still protects a minute's commit.
    // Method: make the first next-minute publication malformed after a valid earlier reading.
    let mut source = Scenario::new();
    source.publish(0);
    source.publish(60);
    source.corrupt_payload(2);

    assert_invalid_payload(source.pass());

    assert!(
        source
            .store
            .progress(&source.registered.identity)
            .unwrap()
            .is_none()
    );
}

#[test]
fn seals_a_valid_prefix_before_a_later_malformed_newest_payload() {
    // Goal: a valid boundary permits the earlier prefix without committing malformed payload.
    // Method: follow a valid next-minute boundary by a malformed publication in that minute.
    let mut source = Scenario::new();
    source.publish(0);
    source.publish(60);
    source.publish(61);
    source.corrupt_payload(3);

    assert!(matches!(source.pass().unwrap(), Progress::Sealed));

    let saved = source
        .store
        .progress(&source.registered.identity)
        .unwrap()
        .unwrap();
    assert_eq!(saved.identity.window, 0);
    assert_eq!((saved.publication_first, saved.publication_end), (1, 2));
    source.ring.0.finish();
    assert_invalid_payload(source.pass());
    assert!(
        source
            .store
            .existing(&WindowIdentity {
                run: source.registered.identity,
                window: 1,
            })
            .unwrap()
            .is_none()
    );
}

#[test]
fn skips_malformed_old_payload_when_its_minute_is_already_committed() {
    // Goal: committed progress remains authoritative after payload validation moves later.
    // Method: seal, corrupt the saved reading, replay its key and drain the valid successor.
    let mut source = Scenario::new();
    source.publish(0);
    source.publish(60);
    assert!(matches!(source.pass().unwrap(), Progress::Sealed));
    source.corrupt_payload(1);
    source.state.next_publication = 1;

    assert!(matches!(source.pass().unwrap(), Progress::Waiting));

    assert_eq!(source.state.next_publication, 2);
    source.ring.0.finish();
    assert!(matches!(source.pass().unwrap(), Progress::Sealed));
    assert_eq!(
        source
            .store
            .progress(&source.registered.identity)
            .unwrap()
            .unwrap()
            .identity
            .window,
        1
    );
}

#[test]
fn counts_an_overwritten_unread_malformed_publication_as_loss() {
    // Goal: overwritten malformed data is disclosed as loss, never claimed as validated data.
    // Method: corrupt publication one, overwrite it in a four-slot ring, finish and drain.
    let mut source = Scenario::new();
    source.publish(0);
    source.corrupt_payload(1);
    source.publish(1);
    source.publish(2);
    source.publish(3);
    source.publish(4);
    source.state.loaded = true;
    source.state.next_publication = 1;
    source.ring.0.finish();

    assert!(matches!(source.pass().unwrap(), Progress::Sealed));

    let saved = source
        .store
        .progress(&source.registered.identity)
        .unwrap()
        .unwrap();
    assert_eq!((saved.publication_first, saved.publication_end), (2, 6));
    assert_eq!(saved.losses.publications, 1);
    assert_eq!(saved.losses.events, 0);
}

macro_rules! terminal_newest_bypass {
    ($name:ident, $finish:expr, $pid_zero:expr, $retired:expr) => {
        #[test]
        fn $name() {
            // Goal: a known terminal source recovers a valid prefix without reading its newest.
            // Method: corrupt only a later sequence beyond a valid next-minute boundary.
            let mut source = Scenario::new();
            source.publish(0);
            source.publish(60);
            source.publish(61);
            source.change_slot_word(3, 0, 0);
            if $finish {
                source.ring.0.finish();
            }
            if $pid_zero {
                source.registered.collector_pid = 0;
            }

            let result = source.pass_retired($retired).unwrap();

            assert!(matches!(result, Progress::Sealed));
            assert_eq!(
                source
                    .store
                    .progress(&source.registered.identity)
                    .unwrap()
                    .unwrap()
                    .identity
                    .window,
                0
            );
        }
    };
}

terminal_newest_bypass!(
    completed_sources_bypass_corrupt_newest_timing,
    true,
    false,
    false
);
terminal_newest_bypass!(
    pid_zero_sources_bypass_corrupt_newest_timing,
    false,
    true,
    false
);
terminal_newest_bypass!(
    retired_sources_bypass_corrupt_newest_timing,
    false,
    false,
    true
);

#[test]
fn refuses_continuous_time_before_the_run_origin() {
    // Goal: timing-only selection preserves elapsed clock ordering refusal.
    // Method: publish coherent clocks before the source's declared continuous origin.
    let mut source = Scenario::new();
    source.ring.publish(
        0,
        &ObservationTimes {
            continuous_ns: 0,
            ..source.times
        },
    );

    let result = source.pass();

    assert!(matches!(
        result,
        Err(crate::Error::Bound("continuous time order"))
    ));
    assert!(
        source
            .store
            .progress(&source.registered.identity)
            .unwrap()
            .is_none()
    );
}

#[test]
fn rechecks_a_producer_death_without_another_publication() {
    // Goal: a cached publication number cannot keep a dead producer's partial minute open.
    // Method: wait on a real live child, reap it, and run the same source's next pass.
    let mut child = std::process::Command::new("/bin/cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut source = Scenario::new();
    source.registered.collector_pid = child.id();
    source.publish(0);
    let waiting = source.pass();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(matches!(waiting.unwrap(), Progress::Waiting));

    let result = source.pass().unwrap();

    assert!(matches!(result, Progress::Sealed));
    assert_eq!(source.state.next_publication, 2);
    assert_eq!(
        source
            .store
            .progress(&source.registered.identity)
            .unwrap()
            .unwrap()
            .publication_end,
        2
    );
}

#[test]
fn seals_completion_without_another_publication_after_waiting() {
    // Goal: completion remains observable even when publication numbering is unchanged.
    // Method: wait on a valid partial minute, finish the source, then query its next pass.
    let mut source = Scenario::new();
    source.publish(0);
    assert!(matches!(source.pass().unwrap(), Progress::Waiting));
    source.ring.0.finish();

    let result = source.pass().unwrap();

    assert!(matches!(result, Progress::Sealed));
    assert_eq!(source.state.next_publication, 2);
    assert!(matches!(source.pass().unwrap(), Progress::Complete));
}

#[test]
fn skips_a_committed_key_before_interpreting_unused_newest_continuous_time() {
    // Goal: an unused newest clock does not prevent committed-key recovery.
    // Method: seal a key, forge a later before-origin clock and replay only that key's scan step.
    let mut source = Scenario::new();
    source.publish(0);
    source.publish(60);
    assert!(matches!(source.pass().unwrap(), Progress::Sealed));
    source.publish(61);
    source.change_slot_word(3, 40, 0);
    source.state.next_publication = 1;
    let reader = persist_one_open(&source.registered, &mut source.state)
        .unwrap()
        .unwrap();
    let mut scan = scanning::Scanning::new(reader, &source.registered, false).unwrap();

    let result = scan
        .read(&source.store, &source.registered, &mut source.state)
        .unwrap();

    assert!(result.is_none());
    assert_eq!(source.state.next_publication, 2);
}

#[test]
fn refuses_a_persistence_shape_bound_before_waiting_on_an_empty_source() {
    // Goal: avoiding reading buffers does not weaken the tighter persistence admission.
    // Method: create an empty ring above the process staging limit and run the actual worker.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let mut registered = registered(store.recording());
    registered.collector_pid = std::process::id();
    let ring = WaitingRing::new(&root, &mut registered, 65_537);
    let writers = Writers {
        encoder: Encoder,
        derive: &Underived,
    };

    let result = persist_one(
        &store,
        &writers,
        &registered,
        false,
        &mut RunState::default(),
    );

    assert!(matches!(
        result,
        Err(crate::Error::Bound("ring section capacity"))
    ));
    assert!(store.progress(&registered.identity).unwrap().is_none());
    drop(ring);
}

#[test]
fn refuses_same_owner_usage_without_sealing_or_retiring() {
    // Goal: refused producer usage cannot prove that an unchanged owner has terminated.
    // Method: deny only other-process rusage in an isolated child and run the real worker.
    refused_usage_process();
}

fn refused_usage_process() {
    const PARENT: &str = "FLIGHTDECK_TIMING_REFUSED_PARENT";
    const PROFILE: &str = concat!(
        "(version 1)(allow default)(deny process-info-rusage)",
        "(allow process-info-rusage (target self))"
    );
    const TEST: &str =
        "worker::tests::timing_tests::refuses_same_owner_usage_without_sealing_or_retiring";
    if let Some(parent) = std::env::var_os(PARENT) {
        refused_usage_observation(parent.to_str().unwrap().parse().unwrap());
        return;
    }

    let root = Directory::new();
    let output_path = root.0.join("child-output");
    let output = std::fs::File::create(&output_path).unwrap();
    let mut command = std::process::Command::new("/usr/bin/sandbox-exec");
    command
        .args(["-p", PROFILE])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(PARENT, std::process::id().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(output)
        .stderr(std::process::Stdio::inherit());
    let timebase = collector::clock::Timebase::read().unwrap();
    let mut child =
        collector::recording::child::Persister::spawn_command(&mut command, timebase).unwrap();

    assert!(child.finish(Duration::from_secs(20)).unwrap().successful());
    assert!(std::fs::metadata(&output_path).unwrap().len() <= 8_192);
    let output = std::fs::read_to_string(output_path).unwrap();

    print!("{output}");
    assert_eq!(
        output
            .lines()
            .filter(|line| *line == "refused usage observed")
            .count(),
        1
    );
}

fn refused_usage_observation(parent: u32) {
    let mut source = Scenario::new();
    source.publish(0);
    source.registered.collector_pid = parent;
    Registry {
        runs: vec![source.registered.clone()],
        evictions: 0,
    }
    .write(&source.root.0)
    .unwrap();
    let pid = i32::try_from(parent).unwrap();
    let timebase = collector::clock::Timebase::read().unwrap();
    assert!(matches!(
        processes::read_usage(timebase, pid).unwrap(),
        Err(Unreadable::Refused)
    ));
    assert!(!crate::resources::producer_owner_changed(pid).unwrap());
    drop(source.store);

    let result = run(&source.root.0, WORKING_BYTES_MAX, &Underived);

    assert!(matches!(result, Err(crate::Error::Io(error))
        if error.kind() == io::ErrorKind::PermissionDenied));
    let store = Store::open(&source.root.0).unwrap();
    assert!(
        store
            .progress(&source.registered.identity)
            .unwrap()
            .is_none()
    );
    assert!(
        !Registry::completed(
            &source.root.0,
            &source.registered.identity,
            &source.registered.ring_name
        )
        .unwrap()
    );
    let retained = Registry::read(&source.root.0).unwrap();
    assert_eq!(retained.runs.len(), 1);
    assert_eq!(retained.runs[0].identity, source.registered.identity);
    println!("\nrefused usage observed");
}

#[test]
fn preserves_unknown_loss_after_a_registered_object_disappears() {
    // Goal: an absent source earns no receipt and its unknown loss reaches the next seal.
    // Method: unlink an actual registered ring, then drain a completed healthy successor.
    let mut source = Scenario::new();
    source.publish(0);
    let successor_root = Directory::new();
    let mut successor = registered(source.store.recording());
    successor.identity.run = Identity([7, 8]);
    let mut ring = WaitingRing::new(&successor_root, &mut successor, 1);
    ring.publish(0, &source.times);
    ring.0.finish();
    Registry {
        runs: vec![source.registered.clone(), successor.clone()],
        evictions: 0,
    }
    .write(&source.root.0)
    .unwrap();
    drop(source.ring);
    assert!(matches!(
        RingReader::open_generation(&source.registered.ring_name),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    ));
    drop(source.store);

    let result = run(&source.root.0, WORKING_BYTES_MAX, &Underived);

    assert!(matches!(result.unwrap(), Outcome::Complete));
    let store = Store::open(&source.root.0).unwrap();
    assert!(
        store
            .progress(&source.registered.identity)
            .unwrap()
            .is_none()
    );
    let saved = store.progress(&successor.identity).unwrap().unwrap();
    assert_eq!((saved.publication_first, saved.publication_end), (1, 2));
    assert_eq!(
        saved.losses,
        Losses {
            unknown: true,
            ..Losses::default()
        }
    );
    assert!(
        !Registry::completed(
            &source.root.0,
            &source.registered.identity,
            &source.registered.ring_name
        )
        .unwrap()
    );
    let retained = Registry::read(&source.root.0).unwrap();
    assert!(
        retained
            .runs
            .iter()
            .any(|run| run.identity == source.registered.identity)
    );
}

fn assert_invalid_payload(result: Result<Progress>) {
    assert!(matches!(result, Err(crate::Error::Io(error))
        if error.kind() == io::ErrorKind::InvalidData));
}

struct Scenario {
    store: Store,
    registered: RegisteredRun,
    ring: WaitingRing,
    state: RunState,
    writers: Writers<'static>,
    times: ObservationTimes,
    snapshot: Option<WritableSource>,
    root: Directory,
}

impl Scenario {
    fn new() -> Self {
        let root = Directory::new();
        let store = Store::open(&root.0).unwrap();
        let mut registered = registered(store.recording());
        registered.collector_pid = std::process::id();
        let ring = WaitingRing::new(&root, &mut registered, 1);
        Self {
            store,
            registered,
            ring,
            state: RunState::default(),
            writers: Writers {
                encoder: Encoder,
                derive: &Underived,
            },
            times: waiting_times(),
            snapshot: None,
            root,
        }
    }

    fn publish(&mut self, run_tick: u64) {
        self.ring.publish(
            run_tick,
            &ObservationTimes {
                continuous_ns: 1 + run_tick * 1_000_000_000,
                ..self.times
            },
        );
    }

    fn pass(&mut self) -> Result<Progress> {
        self.pass_retired(false)
    }

    fn pass_retired(&mut self, retired: bool) -> Result<Progress> {
        if let Some(snapshot) = self.snapshot.as_ref() {
            snapshot.refresh();
        }
        persist_one(
            &self.store,
            &self.writers,
            &self.registered,
            retired,
            &mut self.state,
        )
    }

    fn corrupt_payload(&mut self, publication: u64) {
        self.change_slot_word(publication, 9, 2);
    }

    fn change_slot_word(&mut self, publication: u64, slot_word: usize, value: u64) {
        // This fixture has a v4 header and one of every base capacity including identity.
        const SLOT_WORDS: usize = 43 + 2 + 10 + 1 + 2 + 1 + 18 + 10 + 6;
        let reader = RingReader::open_generation(self.ring.0.name()).unwrap();
        assert_eq!(reader.source_layout_version(), 4);
        assert_eq!(reader.shape().process_capacity, 1);
        assert_eq!(reader.shape().slot_count, 4);
        let slot = 40 + usize::try_from((publication - 1) % 4).unwrap() * SLOT_WORDS;
        let snapshot = WritableSource::new(
            self.ring.0.name(),
            reader.mapped_size_bytes(),
            ForgedWord {
                publication,
                slot,
                index: slot + slot_word,
                value,
            },
        );
        self.registered.ring_name.clone_from(&snapshot.name);
        self.snapshot = Some(snapshot);
    }
}

struct WritableSource {
    address: *mut std::ffi::c_void,
    original: *mut std::ffi::c_void,
    bytes: usize,
    name: String,
    fault: ForgedWord,
}

struct ForgedWord {
    publication: u64,
    slot: usize,
    index: usize,
    value: u64,
}

impl WritableSource {
    fn new(name: &str, bytes: usize, fault: ForgedWord) -> Self {
        let target = format!("{}.99", name.rsplit_once('.').unwrap().0);
        let name = std::ffi::CString::new(name).unwrap();
        let descriptor = unsafe { shm_open(name.as_ptr(), 0) };
        assert!(descriptor >= 0);
        let object = unsafe { std::fs::File::from_raw_fd(descriptor) };
        let original = unsafe { mmap(std::ptr::null_mut(), bytes, 1, 1, object.as_raw_fd(), 0) };
        assert_ne!(original.addr(), usize::MAX);
        let target_name = std::ffi::CString::new(target.clone()).unwrap();
        let descriptor = unsafe { shm_open(target_name.as_ptr(), 0x602, 0o400) };
        assert!(descriptor >= 0);
        let writable = unsafe { std::fs::File::from_raw_fd(descriptor) };
        assert_eq!(
            unsafe { ftruncate(descriptor, i64::try_from(bytes).unwrap()) },
            0
        );
        let address = unsafe { mmap(std::ptr::null_mut(), bytes, 3, 1, writable.as_raw_fd(), 0) };
        assert_ne!(address.addr(), usize::MAX);
        let snapshot = Self {
            address,
            original,
            bytes,
            name: target,
            fault,
        };
        snapshot.refresh();
        snapshot
    }

    fn refresh(&self) {
        // Frozen malformed states follow real writer publications until their slot is replaced.
        // There is no concurrent writer during this bounded copy or the following worker pass.
        let original = unsafe {
            std::slice::from_raw_parts(self.original.cast::<AtomicU64>(), self.bytes / 8)
        };
        for (target, source) in self.words().iter().zip(original) {
            target.store(source.load(Ordering::Acquire), Ordering::Release);
        }
        if self.words()[self.fault.slot].load(Ordering::Acquire) == 2 * self.fault.publication {
            self.words()[self.fault.index].store(self.fault.value, Ordering::Release);
        }
    }

    fn words(&self) -> &[AtomicU64] {
        unsafe { std::slice::from_raw_parts(self.address.cast::<AtomicU64>(), self.bytes / 8) }
    }
}

impl Drop for WritableSource {
    fn drop(&mut self) {
        assert_eq!(unsafe { munmap(self.address, self.bytes) }, 0);
        assert_eq!(unsafe { munmap(self.original, self.bytes) }, 0);
        collector::ring::unlink_generation(&self.name).unwrap();
    }
}

unsafe extern "C" {
    fn shm_open(name: *const std::ffi::c_char, flags: std::ffi::c_int, ...) -> std::ffi::c_int;
    fn ftruncate(descriptor: std::ffi::c_int, length: i64) -> std::ffi::c_int;
    fn mmap(
        address: *mut std::ffi::c_void,
        length: usize,
        protection: std::ffi::c_int,
        flags: std::ffi::c_int,
        descriptor: std::ffi::c_int,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    fn munmap(address: *mut std::ffi::c_void, length: usize) -> std::ffi::c_int;
}

#[test]
fn empty_waiting_allocation_is_independent_of_process_capacity() {
    // Goal: an empty open source needs no shape-sized reading buffers while waiting.
    // Method: measure actual warmed worker passes over equal inputs in two admitted shapes.
    let times = waiting_times();
    let small = waiting_allocations(1, None, &times);
    let large = waiting_allocations(64, None, &times);

    assert_eq!(small.allocated_bytes, large.allocated_bytes);
    assert_eq!(small.deallocated_bytes, large.deallocated_bytes);
    assert_eq!(small.allocations, large.allocations);
    assert_eq!(small.deallocations, large.deallocations);
    assert_eq!(small.reallocations, large.reallocations);
}

#[test]
fn populated_waiting_allocation_is_independent_of_process_capacity() {
    // Goal: deciding an open minute's readiness does not allocate payload storage.
    // Method: measure actual warmed worker passes with equal publications and larger capacity.
    let times = waiting_times();
    let small = waiting_allocations(1, Some(7), &times);
    let large = waiting_allocations(64, Some(7), &times);

    assert_eq!(small.allocated_bytes, large.allocated_bytes);
    assert_eq!(small.deallocated_bytes, large.deallocated_bytes);
    assert_eq!(small.allocations, large.allocations);
    assert_eq!(small.deallocations, large.deallocations);
    assert_eq!(small.reallocations, large.reallocations);
}

fn waiting_times() -> ObservationTimes {
    ObservationTimes {
        wall_ns: 1_000_000_000_000,
        continuous_ns: 1,
        awake_ns: collector::recording::observation_times(
            collector::clock::Timebase::read().unwrap(),
        )
        .unwrap()
        .awake_ns,
    }
}

fn waiting_allocations(
    process_capacity: u64,
    run_tick: Option<u64>,
    times: &ObservationTimes,
) -> Allocations {
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let mut registered = registered(store.recording());
    registered.collector_pid = std::process::id();
    let mut ring = WaitingRing::new(&root, &mut registered, process_capacity);
    if let Some(run_tick) = run_tick {
        ring.publish(run_tick, times);
    }
    let writers = Writers {
        encoder: Encoder,
        derive: &Underived,
    };
    let mut state = RunState::default();
    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Waiting
    ));
    let budget = ByteBudget::new(1024 * 1024);
    let warm = observe(&budget, || ());
    assert_eq!(warm.1.allocations, 0);
    let (result, allocation) = observe(&budget, || {
        persist_one(&store, &writers, &registered, false, &mut state)
    });

    assert!(matches!(result.unwrap(), Progress::Waiting));
    let admitted = RingReader::open_generation(ring.0.name()).unwrap();
    eprintln!(
        "waiting capacity={process_capacity} tick={run_tick:?} pid={} times={times:?} \
         root_len={} name_len={} calls={} bytes={} frees={} freed={} reallocs={}",
        registered.collector_pid,
        root.0.as_os_str().len(),
        registered.ring_name.len(),
        allocation.allocations,
        allocation.allocated_bytes,
        allocation.deallocations,
        allocation.deallocated_bytes,
        allocation.reallocations,
    );
    eprintln!(
        "waiting layout={} source_extent_bytes={} allocation_layouts={:?}",
        admitted.source_layout_version(),
        admitted.mapped_size_bytes(),
        &allocation.layouts[..allocation.allocations],
    );
    allocation
}

struct WaitingRing(Writer);

impl WaitingRing {
    fn new(root: &Directory, registered: &mut RegisteredRun, process_capacity: u64) -> Self {
        let place = Place {
            lock_path: root.0.join("ring.lock"),
            name_prefix: "/fdwait".to_owned(),
        };
        let shape = Shape {
            slot_count: 4,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity,
            coalition_capacity: 1,
            event_capacity: 4,
        };
        let writer = Writer::create_with_system(
            &place,
            shape,
            collector::ring::DictionaryCapacity {
                entries: 2,
                arena_bytes: 128,
            },
            &registered.identity,
            1,
        )
        .unwrap();
        registered.ring_name = writer.name().to_owned();
        Self(writer)
    }

    fn publish(&mut self, run_tick: u64, times: &ObservationTimes) {
        let mut identity = IdentityObservation::EMPTY;
        publish_readable_identity_process(&mut self.0, &mut identity);
        identity.usage = collector::ring::UsageAvailability::Measured;
        self.0
            .publish_with_disks(
                &Readings {
                    run_tick,
                    cpus: &[],
                    memory: &collector::machine::Memory::default(),
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes: &[waiting_process()],
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[Event::Start {
                        pid: 41,
                        coalition: None,
                    }],
                    dropped: collector::ring::Dropped::default(),
                },
                Some(times),
                &collector::ring::IdentityPublication {
                    processes: &[identity],
                    events: &[IdentityObservation {
                        usage: collector::ring::UsageAvailability::NotApplicable,
                        ..identity
                    }],
                },
                &collector::ring::SupplementalReadings {
                    battery: Some(&collector::battery::BatteryReading::default()),
                    disks: Some(&collector::disks::DiskSnapshot::default()),
                    system: Some(&collector::ring::SystemPublication {
                        reading: &collector::system::SystemReading::default(),
                        machine: &collector::system::MachineFacts::default(),
                        clock: None,
                        process_gpu_ns: &[Some(17)],
                    }),
                },
            )
            .unwrap();
    }
}

fn waiting_process() -> processes::Process {
    processes::Process {
        pid: 41,
        usage: processes::Usage {
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
        task: processes::Task {
            threads: 15,
            faults: 16,
            context_switches: 17,
            mach_syscalls: 18,
            unix_syscalls: 19,
        },
    }
}

impl Drop for WaitingRing {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.0.name()).unwrap();
    }
}
