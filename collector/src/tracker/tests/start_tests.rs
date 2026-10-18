use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super as tests;
use super::super::*;
use crate::programs::{self, sweep::Sweep};
use crate::ring::{
    DictionaryCapacity, Identity, Place, ProcessIdentity, RunIdentity, Shape, Writer,
};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Harness {
    writer: Writer,
    lock_path: PathBuf,
}

impl Harness {
    fn new() -> Self {
        Self::with_entries(4)
    }

    fn with_entries(entries: u64) -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let lock_path =
            std::env::temp_dir().join(format!("fd-start-{}-{number}.lock", std::process::id()));
        let place = Place {
            lock_path: lock_path.clone(),
            name_prefix: format!("/fdst{}.{number}.", std::process::id()),
        };
        let shape = Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 4,
            coalition_capacity: 1,
            event_capacity: 4,
        };
        let identity = RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, number]),
            boot: Identity([4, 5]),
        };
        let writer = Writer::create_with_identity(
            &place,
            shape,
            DictionaryCapacity {
                entries,
                arena_bytes: 1024,
            },
            &identity,
            1,
        )
        .unwrap();

        Self { writer, lock_path }
    }
}

#[test]
fn preserves_the_start_and_coalition_when_its_new_program_exhausts_the_dictionary() {
    // Goal: dictionary refusal leaves the numeric Start and independently read coalition intact.
    // Method: fill a one-entry dictionary, discover a held child, and count capture allocations.
    let mut tracker = tests::tracker(tests::INTERVAL);
    let mut harness = Harness::with_entries(1);
    let mut bytes = None;
    programs::split_program(b"/occupied", &mut bytes).unwrap();
    let mut program = None;
    harness
        .writer
        .intern_program(&bytes.unwrap(), &mut program)
        .unwrap();
    let mut sweep = Sweep::new(4);
    let mut child = tests::ExitChild(tests::spawn_held("/bin/cat", &[]));
    let pid = child.0.id().cast_signed();
    let coalition = coalitions::coalition_of(pid).unwrap();
    let mut creation = Err(programs::CreationError::NotRead);
    programs::read_creation(pid, false, &mut creation);

    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        tracker
            .observe_with_identity(
                &[pid],
                &mut Meter::default(),
                &mut sweep,
                &mut harness.writer,
            )
            .unwrap();
    });
    tests::release(&mut child.0);
    child.0.wait().unwrap();
    let observation = tracker.drain_observations().find(|observation| {
        matches!(observation.event, Event::Start { pid: found, .. } if found == pid)
    }).unwrap();

    assert_eq!(0, allocations);
    assert_eq!(Event::Start { pid, coalition }, observation.event);
    assert_eq!(
        EventCreation::Start(ProcessIdentity::Unavailable {
            creation: Some(creation.unwrap()),
            reason: IdentityUnavailable::Exhausted,
        }),
        observation.identity.creation
    );
    assert_eq!(1, sweep.diagnostics().exhausted);
    eprintln!("exhausted Start pid={pid} rust_heap_calls={allocations}");
}

#[test]
fn preserves_a_known_start_reference_after_the_dictionary_is_full() {
    // Goal: exhaustion does not remove captured program references already in the dictionary.
    // Method: fill the sole entry with cat's path, refuse a new key, and then discover cat.
    let mut tracker = tests::tracker(tests::INTERVAL);
    let mut harness = Harness::with_entries(1);
    let mut sweep = Sweep::new(4);
    let mut child = tests::ExitChild(tests::spawn_held("/bin/cat", &[]));
    let pid = child.0.id().cast_signed();
    let mut scratch = [0; 4096];
    let length = programs::read_path(pid, &mut scratch).unwrap();
    let mut bytes = None;
    programs::split_program(&scratch[..length], &mut bytes).unwrap();
    let mut expected = None;
    harness
        .writer
        .intern_program(bytes.as_ref().unwrap(), &mut expected)
        .unwrap();
    programs::split_program(b"/new", &mut bytes).unwrap();
    let mut refused = None;
    let exhaustion = harness.writer.intern_program(&bytes.unwrap(), &mut refused);

    tracker
        .observe_with_identity(
            &[pid],
            &mut Meter::default(),
            &mut sweep,
            &mut harness.writer,
        )
        .unwrap();
    tests::release(&mut child.0);
    child.0.wait().unwrap();
    let observation = tracker.drain_observations().find(|observation| {
        matches!(observation.event, Event::Start { pid: found, .. } if found == pid)
    }).unwrap();

    assert_eq!(
        Err(crate::ring::IdentityAvailability::Exhausted),
        exhaustion
    );
    let EventCreation::Start(ProcessIdentity::Measured { program, .. }) =
        observation.identity.creation
    else {
        panic!("missing known program");
    };
    assert_eq!(Some(program), expected);
    assert_eq!(1, sweep.diagnostics().measured);
}

