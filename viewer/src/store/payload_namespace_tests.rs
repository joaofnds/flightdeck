use std::sync::Arc;

use collector::processes::{Process, Task};
use collector::ring::{
    IdentityObservation, IdentityPublication, ObservationTimes, ProcessCreation, ProcessIdentity,
    ProgramBytes, Readings, UsageAvailability,
};

use crate::cancellation::{Cancellation, Operation};
use crate::live::identity_fixture::Fixture;
use crate::store::{AppsSecond, ByteBudget, ring::Ring};

#[test]
fn three_cohorts_and_temporary_absence_keep_the_original_name_owner() {
    // Goal: reuse follows the input namespace rather than adjacent process snapshots.
    // Method: retain A/B/C, publish an empty snapshot, and return to A through the real producer.
    let mut fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut seconds = Vec::new();

    for (step, name) in [
        Some(&b"alpha"[..]),
        Some(b"beta"),
        Some(b"gamma"),
        None,
        Some(b"alpha"),
    ]
    .into_iter()
    .enumerate()
    {
        let names: Vec<_> = name.into_iter().collect();
        publish(&mut fixture, u64::try_from(step + 1).unwrap(), &names);
        seconds.push(take(&mut ring, &budget, &operation, seconds.last()));
    }

    assert_eq!(payload(&seconds[3]).len(), 0);
    assert!(std::ptr::eq(
        payload(&seconds[0])[0].executable_name().unwrap(),
        payload(&seconds[4])[0].executable_name().unwrap()
    ));
    assert_eq!(
        payload(&seconds[0])[0].executable_name().unwrap().bytes,
        b"alpha"
    );
    assert_eq!(payload(&seconds[4])[0].creation.unwrap().pid_version, 5);
    let observation = ring.name_observation().unwrap();
    assert_eq!(observation.0, 3);
    println!("{}", observation.1);
    drop((ring, fixture));
    assert_eq!(
        payload(&seconds[0])[0].executable_name().unwrap().bytes,
        b"alpha"
    );
    drop(seconds);
    assert_eq!(budget.used(), 0);
}

#[test]
fn duplicate_long_escaped_names_share_one_admitted_owner_after_source_retirement() {
    // Goal: one exact raw/display value is retained once even within a publication.
    // Method: capture two processes with the same maximal invalid-UTF-8 name and retire input.
    let mut fixture = Fixture::new();
    let name = vec![0xff; collector::ring::PROGRAM_PATH_BYTES_MAX];
    publish(&mut fixture, 1, &[&name, &name]);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();

    let second = take(&mut ring, &budget, &operation, None);

    assert_eq!(payload(&second).len(), 2);
    assert!(std::ptr::eq(
        payload(&second)[0].executable_name().unwrap(),
        payload(&second)[1].executable_name().unwrap()
    ));
    assert_eq!(ring.name_observation().unwrap().0, 1);
    drop((ring, fixture));
    let held = payload(&second)[0].executable_name().unwrap();
    assert_eq!(held.bytes, name);
    assert_eq!(held.display, "\\xff".repeat(name.len()));
    assert!(budget.used() >= held.reserved_bytes());
    drop(second);
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_ready_blocked_candidate_keeps_names_private_and_reuses_its_allocation() {
    // Goal: a retryable Ready candidate cannot install name additions before acceptance.
    // Method: reject the same prepared observation twice, then accept it without recapture.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1, &[b"alpha"]);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut address = 0;
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        address = std::ptr::from_ref(payload(&prepared.apps)[0].executable_name().unwrap()).addr();
        false
    })
    .unwrap();
    let used = budget.used();
    let peak = budget.used_peak();

    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        assert_eq!(
            std::ptr::from_ref(payload(&prepared.apps)[0].executable_name().unwrap()).addr(),
            address
        );
        false
    })
    .unwrap();

    assert_eq!(ring.name_observation().unwrap().0, 0);
    assert_eq!(budget.used(), used);
    assert_eq!(budget.used_peak(), peak);
    let second = take(&mut ring, &budget, &operation, None);
    assert_eq!(ring.name_observation().unwrap().0, 1);
    assert_eq!(
        std::ptr::from_ref(payload(&second)[0].executable_name().unwrap()).addr(),
        address
    );
    drop((second, ring, fixture));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_new_reader_namespace_preserves_old_readers_and_uses_owned_values() {
    // Goal: equal dictionary numbers from different handles cannot alias captured names.
    // Method: replace the source with a new-run dictionary while an old payload stays owned.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1, &[b"alpha"]);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let first = take(&mut ring, &budget, &operation, None);

    replace_reader(&mut fixture);
    publish(&mut fixture, 2, &[b"beta"]);
    ring.resume().unwrap();
    let second = take(&mut ring, &budget, &operation, Some(&first));

    assert_eq!(
        payload(&first)[0].executable_name().unwrap().bytes,
        b"alpha"
    );
    assert_eq!(
        payload(&second)[0].executable_name().unwrap().bytes,
        b"beta"
    );
    assert_eq!(ring.name_observation().unwrap().0, 1);
    drop((ring, fixture));
    assert_eq!(
        payload(&first)[0].executable_name().unwrap().bytes,
        b"alpha"
    );
    drop(first);
    assert_eq!(
        payload(&second)[0].executable_name().unwrap().bytes,
        b"beta"
    );
    drop(second);
    assert_eq!(budget.used(), 0);
}

