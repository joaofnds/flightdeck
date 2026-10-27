use super::tests::view_where;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::{Metric, Rankable};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Generations([Option<String>; 2]);

impl Generations {
    fn keep(&mut self, writer: &collector::ring::Writer) {
        let slot = self.0.iter_mut().find(|name| name.is_none()).unwrap();
        *slot = Some(writer.name().to_owned());
    }
}

impl Drop for Generations {
    fn drop(&mut self) {
        for name in self.0.iter().flatten() {
            collector::ring::unlink_generation(name).unwrap();
        }
    }
}

#[test]
fn a_contiguous_sealed_to_ring_handoff_qualifies_the_published_view() {
    // Goal: a View distinguishes a complete ring handoff from merely having some readings.
    // Method: load one real sealed publication followed by its unsealed ring successor.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.seal_minute(0);
    fixture.publish_with_events(10_001, &[]);

    let store = super::Store::start_with_payload(&fixture.root, 64 * 1024 * 1024).unwrap();
    let view = view_where(&store, |view| view.seconds.len() == 2);

    assert_eq!(view.failure, None);
    assert_eq!(view.left_out, None);
    assert!(view.ring_complete);
    assert_eq!(view.apps[1].observation.unwrap().stamp.publication, 2);
}

#[test]
fn an_initially_overwritten_prefix_stays_unqualified_after_catching_up() {
    // Goal: a reader starting after lost publications cannot call its retained suffix complete.
    // Method: overwrite four unsealed ticks before startup, then advance the caught-up reader.
    let mut fixture = Fixture::new();
    for second in 10_000..10_012 {
        fixture.publish_with_events(second, &[]);
    }
    let store = super::Store::start_with_payload(&fixture.root, 64 * 1024 * 1024).unwrap();

    let initial = view_where(&store, |view| view.seconds.len() == 8);

    assert!(!initial.ring_complete);
    assert_eq!(initial.seconds[0].publication, 5);
    fixture.publish_with_events(10_012, &[]);
    let later = view_where(&store, |view| view.seconds.len() == 9);
    assert!(!later.ring_complete);
    assert!(!initial.ring_complete);
}

#[test]
fn a_matching_sealed_prefix_qualifies_an_initially_wrapped_ring() {
    // Goal: a sealed handoff explains publications no longer retained in the ring.
    // Method: seal four ticks before eight successors replace them in the real ring.
    let mut fixture = Fixture::new();
    for second in 10_000..10_004 {
        fixture.publish_with_events(second, &[]);
    }
    fixture.seal_minute(0);
    for second in 10_004..10_012 {
        fixture.publish_with_events(second, &[]);
    }

    let store = super::Store::start_with_payload(&fixture.root, 64 * 1024 * 1024).unwrap();
    let view = view_where(&store, |view| view.seconds.len() == 12);

    assert!(view.ring_complete);
    assert_eq!(view.seconds[0].publication, 1);
    assert_eq!(view.seconds[11].publication, 12);
}

#[test]
fn an_overwrite_after_attachment_stays_unqualified_at_the_newest_cursor() {
    // Goal: catching up cannot erase a publication gap discovered after the handoff was checked.
    // Method: overwrite terminal plus one after attaching, then advance the same reader again.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.seal_minute(0);
    fixture.publish_with_events(10_001, &[]);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = super::Held::new();
    held.enable_payload(Arc::new(super::ByteBudget::new(64 * 1024 * 1024)));
    let mut ring = super::Ring::new(fixture.root.clone());
    super::startup(&fixture.root, &mut held, &mut ring, &operation).unwrap();
    ring.follow().unwrap();
    for second in 10_002..10_010 {
        fixture.publish_with_events(second, &[]);
    }

    let initial = stepped_view(&mut held, &mut ring, &operation);

    assert!(!initial.ring_complete);
    let publications: Vec<_> = initial
        .seconds
        .iter()
        .map(|second| second.publication)
        .collect();
    assert_eq!(publications, [1, 3, 4, 5, 6, 7, 8, 9, 10]);
    fixture.publish_with_events(10_010, &[]);
    let later = stepped_view(&mut held, &mut ring, &operation);
    assert!(!later.ring_complete);
    assert!(!initial.ring_complete);
}

