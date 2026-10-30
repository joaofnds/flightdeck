use std::sync::Arc;

use crate::cancellation::Cancellation;
use crate::live::forged_source::ForgedSource;
use crate::live::identity_fixture::Fixture;
use crate::store::{AppsSecond, ByteBudget};

#[test]
fn an_event_count_refusal_accepts_machine_facts_and_allows_the_next_publication() {
    // Goal: an intrinsic event bound finishes one refused candidate without blocking its successor.
    // Method: retain 16,385 real Exec events, deny acceptance, overwrite them in a copy of the
    // ring whose log claims a whole log past them, as a writer before the event room guard
    // could leave it, then retry B and C.
    let mut fixture = oversized_events_fixture();
    publish_events(&mut fixture, 0, collector::tracker::EVENTS_PER_TICK_MAX + 1);
    publish_events(&mut fixture, 1, 0);
    let mut forged = ForgedSource::new(&fixture.root, fixture.writer.name());
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut denied = 0;

    let read = ring
        .read_prepared(&budget, &operation, None, |_, prepared| {
            assert_event_refusal(prepared);
            denied += 1;
            false
        })
        .unwrap();
    assert_eq!(read, super::NativeRead::Blocked);
    assert_eq!(denied, 1);
    forged.claim_event_newest(
        2 * u64::try_from(collector::tracker::EVENTS_PER_TICK_MAX + 1).unwrap(),
    );
    let mut accepted = Vec::new();
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        assert_eq!(prepared.observation.stamp.publication, 1);
        assert_event_refusal(prepared);
        accepted.push(1);
        true
    })
    .unwrap();
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        assert_eq!(prepared.observation.stamp.publication, 2);
        let machine = prepared.machine.as_ref().unwrap();
        assert!(machine.follows);
        assert_eq!(machine.cpu_percent, Some(50.0));
        assert_eq!(prepared.apps.elapsed_ns, None);
        assert_eq!(prepared.apps.missing, None);
        accepted.push(2);
        true
    })
    .unwrap();

    assert_eq!(accepted, [1, 2]);
    drop((ring, forged, fixture));
    assert_eq!(budget.used(), 0);
}

fn assert_event_refusal(prepared: &super::Prepared) {
    assert!(prepared.machine.is_some());
    assert_eq!(
        prepared.app_refusal(),
        Some(super::Unavailable::Input("store tick events"))
    );
    assert!(matches!(
        prepared.raw(),
        Err(super::Unavailable::UnvalidatedFrame)
    ));
    assert_eq!(
        prepared.apps.missing,
        Some(crate::store::Missing::Refused(
            persistence::Error::Bound("store tick events").to_string()
        ))
    );
}

fn oversized_events_fixture() -> Fixture {
    use collector::ring::{Place, RingReader, Writer};
    let mut fixture = Fixture::new_mode(false, 2, None);
    let shape = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    let writer = Writer::create_recording(
        &Place {
            lock_path: fixture.root.join("events.lock"),
            name_prefix: format!("/fe{}", std::process::id()),
        },
        collector::ring::Shape {
            event_capacity: u64::try_from(collector::tracker::EVENTS_PER_TICK_MAX + 1).unwrap(),
            ..shape
        },
        &fixture.writer.identity().unwrap(),
        0,
    )
    .unwrap();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = writer;
    let mut registry = collector::recording::Registry::read(&fixture.root).unwrap();
    registry.runs[0].ring_name = fixture.writer.name().to_owned();
    registry.write(&fixture.root).unwrap();
    fixture
}

fn publish_events(fixture: &mut Fixture, run_tick: u64, count: usize) {
    let events: Vec<_> = (0..count)
        .map(|_| collector::tracker::Event::Exec { pid: 1 })
        .collect();
    let counter = u32::try_from(run_tick).unwrap();
    fixture.writer.publish_observed(
        &collector::ring::Readings {
            run_tick,
            cpus: &[collector::machine::CPUTicks {
                user: counter,
                idle: counter,
                ..collector::machine::CPUTicks::default()
            }],
            memory: &collector::machine::Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &events,
            dropped: collector::ring::Dropped::default(),
        },
        &collector::ring::ObservationTimes {
            wall_ns: (1000 + run_tick) * 1_000_000_000,
            continuous_ns: (1 + run_tick) * 1_000_000_000,
            awake_ns: (1 + run_tick) * 1_000_000_000,
        },
    );
}