#[test]
fn sealed_exit_names_survive_when_only_the_inherited_before_has_the_value() {
    // Goal: an exited process retains its captured name across sealed input namespaces.
    // Method: A names alpha; the next sealed minute has only an identity-unavailable Exit.
    let mut fixture = Fixture::new();
    publish(&mut fixture, 1, &[b"alpha"]);
    publish_exit(&mut fixture);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut seconds = Vec::new();

    let outcome =
        crate::store::recording::load_payload(&fixture.root, &budget, &operation, |_, _, apps| {
            let crate::store::apps::TickApps::Loaded(apps) = apps else {
                panic!("complete saved observation");
            };
            seconds.push(std::mem::take(apps));
        })
        .unwrap();

    assert_eq!(outcome.left_out, None);
    assert_eq!(seconds.len(), 2);
    assert_eq!(payload(&seconds[1]).len(), 1);
    assert_eq!(
        payload(&seconds[1])[0].executable_name().unwrap().bytes,
        b"alpha"
    );
    assert_eq!(
        payload(&seconds[1])[0].creation,
        payload(&seconds[0])[0].creation
    );
    assert!(!std::ptr::eq(
        payload(&seconds[0])[0].executable_name().unwrap(),
        payload(&seconds[1])[0].executable_name().unwrap()
    ));
    drop((outcome, fixture));
    assert_eq!(
        payload(&seconds[0])[0].executable_name().unwrap().bytes,
        b"alpha"
    );
    drop(seconds);
    assert_eq!(budget.used(), 0);
}

fn replace_reader(fixture: &mut Fixture) {
    use collector::recording::{RegisteredRun, Registry};
    use collector::ring::{DictionaryCapacity, Place, RingReader, Writer};
    let shape = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    let mut identity = fixture.writer.identity().unwrap();
    identity.run.0[0] ^= 1;
    let writer = Writer::create_with_identity(
        &Place {
            lock_path: fixture.root.join("replacement.lock"),
            name_prefix: format!("/fdnr{}", std::process::id()),
        },
        shape,
        DictionaryCapacity {
            entries: 1,
            arena_bytes: 1024,
        },
        &identity,
        0,
    )
    .unwrap();
    fixture.writer.finish();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = writer;
    let mut registry = Registry::read(&fixture.root).unwrap();
    registry.runs.push(RegisteredRun {
        identity,
        ring_name: fixture.writer.name().to_owned(),
        collector_pid: 0,
    });
    registry.write(&fixture.root).unwrap();
}

