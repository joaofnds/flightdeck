use std::io::ErrorKind;
use std::sync::Arc;

use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::store::{ByteBudget, Failure};

#[test]
fn a_capacity_stop_reports_incomplete_history_before_decoding() {
    // Goal: a bounded discovery cannot present a truncated source window as complete.
    // Method: seal 129 short windows inside the requested hour and invoke both loaders.
    let fixture = short_windows(129);
    let mut accepted = 0;

    let left_out = super::load_window(
        &fixture.root,
        None,
        crate::session::LOADED_SECONDS,
        &mut super::LoadBuffers::default(),
        |_, _, _| accepted += 1,
    );

    assert_eq!(left_out, Some(Failure::Commits(ErrorKind::InvalidData)));
    assert_eq!(accepted, 0);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

    let outcome =
        super::load_payload(&fixture.root, &budget, &operation, |_, _, _| accepted += 1).unwrap();

    assert_eq!(outcome.left_out, left_out);
    assert!(outcome.terminal.is_none());
    assert_eq!(accepted, 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn exactly_full_discovery_succeeds_when_the_source_is_exhausted() {
    // Goal: reaching the capacity is valid when discovery also proves source exhaustion.
    // Method: enumerate exactly 128 real sealed windows with no older source window.
    let fixture = short_windows(128);
    let mut found = Vec::new();

    let result = super::find(&fixture.root, &mut found, None);

    assert_eq!(result, Ok(()));
    assert_eq!(found.len(), 128);
}

#[test]
fn exactly_full_discovery_succeeds_when_the_next_window_is_outside_the_period() {
    // Goal: an older window can prove the requested boundary after the last accepted slot.
    // Method: request 67 seconds plus the 60-second lead-in from 129 short windows.
    let fixture = short_windows(129);
    let mut found = Vec::new();

    let result = super::find_window(
        &fixture.root,
        &mut found,
        &super::LoadWindow {
            since: None,
            seconds: 67,
            unread: false,
        },
    );

    assert_eq!(result, Ok(()));
    assert_eq!(found.len(), 128);
    assert_eq!(found.last().unwrap().publication_end, 3);
}

#[test]
fn payload_discovery_keeps_in_range_history_behind_an_old_clock_marker() {
    // Goal: an older wall time cannot hide a still-relevant earlier publication.
    // Method: seal a clock rollback between two in-range windows and read the payload adapter.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(6_000, &[]);
    fixture.seal_minute(1);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(10_010, &[]);
    fixture.seal_minute(2);

    let actual = payload_publications(&fixture);

    assert_eq!(actual, [1, 3]);
}

#[test]
fn payload_discovery_keeps_in_range_history_in_a_third_partition_day() {
    // Goal: elapsed partition days cannot exclude readings inside the requested wall period.
    // Method: seal three sparse days whose observed wall times remain one second apart.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(86_399);
    fixture.publish_with_events(10_001, &[]);
    fixture.seal_minute(1_440);
    fixture.skip_run_ticks(86_399);
    fixture.publish_with_events(10_002, &[]);
    fixture.seal_minute(2_880);

    let actual = payload_publications(&fixture);

    assert_eq!(actual, [1, 2, 3]);
}

#[test]
fn payload_discovery_keeps_committed_history_after_run_registry_eviction() {
    // Goal: the current ring registry cannot erase independently committed history.
    // Method: remove a sealed run from the registry, then read its original payload publication.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.seal_minute(0);
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 1,
    }
    .write(&fixture.root)
    .unwrap();

    let actual = payload_publications(&fixture);

    assert_eq!(actual, [1]);
}

#[test]
fn payload_discovery_refuses_an_unknown_checkpoint_run() {
    // Goal: catalog discovery cannot accept a checkpoint whose run the source never recorded.
    // Method: change the checkpoint run identity while keeping its real recording identity.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.seal_minute(0);
    let mut run = fixture.writer.identity().unwrap();
    run.run.0[0] ^= 1;
    let since = crate::store::Newest {
        run,
        publication: 1,
        segment: 0,
    };
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut accepted = 0;

    let outcome = super::load_payload_since(
        &fixture.root,
        &budget,
        &operation,
        Some(&since),
        |_, _, _| accepted += 1,
    )
    .unwrap();

    assert_eq!(outcome.left_out, Some(Failure::ChangedIdentity));
    assert!(outcome.terminal.is_none());
    assert_eq!(accepted, 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn payload_discovery_anchors_and_finishes_at_the_registered_run() {
    // Goal: archived clocks and UUID order cannot displace the current run's payload handoff.
    // Method: register only the lower UUID, with an archived wall time far beyond its endpoint.
    let mut first = Fixture::new();
    let mut second = Fixture::new_mode(false, 2, Some(&first));
    let (current, archived) = register_lower_run(&mut first, &mut second);
    archived.publish_with_events(20_000, &[]);
    archived.seal_minute(0);
    current.publish_with_events(10_000, &[]);
    current.seal_minute(0);
    let current_run = current.writer.identity().unwrap();
    let archived_run = archived.writer.identity().unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut accepted = Vec::new();

    let outcome = super::load_payload(&current.root, &budget, &operation, |run, second, _| {
        accepted.push((*run, second.publication, second.wall_ns));
    })
    .unwrap();

    assert_eq!(outcome.left_out, None);
    assert_eq!(
        accepted,
        [
            (archived_run, 1, 20_000_000_000_000),
            (current_run, 1, 10_000_000_000_000),
        ]
    );
    assert_eq!(outcome.terminal.as_ref().unwrap().run, current_run);
    drop(outcome);
    assert_eq!(budget.used(), 0);
}

fn register_lower_run<'a>(
    first: &'a mut Fixture,
    second: &'a mut Fixture,
) -> (&'a mut Fixture, &'a mut Fixture) {
    let (current, archived) =
        if first.writer.identity().unwrap().run.0 < second.writer.identity().unwrap().run.0 {
            (first, second)
        } else {
            (second, first)
        };
    collector::recording::Registry {
        runs: vec![collector::recording::RegisteredRun {
            identity: current.writer.identity().unwrap(),
            ring_name: current.writer.name().to_owned(),
            collector_pid: 0,
        }],
        evictions: 1,
    }
    .write(&current.root)
    .unwrap();

    (current, archived)
}

fn payload_publications(fixture: &Fixture) -> Vec<u64> {
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut publications = Vec::new();
    let outcome = super::load_payload(&fixture.root, &budget, &operation, |_, second, _| {
        publications.push(second.publication);
    })
    .unwrap();
    assert_eq!(outcome.left_out, None);
    drop(outcome);
    assert_eq!(budget.used(), 0);

    publications
}

fn short_windows(count: u64) -> Fixture {
    let mut fixture = Fixture::new();
    for minute in 0..count {
        fixture.publish_with_events(10_000 + minute, &[]);
        fixture.skip_run_ticks(59);
        fixture.seal_minute(minute);
    }

    fixture
}