#[test]
fn preserves_a_gone_discovery_reason_without_allocating() {
    // Goal: unavailable creation preserves its source reason and still runs independent reads.
    // Method: capture an impossible PID with a counted callback and native allocation guard.
    let mut harness = Harness::new();
    let mut sweep = Sweep::new(4);
    let mut observation = IdentityObservation {
        pid: i32::MAX,
        ..IdentityObservation::EMPTY
    };
    let mut reads = 0;

    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        sweep.capture(&mut observation, &mut harness.writer, || reads += 1);
    });

    assert_eq!(0, allocations);
    assert_eq!(1, reads);
    assert_eq!(
        ProcessIdentity::Unavailable {
            creation: None,
            reason: IdentityUnavailable::Gone,
        },
        observation.identity
    );
    assert_eq!(1, sweep.diagnostics().gone);
    assert_eq!(1, sweep.diagnostics().readiness_unavailable);
    eprintln!("gone discovery rust_heap_calls={allocations}");
}

impl Drop for Harness {
    fn drop(&mut self) {
        crate::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_file(&self.lock_path).unwrap();
    }
}

#[test]
fn retains_discovered_program_after_the_child_has_exited_without_allocating() {
    // Goal: Start retains its verified program before a short-lived process becomes unreadable.
    // Method: discover a held child, reap it before draining, and count native capture allocations.
    let mut tracker = tests::tracker(tests::INTERVAL);
    let mut harness = Harness::new();
    let mut sweep = Sweep::new(4);
    let mut child = tests::ExitChild(tests::spawn_held("/bin/cat", &[]));
    let pid = child.0.id().cast_signed();
    let mut expected = Err(programs::CreationError::NotRead);
    programs::read_creation(pid, false, &mut expected);
    let expected = expected.unwrap();
    let mut scratch = [0; 4096];
    let length = programs::read_path(pid, &mut scratch).unwrap();
    let mut bytes = None;
    programs::split_program(&scratch[..length], &mut bytes).unwrap();

    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        tracker
            .observe_with_identity(
                &[pid],
                &mut Meter::default(),
                &mut sweep,
                &mut harness.writer,
            )
            .unwrap();
    });
    tests::release(&mut child.0);
    child.0.wait().unwrap();
    let observation = tracker.drain_observations().find(|observation| {
        matches!(observation.event, Event::Start { pid: found, .. } if found == pid)
    }).unwrap();
    let mut program = None;
    harness
        .writer
        .intern_program(&bytes.unwrap(), &mut program)
        .unwrap();

    assert_eq!(0, allocations);
    assert_eq!(
        EventCreation::Start(ProcessIdentity::Measured {
            creation: expected,
            program: program.unwrap(),
        }),
        observation.identity.creation
    );
    assert_eq!(1, sweep.diagnostics().measured);
    eprintln!(
        "retained start pid={pid} metadata={:?} rust_heap_calls={allocations}",
        observation.identity
    );
}

#[test]
fn captures_program_during_the_wait_that_discovers_a_start() {
    // Goal: between-tick discovery uses the supplied writer before the scheduled sweep.
    // Method: wait for a held child's Start and inspect its captured metadata and source charge.
    let mut tracker = tests::tracker(Duration::ZERO);
    let mut harness = Harness::new();
    let mut sweep = Sweep::new(4);
    let mut meter = Meter::default();
    let timebase = Timebase::read().unwrap();
    let start = crate::cost::Snapshot::take(&meter, timebase).unwrap();
    let mut child = tests::ExitChild(tests::spawn_held("/bin/cat", &[]));
    let pid = child.0.id().cast_signed();

    let observation = wait_for_start(
        &mut tracker,
        &mut meter,
        &mut sweep,
        &mut harness.writer,
        pid,
    );
    tests::release(&mut child.0);
    child.0.wait().unwrap();
    let cost = crate::cost::Snapshot::take(&meter, timebase)
        .unwrap()
        .cost_since(&start)
        .unwrap();

    assert!(matches!(
        observation.identity.creation,
        EventCreation::Start(ProcessIdentity::Measured { .. })
    ));
    assert!(cost.ms_per_second(Source::StartReads) > 0.0);
    assert!(sweep.observations().is_empty());
    assert!(sweep.diagnostics().measured > 0);
}

fn wait_for_start(
    tracker: &mut Tracker,
    meter: &mut Meter,
    sweep: &mut Sweep,
    writer: &mut Writer,
    pid: i32,
) -> EventObservation {
    for _attempt in 0..tests::STEPS_MAX {
        let deadline = clock::now() + tracker.timebase.ticks(tests::STEP_MS * 1_000_000).unwrap();
        tracker
            .follow_until_with_identity(deadline, meter, sweep, writer)
            .unwrap();
        for observation in tracker.drain_observations() {
            if matches!(observation.event, Event::Start { pid: found, .. } if found == pid) {
                return observation;
            }
        }
    }

    panic!("no Start for owned pid={pid}");
}
