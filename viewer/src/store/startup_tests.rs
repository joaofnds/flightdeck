use std::sync::Arc;

use super::{AppsSecond, ByteBudget};
use crate::live::identity_fixture::Fixture;

#[test]
fn cancellation_immediately_before_publication_leaves_no_startup_view() {
    // Goal: a populated startup cannot publish after its caller cancels.
    // Method: consume real sealed data, cancel, then invoke the Store's actual publication step.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let cancellation = Arc::new(crate::cancellation::Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::new();
    held.enable_payload(Arc::clone(&budget));
    let outcome =
        super::recording::load_payload(&fixture.root, &budget, &operation, |run, second, apps| {
            assert!(held.take(run, second, apps));
        })
        .unwrap();
    let mut ring = super::Ring::new(fixture.root.clone());
    ring.accept(outcome.terminal);
    let mailbox = super::Mailbox::new();
    let reader = Arc::clone(held.apps.newest_held().unwrap());
    let charge = reader.processes.as_ref().unwrap().bytes();
    cancellation.cancel().unwrap();

    let result = super::step(&mut held, &mut ring, Ok(()), &mailbox, Some(&operation));

    assert!(mailbox.take().is_none());
    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    drop(ring);
    drop(held);
    assert_eq!(budget.used(), charge);
    assert_eq!(reader.observation.unwrap().stamp.publication, 1);
    drop(reader);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_before_loading_accepts_no_observations() {
    // Goal: a superseded startup cannot accept its first minute.
    // Method: cancel an existing caller operation before loading a real sealed minute.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let cancellation = Arc::new(crate::cancellation::Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut accepted = 0;
    cancellation.cancel().unwrap();

    let outcome = super::recording::load_payload(&fixture.root, &budget, &operation, |_, _, _| {
        accepted += 1;
    });

    assert_eq!(accepted, 0);
    assert!(matches!(outcome, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_during_consumption_accepts_no_later_minute() {
    // Goal: cancelling one completion prevents any later minute from being accepted.
    // Method: cancel in the oldest of three real minutes and inspect the accepted publications.
    let mut fixture = Fixture::new();
    for minute in 0..3 {
        fixture.publish_with_system_rows(100 + minute * 60);
        fixture.skip_run_ticks(59);
        fixture.seal_minute(minute);
    }
    let cancellation = Arc::new(crate::cancellation::Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut accepted = Vec::new();

    let outcome =
        super::recording::load_payload(&fixture.root, &budget, &operation, |_, second, _| {
            accepted.push(second.publication);
            cancellation.cancel().unwrap();
        });

    assert_eq!(accepted, [1]);
    assert!(matches!(outcome, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_after_last_consumption_rejects_the_terminal_handoff() {
    // Goal: a cancellation after all rows were consumed still rejects startup completion.
    // Method: cancel the caller's operation while consuming the final real sealed observation.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let cancellation = Arc::new(crate::cancellation::Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::new();
    held.enable_payload(Arc::clone(&budget));
    let mut accepted = 0;

    let outcome =
        super::recording::load_payload(&fixture.root, &budget, &operation, |run, second, apps| {
            assert!(held.take(run, second, apps));
            accepted += 1;
            cancellation.cancel().unwrap();
        });
    let reader = Arc::clone(held.apps.newest_held().unwrap());
    let charge = reader.processes.as_ref().unwrap().bytes();
    let mut ring = super::Ring::new(fixture.root.clone());
    let mailbox = super::Mailbox::new();
    let result = super::startup_accept(outcome, &mut held, &mut ring, &operation);
    let published = super::step(&mut held, &mut ring, Ok(()), &mailbox, Some(&operation));

    assert_eq!(accepted, 1);
    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert!(published.is_err());
    assert!(mailbox.take().is_none());
    drop(ring);
    drop(held);
    assert_eq!(budget.used(), charge);
    assert_eq!(reader.observation.unwrap().stamp.publication, 1);
    drop(reader);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cold_store_keeps_the_overwritten_sealed_predecessor() {
    // Goal: opening after the sealed predecessor leaves the ring preserves exact rates.
    // Method: compare warm/cold Stores at publication two after eight successors overwrite one.
    assert_cold_matches_warm(1);
}

#[test]
fn cold_store_uses_the_last_tick_of_a_multi_tick_sealed_minute() {
    // Goal: the terminal machine predecessor is the last tick, not the minute's first.
    // Method: compare exact warm/cold rates after a two-tick minute leaves the ring.
    assert_cold_matches_warm(2);
}

fn assert_cold_matches_warm(sealed_count: u64) {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(60 - sealed_count);
    for offset in 0..sealed_count {
        fixture.publish_with_usage(100 + offset, &usage(offset));
    }
    let warm = super::Store::start_with_payload(&fixture.root, 64 * 1024 * 1024).unwrap();
    super::tests::view_where(&warm, |view| {
        view.seconds.len() == usize::try_from(sealed_count).unwrap()
    });
    fixture.seal_minute(0);
    for offset in sealed_count..sealed_count + 8 {
        fixture.publish_with_usage(100 + offset, &usage(offset));
    }
    let expected = super::tests::view_where(&warm, |view| {
        view.seconds.len() == usize::try_from(sealed_count + 8).unwrap()
    });

    let cold = super::Store::start_with_payload(&fixture.root, 64 * 1024 * 1024).unwrap();
    let actual = super::tests::view_where(&cold, |view| {
        view.seconds.len() == usize::try_from(sealed_count + 8).unwrap()
    });

    let index = usize::try_from(sealed_count).unwrap();
    assert_eq!(expected.seconds[index].publication, sealed_count + 1);
    assert_eq!(expected.seconds[index].cpu_percent, Some(50.0));
    assert_eq!(actual.seconds[index], expected.seconds[index]);
    assert_eq!(actual.apps[index], expected.apps[index]);
    let payload = actual.apps[index].processes.as_ref().unwrap();
    let process = payload.iter().find(|process| process.pid == 7).unwrap();
    assert_eq!(process.disk_read_bytes, crate::apps::Amount::Known(17));
    assert_eq!(process.disk_written_bytes, crate::apps::Amount::Known(43));
    assert_eq!(payload.previous.unwrap().publication, sealed_count);
    assert_eq!(actual.apps[index].elapsed_ns, Some(1_000_000_000));
}

pub(super) fn usage(offset: u64) -> collector::processes::Usage {
    collector::processes::Usage {
        awake_ns_at_start: 0,
        user_ns: offset * 100_000_000,
        system_ns: 0,
        energy_nj: 0,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes: 11,
        footprint_bytes: 12,
        disk_read_bytes: 9_007_199_254_740_993 + offset * 17,
        disk_written_bytes: 9_007_199_254_741_993 + offset * 43,
    }
}

#[test]
fn sealed_minutes_release_each_completion_before_decoding_the_next() {
    // Goal: consumed minute payloads cannot starve subsequent sealed observations.
    // Method: load three real minutes under one observed payload's byte budget.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(160);
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(220);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    fixture.seal_minute(2);
    let charges = fixture_payload_charges(&fixture);
    let payload_maximum = *charges.iter().max().unwrap();
    let maximum = payload_maximum + fixture_input_peak(&fixture);
    assert!(*charges.iter().min().unwrap() > 0);
    assert!(charges.iter().sum::<usize>() > payload_maximum);
    assert!(charges.iter().min().unwrap() * 2 > payload_maximum);
    let budget = Arc::new(ByteBudget::new(maximum));
    let mut results = Vec::with_capacity(3);
    let mut captures = 0;
    let mut capture = |file, budget: &Arc<ByteBudget>| {
        assert_eq!(
            budget.used(),
            0,
            "only consumed completions may precede the next capture"
        );
        captures += 1;
        persistence::engine::AdmittedTable::from_file_budgeted(file, budget)
    };

    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let outcome = super::recording::load_payload_using(
        &fixture.root,
        &budget,
        &operation,
        &mut capture,
        |_, second, apps| {
            let super::apps::TickApps::Loaded(loaded) = apps else {
                panic!("valid fixture machine and App rows must decode");
            };
            let mut taken = AppsSecond::with_room();
            taken.take(super::apps::TickApps::Loaded(loaded), None, Some(&budget));
            let count = taken
                .processes
                .as_ref()
                .map(|payload| payload.len())
                .map_err(Clone::clone);
            results.push((second.publication, count));
            drop(taken);
        },
    );

    assert_eq!(outcome.unwrap().left_out, None);
    assert_eq!(results, [(1, Ok(2)), (2, Ok(2)), (3, Ok(2))]);
    assert_eq!(captures, 3);
    assert_eq!(budget.used(), 0);
}

fn fixture_input_peak(fixture: &Fixture) -> usize {
    // The cursor certificate requires 43 MiB scratch alongside retained ticks and names.
    let budget = Arc::new(ByteBudget::new(usize::MAX));
    let mut peak = 0;
    for window in 0..4 {
        let marker = persistence::layout::WindowKey {
            run: fixture.writer.identity().unwrap().run,
            window,
        }
        .marker(&fixture.root);
        if !marker.exists() {
            continue;
        }
        let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
        let path = fixture
            .root
            .join(manifest.data_directory)
            .join("ticks.parquet");
        let file = std::fs::File::open(path).unwrap();
        let input = persistence::engine::AdmittedTable::from_file_budgeted(file, &budget).unwrap();
        peak = peak.max(43 * 1024 * 1024 + budget.used());
        drop(input);
    }
    assert_eq!(budget.used(), 0);
    peak
}

fn fixture_payload_charges(fixture: &Fixture) -> Vec<usize> {
    let budget = Arc::new(ByteBudget::new(usize::MAX));
    let mut ring = super::ring::Ring::new(fixture.root.clone());
    ring.follow().unwrap();
    let mut charges = Vec::with_capacity(3);
    ring.read(|_, _, apps| {
        let mut captured = AppsSecond::with_room();
        captured.take(apps, None, Some(&budget));
        let payload = captured.processes.as_ref().unwrap();
        assert_eq!(payload.len(), 2);
        charges.push(payload.bytes());
        true
    })
    .unwrap();
    assert!(!charges.is_empty());
    assert_eq!(budget.used(), 0);
    charges
}

#[test]
fn consumed_completions_release_displaced_payloads_but_preserve_readers() {
    // Goal: recycling releases displaced payloads without releasing a retained reader's lease.
    // Method: transfer four real minutes through two buffers, retaining the first reading.
    let mut fixture = Fixture::new();
    for minute in 0..4 {
        fixture.publish_with_system_rows(100 + minute * 60);
        fixture.skip_run_ticks(59);
        fixture.seal_minute(minute);
    }
    let charges = fixture_payload_charges(&fixture);
    let payload_maximum = charges.iter().max().unwrap() * 3;
    let maximum = payload_maximum + fixture_input_peak(&fixture);
    assert!(charges.iter().sum::<usize>() > payload_maximum);
    let budget = Arc::new(ByteBudget::new(maximum));
    let mut span = super::Span::new(1, super::Buffers::new(2, AppsSecond::with_room));
    let mut reader = None;
    let mut publications = Vec::new();
    let expected = [
        0,
        charges[0],
        charges[0] + charges[1],
        charges[0] + charges[2],
    ];
    let mut captures = 0;
    let mut capture = |file, budget: &Arc<ByteBudget>| {
        assert_eq!(
            budget.used(),
            expected[captures],
            "only reader/pool owners may precede capture"
        );
        captures += 1;
        persistence::engine::AdmittedTable::from_file_budgeted(file, budget)
    };

    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let outcome = super::recording::load_payload_using(
        &fixture.root,
        &budget,
        &operation,
        &mut capture,
        |_, second, apps| {
            assert!(span.push(|buffer| buffer.take(apps, None, Some(&budget))));
            let newest = span.newest_held().unwrap();
            assert_eq!(newest.processes.as_ref().unwrap().len(), 2);
            publications.push(second.publication);
            reader.get_or_insert_with(|| Arc::clone(newest));
        },
    );

    assert_eq!(outcome.unwrap().left_out, None);
    assert_eq!(publications, [1, 2, 3, 4]);
    assert_eq!(captures, 4);
    let reader = reader.unwrap();
    assert_eq!(reader.observation.unwrap().stamp.publication, 1);
    assert_eq!(reader.processes.as_ref().unwrap().len(), 2);
    assert_eq!(budget.used(), charges[0] + charges[3]);
    drop(span);
    assert_eq!(budget.used(), charges[0]);
    drop(reader);
    assert_eq!(budget.used(), 0);
}