#[test]
fn a_native_identity_keeps_its_charge_without_the_source_or_metadata_owner() {
    // Goal: native App identities own their admission beyond every enclosing second.
    // Method: retain a name from the real prepared producer, then drop source and row owners.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut second = AppsSecond::default();
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        second = std::mem::take(&mut prepared.apps);
        true
    })
    .unwrap();
    let name = Arc::clone(&second.apps[0].name);
    let backing = name.bytes.capacity() + name.display_name.capacity();
    assert!(backing > 0);

    drop((second, ring, fixture));

    assert!(budget.used() >= backing);
    assert!(!name.bytes.is_empty());
    drop(name);
    assert_eq!(budget.used(), 0);
}

#[test]
fn an_oversized_clockless_app_facet_keeps_its_exact_observation() {
    // Goal: an App capacity refusal cannot erase the identity of an accepted native facet.
    // Method: prepare one clockless publication with 2,049 distinct short executable names.
    let mut fixture = distinct_apps_fixture();
    publish_distinct_apps(&mut fixture);
    let identity = fixture.writer.identity().unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut observed = false;

    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        assert!(prepared.machine.is_none());
        assert_eq!(prepared.apps.missing, Some(crate::store::Missing::TooMany));
        assert!(prepared.apps.apps.is_empty());
        assert_eq!(prepared.apps.observation, Some(prepared.observation));
        assert_eq!(prepared.observation.recording, identity.recording.0);
        assert_eq!(prepared.observation.stamp.run, identity.run.0);
        assert_eq!(prepared.observation.stamp.boot, identity.boot.0);
        assert_eq!(prepared.observation.stamp.publication, 1);
        assert_eq!(prepared.observation.run_tick, 0);
        assert_eq!(prepared.observation.wall_ns, None);
        observed = true;
        true
    })
    .unwrap();

    assert!(observed);
    drop((ring, fixture));
    assert_eq!(budget.used(), 0);
}

fn distinct_apps_fixture() -> Fixture {
    use collector::ring::{DictionaryCapacity, Place, RingReader, Writer};
    let count = crate::store::apps::APPS_PER_SECOND_MAX + 1;
    let mut fixture = Fixture::new_mode(true, u64::try_from(count).unwrap(), None);
    let shape = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    let writer = Writer::create_with_identity(
        &Place {
            lock_path: fixture.root.join("distinct.lock"),
            name_prefix: format!("/fdnames{}", std::process::id()),
        },
        shape,
        DictionaryCapacity {
            entries: 4096,
            arena_bytes: 131_072,
        },
        &fixture.writer.identity().unwrap(),
        0,
    )
    .unwrap();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = writer;
    let mut registry = collector::recording::Registry::read(&fixture.root).unwrap();
    registry.runs[0].ring_name = fixture.writer.name().to_owned();
    registry.write(&fixture.root).unwrap();
    fixture
}

fn publish_distinct_apps(fixture: &mut Fixture) {
    use collector::processes::{Process, Task};
    use collector::ring::{Dropped, IdentityPublication, Readings};
    let count = crate::store::apps::APPS_PER_SECOND_MAX + 1;
    let mut processes = Vec::with_capacity(count);
    let mut identities = Vec::with_capacity(count);
    for index in 0..count {
        let pid = i32::try_from(index + 1).unwrap();
        processes.push(Process {
            pid,
            usage: crate::store::startup_tests::usage(0),
            task: Task {
                threads: 1,
                faults: 0,
                context_switches: 0,
                mach_syscalls: 0,
                unix_syscalls: 0,
            },
        });
        identities.push(publish_distinct_apps_identity(fixture, pid));
    }
    fixture
        .writer
        .publish_with_identity(
            &Readings {
                run_tick: 0,
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
                dropped: Dropped::default(),
            },
            None,
            &IdentityPublication {
                processes: &identities,
                events: &[],
            },
        )
        .unwrap();
}

fn publish_distinct_apps_identity(
    fixture: &mut Fixture,
    pid: i32,
) -> collector::ring::IdentityObservation {
    use collector::ring::{
        IdentityObservation, ProcessCreation, ProcessIdentity, ProgramBytes, UsageAvailability,
    };
    let name = format!("p{pid:04}");
    let mut program = None;
    fixture
        .writer
        .intern_program(
            &ProgramBytes {
                executable_name: name.as_bytes(),
                app_path: None,
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
                pid_version: 1,
            },
            program: program.unwrap(),
        },
    }
}