#[test]
fn a_missing_replacement_generation_republishes_unqualified_metadata() {
    // Goal: retained old-generation readings cannot qualify as the newest registered source.
    // Method: replace only the registry name, then require a new View with identical facts.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.seal_minute(0);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = super::Held::new();
    held.enable_payload(Arc::new(super::ByteBudget::new(64 * 1024 * 1024)));
    let mut ring = super::Ring::new(fixture.root.clone());
    super::startup(&fixture.root, &mut held, &mut ring, &operation).unwrap();
    let initial = stepped_view(&mut held, &mut ring, &operation);
    assert!(initial.ring_complete);
    register_ring(&fixture, &format!("{}x", fixture.writer.name()));

    ring.resume().unwrap();
    let replacement = stepped_view(&mut held, &mut ring, &operation);

    assert!(!replacement.ring_complete);
    assert!(replacement.version > initial.version);
    assert_eq!(replacement.seconds, initial.seconds);
    assert_eq!(replacement.apps, initial.apps);
    assert!(initial.ring_complete);
}

#[test]
fn a_new_generation_of_the_same_run_preserves_the_readers_predecessor() {
    // Goal: a generation change follows its registered name without losing consecutive rates.
    // Method: restart a real writer in the same Place and check publication two and CPU rate.
    let fixture = Fixture::new();
    let identity = fixture.writer.identity().unwrap();
    let shape = collector::ring::RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    let place = generation_place(&fixture, "replacement.lock");
    let mut generations = Generations::default();
    let mut writer = generation_writer(&place, shape, &identity);
    generations.keep(&writer);
    publish_generation_app_tick(&mut writer, 0);
    register_ring(&fixture, writer.name());
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = super::Held::new();
    let mut ring = super::Ring::new(fixture.root.clone());
    let initial = stepped_view(&mut held, &mut ring, &operation);
    assert!(initial.ring_complete);
    drop(writer);
    let mut writer = generation_writer(&place, shape, &identity);
    generations.keep(&writer);
    publish_generation_app_tick(&mut writer, 1);
    register_ring(&fixture, writer.name());

    ring.resume().unwrap();
    let replacement = stepped_view(&mut held, &mut ring, &operation);

    assert_eq!(replacement.seconds.len(), 2);
    assert_eq!(replacement.seconds[1].publication, 2);
    assert_eq!(replacement.seconds[1].cpu_percent, Some(50.0));
    assert!(replacement.seconds[1].follows);
    assert_eq!(replacement.apps[1].elapsed_ns, Some(1_000_000_000));
    assert_eq!(replacement.apps[1].apps.len(), 1);
    let app = &replacement.apps[1].apps[0];
    assert_eq!(app.name.bytes, b"/Tools.app");
    assert_eq!(app.measure(Metric::Cpu).known, 100_000_000);
    assert_eq!(app.measure(Metric::Cpu).measured_members, 1);
    assert!(replacement.ring_complete);
    assert_eq!(initial.seconds.len(), 1);
    drop(writer);
    drop(ring);
}

#[test]
fn an_unplaced_tick_cannot_qualify_older_held_readings() {
    // Goal: an unsupported clock cannot make older displayed facts look caught up.
    // Method: consume one known tick, then publish a real legacy tick without observed clocks.
    let mut fixture = Fixture::new_mode(false, 2, None);
    fixture.publish_with_events(10_000, &[]);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = super::Held::new();
    let mut ring = super::Ring::new(fixture.root.clone());
    let initial = stepped_view(&mut held, &mut ring, &operation);
    assert!(initial.ring_complete);

    with_generation_tick(1, |readings| fixture.writer.publish(&readings));
    let replacement = stepped_view(&mut held, &mut ring, &operation);

    assert!(!replacement.ring_complete);
    assert!(replacement.version > initial.version);
    assert_eq!(replacement.seconds, initial.seconds);
    assert!(initial.ring_complete);
}

#[test]
fn a_new_run_waits_until_the_old_pending_publication_is_accepted() {
    // Goal: a registry change cannot pair an old pending machine tick with another run's Apps.
    // Method: refuse a real first-run tick, register a second run, then accept each in order.
    let mut first = Fixture::new();
    first.publish_with_events(10_000, &[]);
    let first_identity = first.writer.identity().unwrap();
    let mut ring = super::Ring::new(first.root.clone());
    ring.follow().unwrap();
    ring.read(|_, _, _| false).unwrap();
    let mut second = Fixture::new_mode(false, 2, Some(&first));
    second.publish_with_events(10_001, &[]);
    let second_identity = second.writer.identity().unwrap();
    let mut accepted = Vec::new();

    ring.resume().unwrap();
    ring.read(|run, machine, apps| {
        let super::TickApps::Derived(sample) = apps else {
            panic!("the first run's identity sample is still owned");
        };
        assert_eq!(sample.selected.stamp.run, first_identity.run.0);
        accepted.push((*run, machine.publication));
        true
    })
    .unwrap();

    assert_eq!(accepted, [(first_identity, 1)]);
    ring.resume().unwrap();
    ring.read(|run, machine, _| {
        accepted.push((*run, machine.publication));
        true
    })
    .unwrap();
    assert_eq!(accepted, [(first_identity, 1), (second_identity, 1)]);
}