fn publish_exit(fixture: &mut Fixture) {
    let event = collector::tracker::Event::Exit {
        pid: 7,
        last: Ok(crate::store::startup_tests::usage(2)),
        noticed: collector::tracker::Noticed::OnExit,
    };
    let identity = IdentityObservation {
        pid: 7,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Unavailable {
            creation: Some(ProcessCreation {
                unique_id: 7,
                pid_version: 1,
            }),
            reason: collector::ring::IdentityUnavailable::Gone,
        },
    };
    fixture
        .writer
        .publish_with_identity(
            &Readings {
                run_tick: 60,
                cpus: &[],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[event],
                dropped: collector::ring::Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: 161_000_000_000,
                continuous_ns: 61_000_000_000,
                awake_ns: 61_000_000_000,
            }),
            &IdentityPublication {
                processes: &[],
                events: &[identity],
            },
        )
        .unwrap();
}

#[test]
fn a_same_run_reader_epoch_retires_its_index_but_preserves_owned_names_and_rates() {
    // Goal: same-run reader replacement starts a fresh value index with the old raw predecessor.
    // Method: restart one Place, reuse dictionary slot one for beta, and retain alpha's payload.
    let fixture = Fixture::new();
    let place = collector::ring::Place {
        lock_path: fixture.root.join("epoch.lock"),
        name_prefix: format!("/fdne{}", std::process::id()),
    };
    let mut generations = Generations(Vec::with_capacity(2));
    let mut writer = generation_writer(&fixture, &place);
    generations.0.push(writer.name().to_owned());
    publish_writer(&mut writer, 1, &[b"alpha"]);
    register_generation(&fixture, writer.name());
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let first = take(&mut ring, &budget, &operation, None);
    assert_eq!(ring.name_observation().unwrap().0, 1);
    drop(writer);

    let mut writer = generation_writer(&fixture, &place);
    generations.0.push(writer.name().to_owned());
    publish_writer(&mut writer, 2, &[b"beta"]);
    register_generation(&fixture, writer.name());
    ring.resume().unwrap();
    let second = take(&mut ring, &budget, &operation, Some(&first));

    assert_eq!(ring.name_observation().unwrap().0, 1);
    assert_eq!(
        first.observation.unwrap().stamp.run,
        second.observation.unwrap().stamp.run
    );
    assert_eq!(second.observation.unwrap().stamp.publication, 2);
    assert_eq!(
        payload(&first)[0].executable_name().unwrap().bytes,
        b"alpha"
    );
    assert_eq!(
        payload(&second)[0].executable_name().unwrap().bytes,
        b"beta"
    );
    assert_eq!(
        payload(&second)[0].cpu_ns(),
        crate::apps::Amount::Known(100_000_000)
    );
    drop((writer, ring, generations, fixture));
    assert_eq!(
        payload(&first)[0].executable_name().unwrap().bytes,
        b"alpha"
    );
    drop((first, second));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_finite_source_keeps_one_owner_per_seen_value_until_its_retirement() {
    // Goal: the chosen input-lifetime tradeoff has an observable finite retained cost.
    // Method: fill four names while releasing previous payloads, then retire the source.
    let mut fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut previous = None;
    let mut owner_bytes = 0;
    let observer = Arc::new(ByteBudget::new(64 * 1024));
    let trace = super::NameTrace::new(4, &observer);

    for (index, name) in [b"alpha".as_slice(), b"beta", b"gamma", b"delta"]
        .iter()
        .enumerate()
    {
        publish(&mut fixture, u64::try_from(index + 1).unwrap(), &[name]);
        let next = take(&mut ring, &budget, &operation, previous.as_ref());
        owner_bytes += payload(&next)[0]
            .executable_name()
            .unwrap()
            .reserved_bytes();
        previous = Some(next);
    }

    let records = trace.finish();
    assert_eq!(records.values().len(), 4);
    for (index, receipt) in records.values().iter().enumerate() {
        assert!(receipt.committed);
        assert_eq!(receipt.index_count, index + 1);
        assert_eq!(receipt.work.new_names, 1);
        assert_eq!(receipt.work.missing_names, 1);
        assert_eq!(receipt.work.allocation_requests, 5);
        assert_eq!(receipt.work.sorts, 1);
        assert_eq!(
            receipt.work.sorted_entries,
            u64::try_from(index + 1).unwrap()
        );
        println!("finite_name_operation={receipt:?}");
    }
    let observation = ring.name_observation().unwrap();
    assert_eq!(observation.0, 4);
    assert_eq!(observation.2, owner_bytes);
    println!(
        "finite_source {} name_owner_bytes={}",
        observation.1, observation.2
    );
    drop((ring, fixture));
    assert_eq!(
        payload(previous.as_ref().unwrap())[0]
            .executable_name()
            .unwrap()
            .bytes,
        b"delta"
    );
    drop((previous, records));
    assert_eq!(budget.used(), 0);
    assert_eq!(observer.used(), 0);
}

struct Generations(Vec<String>);

impl Drop for Generations {
    fn drop(&mut self) {
        assert!(self.0.len() <= 2);
        for name in &self.0 {
            collector::ring::unlink_generation(name).unwrap();
        }
    }
}

fn generation_writer(fixture: &Fixture, place: &collector::ring::Place) -> collector::ring::Writer {
    use collector::ring::{DictionaryCapacity, RingReader, Writer};
    let shape = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    Writer::create_with_identity(
        place,
        shape,
        DictionaryCapacity {
            entries: 2,
            arena_bytes: 1024,
        },
        &fixture.writer.identity().unwrap(),
        0,
    )
    .unwrap()
}

fn register_generation(fixture: &Fixture, name: &str) {
    let mut registry = collector::recording::Registry::read(&fixture.root).unwrap();
    registry.runs[0].ring_name = name.to_owned();
    registry.write(&fixture.root).unwrap();
}

fn payload(second: &AppsSecond) -> &crate::store::Payload {
    second.processes.as_ref().unwrap()
}

fn take(
    ring: &mut Ring,
    budget: &Arc<ByteBudget>,
    operation: &Operation,
    previous: Option<&AppsSecond>,
) -> AppsSecond {
    let mut second = AppsSecond::default();
    ring.read_prepared(budget, operation, previous, |_, prepared| {
        second = std::mem::take(&mut prepared.apps);
        true
    })
    .unwrap();
    second
}

fn publish(fixture: &mut Fixture, step: u64, names: &[&[u8]]) {
    publish_writer(&mut fixture.writer, step, names);
}

fn publish_writer(writer: &mut collector::ring::Writer, step: u64, names: &[&[u8]]) {
    assert!(names.len() <= 2);
    let processes: Vec<_> = (0..names.len())
        .map(|index| Process {
            pid: 7 + i32::try_from(index).unwrap(),
            usage: crate::store::startup_tests::usage(step),
            task: Task {
                threads: 1,
                faults: 0,
                context_switches: 0,
                mach_syscalls: 0,
                unix_syscalls: 0,
            },
        })
        .collect();
    let identities: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| identity(writer, step, 7 + i32::try_from(index).unwrap(), name))
        .collect();
    writer
        .publish_with_identity(
            &Readings {
                run_tick: step - 1,
                cpus: &[],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &processes,
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: collector::ring::Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: (100 + step) * 1_000_000_000,
                continuous_ns: step * 1_000_000_000,
                awake_ns: step * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &identities,
                events: &[],
            },
        )
        .unwrap();
}

fn identity(
    writer: &mut collector::ring::Writer,
    step: u64,
    pid: i32,
    name: &[u8],
) -> IdentityObservation {
    let mut program = None;
    writer
        .intern_program(
            &ProgramBytes {
                executable_name: name,
                app_path: Some(b"/Owner.app"),
            },
            &mut program,
        )
        .unwrap();
    IdentityObservation {
        pid,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: u64::try_from(pid).unwrap(),
                pid_version: i32::try_from(step).unwrap(),
            },
            program: program.unwrap(),
        },
    }
}
