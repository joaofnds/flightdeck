use std::sync::Arc;

use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::store::{AppsSecond, ByteBudget};

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