#[test]
fn a_restarted_publication_sequence_cannot_reuse_a_registered_run_identity() {
    // Goal: a new name cannot reuse an accepted identity with contradictory publication clocks.
    // Method: register a second fresh Place under the same run and require explicit refusal.
    let fixture = Fixture::new();
    let identity = fixture.writer.identity().unwrap();
    let shape = collector::ring::RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    let mut generations = Generations::default();
    let original = generation_place(&fixture, "original.lock");
    let mut writer =
        collector::ring::Writer::create_recording(&original, shape, &identity, 0).unwrap();
    generations.keep(&writer);
    publish_generation_tick(&mut writer, 0);
    register_ring(&fixture, writer.name());
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = super::Held::new();
    let mut ring = super::Ring::new(fixture.root.clone());
    let initial = stepped_view(&mut held, &mut ring, &operation);
    assert!(initial.ring_complete);
    drop(writer);
    let restarted = generation_place(&fixture, "restarted.lock");
    let mut writer =
        collector::ring::Writer::create_recording(&restarted, shape, &identity, 0).unwrap();
    generations.keep(&writer);
    publish_generation_tick(&mut writer, 1);
    register_ring(&fixture, writer.name());

    let result = ring.resume();

    assert_eq!(result, Err(super::Failure::ChangedIdentity));
    let refused = stepped_view(&mut held, &mut ring, &operation);
    assert!(!refused.ring_complete);
    assert_eq!(refused.failure, Some(super::Failure::ChangedIdentity));
    assert_eq!(refused.seconds, initial.seconds);
    assert!(initial.ring_complete);
}

#[test]
fn a_completion_read_failure_stays_unqualified_after_the_header_recovers() {
    // Goal: a transient completion read failure cannot silently restore continuity.
    // Method: fail the header I/O seam once, then read the unchanged real ring successfully.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = super::Held::new();
    let mut ring = super::Ring::new(fixture.root.clone());
    let initial = stepped_view(&mut held, &mut ring, &operation);
    assert!(initial.ring_complete);

    let failed =
        ring.caught_up_read(|_| Err(std::io::Error::from(std::io::ErrorKind::InvalidData)));

    assert_eq!(
        failed,
        Err(super::Failure::Ring(std::io::ErrorKind::InvalidData))
    );
    assert!(!ring.complete(&fixture.writer.identity().unwrap().recording));
    let recovered = stepped_view(&mut held, &mut ring, &operation);
    assert_eq!(recovered.failure, None);
    assert!(!recovered.ring_complete);
    assert_eq!(recovered.seconds, initial.seconds);
    assert!(initial.ring_complete);
}

#[test]
fn a_recording_identity_change_invalidates_the_registered_generation() {
    // Goal: an old ring cannot qualify after its recording root changes identity.
    // Method: retain the registry and ring, replace only the root identity, then resume.
    let foreign = Fixture::new();
    changed_recording(|root| {
        std::fs::copy(foreign.root.join("identity"), root.join("identity")).unwrap();
        super::Failure::ForeignRecording
    });
}

#[test]
fn a_missing_recording_identity_invalidates_the_registered_generation() {
    // Goal: an unverifiable recording root cannot keep its previously qualified metadata.
    // Method: retain the registry and ring, remove only the root identity, then resume.
    changed_recording(|root| {
        std::fs::remove_file(root.join("identity")).unwrap();
        super::Failure::Recording(std::io::ErrorKind::NotFound)
    });
}

fn changed_recording(change: impl FnOnce(&std::path::Path) -> super::Failure) {
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = super::Held::new();
    let mut ring = super::Ring::new(fixture.root.clone());
    let initial = stepped_view(&mut held, &mut ring, &operation);
    assert!(initial.ring_complete);
    let failure = change(&fixture.root);

    assert_eq!(ring.resume(), Err(failure));
    let refused = stepped_view(&mut held, &mut ring, &operation);

    assert!(!refused.ring_complete);
    assert_eq!(refused.failure, Some(failure));
    assert_eq!(refused.seconds, initial.seconds);
    assert_eq!(refused.apps, initial.apps);
    assert!(initial.ring_complete);
}

