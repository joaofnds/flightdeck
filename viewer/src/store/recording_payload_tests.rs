use std::sync::Arc;

use super::{Decoder, load_consume};
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::store::{AppsSecond, ByteBudget};

#[test]
fn a_sealed_boot_mismatch_cannot_seed_machine_rates_under_the_registry_identity() {
    // Goal: a committed minute from another boot cannot be labelled as the registry's run.
    // Method: change only its manifest boot, preserving the hard link, and overwrite its ring tick.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(100);
    fixture.seal_minute(0);
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found).unwrap();
    change_committed_boot(&fixture.root, &found[0].marker);
    for second in 101..109 {
        fixture.publish_with_system_rows(second);
    }
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut accepted = Vec::new();

    let outcome = crate::store::recording::load_payload(
        &fixture.root,
        &budget,
        &operation,
        |_, second, _| accepted.push(second.publication),
    )
    .unwrap();
    let terminal_present = outcome.terminal.is_some();
    let mut ring = crate::store::ring::Ring::new(fixture.root.clone());
    ring.accept(outcome.terminal);
    ring.follow().unwrap();
    let mut successors = Vec::new();
    ring.read(|_, second, _| {
        successors.push(*second);
        true
    })
    .unwrap();

    assert_eq!(successors[0].publication, 2);
    assert_eq!(successors[0].cpu_percent, None);
    assert!(!successors[0].follows);
    assert!(accepted.is_empty());
    assert_eq!(outcome.left_out, Some(crate::store::Failure::Refused));
    assert!(!terminal_present);
    assert_eq!(successors[1].cpu_percent, Some(50.0));
    assert_eq!(budget.used(), 0);
}

fn change_committed_boot(root: &std::path::Path, marker: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;

    let original = persistence::read_committed(root, marker).unwrap();
    let metadata = root.join(&original.data_directory).join("metadata");
    let inode = std::fs::metadata(marker).unwrap().ino();
    assert_eq!(std::fs::metadata(&metadata).unwrap().ino(), inode);
    let mut bytes = std::fs::read(marker).unwrap();
    // Format 1 stores magic, schema/encoding words, then recording/run/boot identities.
    let boot =
        b"FDSEAL01".len() + 2 * size_of::<u64>() + 2 * size_of::<collector::ring::Identity>();
    assert_eq!(&bytes[..8], b"FDSEAL01");
    assert_eq!(
        &bytes[boot..boot + 8],
        &original.identity.run.boot.0[0].to_le_bytes()
    );
    bytes[boot] ^= 1;
    std::fs::write(marker, bytes).unwrap();

    let changed = persistence::read_committed(root, marker).unwrap();
    assert_eq!(
        changed.identity.run.recording,
        original.identity.run.recording
    );
    assert_eq!(changed.identity.run.run, original.identity.run.run);
    assert_ne!(changed.identity.run.boot, original.identity.run.boot);
    assert_eq!(std::fs::metadata(marker).unwrap().ino(), inode);
    assert_eq!(std::fs::metadata(metadata).unwrap().ino(), inode);
}

#[test]
fn frame_assembly_observes_the_callers_cancel_between_samples() {
    // Goal: frame assembly cannot continue under an unrelated uncancelled operation.
    // Method: cancel from the first of two real samples and require no second sample/baseline.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found).unwrap();
    let sealed = crate::recording::SealedMinute::open(&fixture.root, &found[0].marker).unwrap();
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let mut before = None;
    let mut accepted = Vec::new();

    let result =
        sealed.visit_samples_with_operation(&mut before, &operation, |endpoint, sample| {
            assert!(sample.is_ok());
            accepted.push(endpoint.stamp.publication);
            cancellation.cancel()?;
            Ok(())
        });

    assert_eq!(accepted, [1]);
    assert!(
        matches!(result, Err(crate::recording::Refused::Decode(persistence::Error::Io(error)))
        if error.kind() == std::io::ErrorKind::Interrupted)
    );
    assert!(before.is_none());
}

#[test]
fn cancellation_in_one_minute_accepts_only_its_first_observation() {
    // Goal: cancellation within a completion releases its unconsumed observations.
    // Method: cancel the direct adapter's first callback in a real two-observation minute.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let mut accepted = Vec::new();

    let result = crate::store::recording::load_payload(
        &fixture.root,
        &budget,
        &operation,
        |_, second, apps| {
            let crate::store::apps::TickApps::Loaded(apps) = apps else {
                panic!("the real minute has complete payloads");
            };
            assert!(budget.used() > apps.processes.as_ref().unwrap().bytes());
            accepted.push(second.publication);
            cancellation.cancel().unwrap();
        },
    );

    assert_eq!(accepted, [1]);
    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_after_decode_releases_pending_payloads_without_consuming() {
    // Goal: a decoded completion cancelled before consumption cannot become retained state.
    // Method: decode a real minute, cancel its caller operation, then pass it to the consumer.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(160);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found).unwrap();
    found.reverse();
    let mut decoder = Decoder::default();
    let first = decoder.decode(&fixture.root, &found[0], &budget, &operation);
    let mut retained = AppsSecond::with_room();
    load_consume(first, &operation, |_, _, apps| {
        retained.take(apps, None, Some(&budget));
    })
    .unwrap();
    let reader = Arc::new(retained);
    let charge = reader.processes.as_ref().unwrap().bytes();
    let completion = decoder.decode(&fixture.root, &found[1], &budget, &operation);
    assert!(budget.used() > charge);
    let mut accepted = 0;
    cancellation.cancel().unwrap();

    let outcome = load_consume(completion, &operation, |_, _, _| accepted += 1);

    assert_eq!(accepted, 0);
    assert!(matches!(outcome, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    drop(decoder);
    assert_eq!(budget.used(), charge);
    assert_eq!(reader.observation.unwrap().stamp.publication, 1);
    assert_eq!(reader.processes.as_ref().unwrap().len(), 2);
    drop(reader);
    assert_eq!(budget.used(), 0);
}

#[test]
fn terminal_projection_keeps_only_the_last_raw_tick_and_all_its_tables() {
    // Goal: the terminal owner contains one raw tick even when its wall clock is unavailable.
    // Method: add each optional table to a real decoded two-tick minute and project its tail.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let mut found = Vec::new();
    super::super::find(&fixture.root, &mut found).unwrap();
    let mut decoder = Decoder::default();
    drop(decoder.decode(&fixture.root, &found[0], &budget, &operation));
    decoder.previous.ticks[1].wall_ns = None;
    for publication in 1..=2 {
        decoder
            .previous
            .gpu_states
            .push(crate::recording::GpuStateRow {
                publication,
                state: 0,
                active_ns: publication,
            });
        decoder
            .previous
            .core_states
            .push(crate::recording::CoreStateRow {
                publication,
                core: 0,
                state: 0,
                active_ns: publication,
                frequency_khz: Some(1),
            });
        decoder.previous.fans.push(crate::recording::FanRow {
            publication,
            fan: 0,
            rpm: 1.0,
        });
    }

    let terminal = decoder.terminal().unwrap();

    assert_eq!(terminal.machine.ticks.len(), 1);
    assert_eq!(terminal.machine.ticks[0].publication, 2);
    assert_eq!(terminal.machine.ticks[0].wall_ns, None);
    assert_eq!(terminal.machine.cpus.len(), 1);
    assert_eq!(terminal.machine.cpus[0].publication, 2);
    assert_eq!(terminal.machine.memory.len(), 1);
    assert_eq!(terminal.machine.memory[0].publication, 2);
    assert_eq!(terminal.machine.interfaces.len(), 1);
    assert_eq!(terminal.machine.interfaces[0].publication, 2);
    assert_eq!(terminal.machine.gpu_states.len(), 1);
    assert_eq!(terminal.machine.gpu_states[0].publication, 2);
    assert_eq!(terminal.machine.core_states.len(), 1);
    assert_eq!(terminal.machine.core_states[0].publication, 2);
    assert_eq!(terminal.machine.fans.len(), 1);
    assert_eq!(terminal.machine.fans[0].publication, 2);
    assert_eq!(budget.used(), 0);
}