#[test]
fn a_new_run_waits_until_the_old_unread_publications_are_accepted() {
    // Goal: a registry change cannot discard old unread publications without a pending tick.
    // Method: attach without reading, register a second run, and accept both runs in order.
    let mut first = Fixture::new();
    first.publish_with_events(10_000, &[]);
    first.publish_with_events(10_001, &[]);
    let first_identity = first.writer.identity().unwrap();
    let mut ring = super::Ring::new(first.root.clone());
    ring.follow().unwrap();
    let mut second = Fixture::new_mode(false, 2, Some(&first));
    second.publish_with_events(10_002, &[]);
    let second_identity = second.writer.identity().unwrap();
    let mut accepted = Vec::new();

    ring.resume().unwrap();
    ring.read(|run, machine, _| {
        accepted.push((*run, machine.publication));
        true
    })
    .unwrap();

    assert_eq!(accepted, [(first_identity, 1), (first_identity, 2)]);
    ring.resume().unwrap();
    ring.read(|run, machine, _| {
        accepted.push((*run, machine.publication));
        true
    })
    .unwrap();
    assert_eq!(
        accepted,
        [
            (first_identity, 1),
            (first_identity, 2),
            (second_identity, 1)
        ]
    );
}

fn generation_place(fixture: &Fixture, lock: &str) -> collector::ring::Place {
    collector::ring::Place {
        lock_path: fixture.root.join(lock),
        name_prefix: format!(
            "/fdg{}x{}",
            std::process::id(),
            NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
        ),
    }
}

fn register_ring(fixture: &Fixture, name: &str) {
    collector::recording::Registry {
        runs: vec![collector::recording::RegisteredRun {
            identity: fixture.writer.identity().unwrap(),
            ring_name: name.to_owned(),
            collector_pid: 0,
        }],
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
}

fn generation_writer(
    place: &collector::ring::Place,
    shape: collector::ring::Shape,
    identity: &collector::ring::RunIdentity,
) -> collector::ring::Writer {
    collector::ring::Writer::create_with_identity(
        place,
        shape,
        collector::ring::DictionaryCapacity {
            entries: 4,
            arena_bytes: 32_768,
        },
        identity,
        0,
    )
    .unwrap()
}

fn publish_generation_app_tick(writer: &mut collector::ring::Writer, run_tick: u64) {
    let mut program = None;
    writer
        .intern_program(
            &collector::ring::ProgramBytes {
                executable_name: b"engineer",
                app_path: Some(b"/Tools.app"),
            },
            &mut program,
        )
        .unwrap();
    let process = collector::processes::Process {
        pid: 7,
        usage: super::startup_tests::usage(run_tick),
        task: collector::processes::Task {
            threads: 1,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    };
    let identity = collector::ring::IdentityObservation {
        pid: 7,
        usage: collector::ring::UsageAvailability::Measured,
        identity: collector::ring::ProcessIdentity::Measured {
            creation: collector::ring::ProcessCreation {
                unique_id: 7,
                pid_version: -7,
            },
            program: program.unwrap(),
        },
    };

    with_generation_tick(run_tick, |readings| {
        writer
            .publish_with_identity(
                &collector::ring::Readings {
                    processes: std::slice::from_ref(&process),
                    ..readings
                },
                Some(&collector::ring::ObservationTimes {
                    wall_ns: (10_000 + run_tick) * 1_000_000_000,
                    continuous_ns: run_tick * 1_000_000_000,
                    awake_ns: run_tick * 1_000_000_000,
                }),
                &collector::ring::IdentityPublication {
                    processes: std::slice::from_ref(&identity),
                    events: &[],
                },
            )
            .unwrap();
    });
}

fn publish_generation_tick(writer: &mut collector::ring::Writer, run_tick: u64) {
    with_generation_tick(run_tick, |readings| {
        writer.publish_observed(
            &readings,
            &collector::ring::ObservationTimes {
                wall_ns: (10_000 + run_tick) * 1_000_000_000,
                continuous_ns: run_tick * 1_000_000_000,
                awake_ns: run_tick * 1_000_000_000,
            },
        );
    });
}

fn with_generation_tick(run_tick: u64, take: impl FnOnce(collector::ring::Readings<'_>)) {
    let elapsed = u32::try_from(run_tick).unwrap();
    let cpus = [collector::machine::CPUTicks {
        user: elapsed,
        system: 0,
        nice: 0,
        idle: elapsed,
    }];
    let memory = collector::machine::Memory::default();
    take(collector::ring::Readings {
        run_tick,
        cpus: &cpus,
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: collector::ring::Dropped::default(),
    });
}

fn stepped_view(
    held: &mut super::Held,
    ring: &mut super::Ring,
    operation: &crate::cancellation::Operation,
) -> Arc<super::View> {
    let mailbox = super::Mailbox::new();
    for _ in 0..2 {
        super::step(
            held,
            ring,
            Ok(()),
            &mailbox,
            Some(operation),
            super::Publication::Complete,
        )
        .unwrap();
    }
    mailbox.take().unwrap()
}
