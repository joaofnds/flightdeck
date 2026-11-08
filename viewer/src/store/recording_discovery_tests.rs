use std::io::ErrorKind;
use std::sync::Arc;

use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::root_fixture::{older_root, unfinished_root};
use crate::store::{ByteBudget, Failure};

#[test]
fn an_empty_completed_catalog_keeps_checked_absence_without_inventing_a_clock() {
    // Goal: a recording with no sealed windows has checked absence, not unchecked history.
    // Method: load a real registered recording before its first publication or commit.
    let fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

    let outcome = super::load_payload(&fixture.root, &budget, &operation, |_, _, _| {
        panic!("an empty recording has no chart readings");
    })
    .unwrap();

    assert_eq!(outcome.left_out, None);
    assert!(outcome.terminal.is_none());
    assert_eq!(
        outcome.checked_catalog,
        Some(super::CheckedCatalog {
            recording: fixture.writer.identity().unwrap().recording,
            newest_run: fixture.writer.identity(),
            first_ns: None,
            windows: 0,
            first_known_second: None,
            last_known_second: None,
            unknown_clock_ticks: Some(0),
            decoded_is_whole: true,
        })
    );
}

#[test]
fn a_damaged_recording_reports_a_failure_instead_of_no_minutes() {
    // Goal: a recording missing what every recording holds does not read as one with no minutes.
    // Method: seal a window, remove a directory the recording's format requires, and load it.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.seal();
    std::fs::remove_dir(fixture.root.join("event-fragments")).unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

    let outcome = super::load_payload(&fixture.root, &budget, &operation, |_, _, _| {});

    assert_eq!(
        outcome.unwrap().left_out,
        Some(Failure::Commits(ErrorKind::InvalidData))
    );
}

macro_rules! a_root_it_cannot_read_reports_a_failure_instead_of_no_minutes {
    ($($name:ident: $root:expr, $failure:expr,)+) => {$(
        #[test]
        fn $name() {
            // Goal: a load names why it read nothing where the root holds nothing it can read.
            // Method: load a root of another format or still being set up.
            let root = $root;
            let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
            let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

            let payload = super::load_payload(&root, &budget, &operation, |_, _, _| {});

            assert_eq!(payload.unwrap().left_out, Some($failure));
            std::fs::remove_dir_all(root).unwrap();
        }
    )+};
}

a_root_it_cannot_read_reports_a_failure_instead_of_no_minutes! {
    a_load_refuses_a_recording_of_an_older_format:
        older_root(), Failure::Commits(ErrorKind::Unsupported),
    a_load_finds_no_recording_in_a_root_still_being_set_up:
        unfinished_root(), Failure::Recording(ErrorKind::NotFound),
}

#[test]
fn a_successful_load_reports_the_actual_catalog_pruning_floor() {
    // Goal: a catalog certificate reports what discovery selected rather than held timestamps.
    // Method: load a real sealed window and compare its certificate with the pruning threshold.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.seal();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

    let outcome = super::load_payload(&fixture.root, &budget, &operation, |_, _, _| {}).unwrap();

    assert_eq!(outcome.left_out, None);
    let checked = outcome.checked_catalog.unwrap();
    assert_eq!(checked.windows, 1);
    assert_eq!(checked.first_known_second, Some(10_000));
    assert_eq!(checked.last_known_second, Some(10_000));
    assert_eq!(checked.unknown_clock_ticks, Some(0));
    assert_eq!(
        checked.first_ns,
        Some(
            (10_000 - crate::session::LOADED_SECONDS - crate::session::LEAD_IN_SECONDS)
                * 1_000_000_000,
        )
    );
}

#[test]
fn a_registered_recording_loads_its_minutes_through_its_catalog() {
    // Goal: the store's load reaches a registered recording's minutes with no list of them, and
    // reports what it read.
    // Method: save two minutes far apart through the worker and load the newest hour.
    let mut fixture = Fixture::registered();
    fixture.publish_with_events(10_000, &[]);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(100_000, &[]);
    fixture.seal();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

    let outcome = super::load_payload(&fixture.root, &budget, &operation, |_, _, _| {}).unwrap();

    assert_eq!(outcome.left_out, None);
    let checked = outcome.checked_catalog.unwrap();
    assert_eq!(checked.windows, 1);
    assert_eq!(checked.first_known_second, Some(10_000));
    assert_eq!(checked.last_known_second, Some(100_000));
    assert_eq!(checked.unknown_clock_ticks, Some(0));
}

#[test]
fn a_rollback_before_the_catalog_floor_does_not_certify_omitted_readings() {
    // Goal: a latest low clock cannot widen checked coverage over windows discovery skipped.
    // Method: load a high/low-clock minute after an omitted low-clock window.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(5_000, &[]);
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(1);
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut publications = Vec::new();

    let outcome = super::load_payload(&fixture.root, &budget, &operation, |_, second, _| {
        publications.push((second.publication, second.wall_ns));
    })
    .unwrap();

    assert_eq!(publications, [(2, 5_000_000_000_000), (3, 100_000_000_000)]);
    assert_eq!(
        outcome.checked_catalog.unwrap().first_ns,
        Some(1_340_000_000_000)
    );
}

#[test]
fn a_registry_change_during_loading_revokes_the_checked_catalog() {
    // Goal: successful old-file decoding cannot certify a source changed during that decode.
    // Method: retire the registered source inside the real immutable input admission callback.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let changed = std::sync::atomic::AtomicBool::new(false);

    let outcome = super::load_payload_using(
        &fixture.root,
        &budget,
        &operation,
        &|file, budget| {
            let input = persistence::engine::AdmittedTable::from_file_budgeted(file, budget)?;
            if !changed.swap(true, std::sync::atomic::Ordering::AcqRel) {
                collector::recording::Registry {
                    runs: Vec::new(),
                    evictions: 1,
                }
                .write(&fixture.root)
                .unwrap();
            }

            Ok(input)
        },
        |_, _, _| {},
    )
    .unwrap();

    assert_eq!(outcome.left_out, Some(Failure::ChangedIdentity));
    assert_eq!(outcome.checked_catalog, None);
}

#[test]
fn input_admission_failure_does_not_certify_a_discovered_catalog() {
    // Goal: discovering markers does not certify inputs the decoder could not admit.
    // Method: load a real sealed minute with a byte budget too small for its first input.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal();
    let budget = Arc::new(ByteBudget::new(1));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

    let outcome = super::load_payload(&fixture.root, &budget, &operation, |_, _, _| {
        panic!("a denied input cannot produce an accepted reading");
    })
    .unwrap();

    assert_eq!(outcome.left_out, Some(Failure::Budget));
    assert_eq!(outcome.checked_catalog, None);
    assert_eq!(budget.used(), 0);
}

#[test]
fn an_unread_checkpoint_load_does_not_certify_skipped_old_windows() {
    // Goal: a checkpoint alone cannot certify old inputs the current loader did not decode.
    // Method: load only the work after publication 2 from a real three-window recording.
    let mut fixture = Fixture::new();
    for minute in 0..3 {
        fixture.publish_with_events(100 + minute * 60, &[]);
        fixture.skip_run_ticks(59);
        fixture.seal_minute(minute);
    }
    let since = crate::store::Newest {
        run: fixture.writer.identity().unwrap(),
        publication: 2,
        segment: 0,
    };
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut publications = Vec::new();

    let outcome = super::load_payload_since(
        &fixture.root,
        &budget,
        &operation,
        Some(&since),
        |_, second, _| publications.push(second.publication),
    )
    .unwrap();

    assert_eq!(publications, [2, 3]);
    assert_eq!(outcome.left_out, None);
    assert_eq!(outcome.checked_catalog, None);
}

#[test]
fn payload_discovery_keeps_known_readings_when_the_minute_has_no_wall_bounds() {
    // Goal: unknown minute bounds cannot erase the minute's individually clocked readings.
    // Method: seal two real ticks with only the first wall clock absent, then read the payloads.
    let fixture = mixed_wall_minute();

    let actual = payload_publications(&fixture);

    assert_eq!(actual, [2]);
}

#[test]
fn payload_discovery_keeps_a_mixed_minute_beside_a_known_time_anchor() {
    // Goal: a clocked successor cannot make an unbounded predecessor appear irrelevant.
    // Method: add a normally clocked sealed minute after a mixed known/unknown minute.
    let mut fixture = mixed_wall_minute();
    fixture.skip_run_ticks(58);
    fixture.publish_with_events(10_002, &[]);
    fixture.seal_minute(1);

    let actual = payload_publications(&fixture);

    assert_eq!(actual, [2, 3]);
}

#[test]
fn an_unbounded_newest_minute_cannot_borrow_an_older_time_anchor() {
    // Goal: an earlier known bound cannot prune the actual newest observation's context.
    // Method: seal a high-clock minute before a mixed newest minute whose clock rolled back.
    let fixture = unbounded_newest_minute();

    let actual = payload_publications(&fixture);

    assert_eq!(actual, [1, 2, 4]);
}

fn unbounded_newest_minute() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_events(1_000, &[]);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(5_000, &[]);
    fixture.seal_minute_derived(1, &persistence::sealed_apps::SealedApps::default());
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(1_009, &[]);
    fixture.publish_with_events(1_010, &[]);
    fixture.seal_window(
        Some(2),
        &[persistence::Observation {
            publication: 3,
            wall_ns: None,
            continuous_ns: Some(120_000_000_000),
            awake_ns: Some(120_000_000_000),
        }],
        &crate::live::identity_fixture::Underived,
    );

    fixture
}

#[test]
fn clockless_payload_history_preserves_its_raw_terminal_without_inventing_readings() {
    // Goal: a wholly clockless minute retains its rate predecessor without fake chart times.
    // Method: decode a real two-tick minute with both stored wall clocks absent.
    let mut fixture = Fixture::new();
    fixture.set_next_clocks(None);
    fixture.publish_with_events(10_000, &[]);
    fixture.set_next_clocks(None);
    fixture.publish_with_events(10_001, &[]);
    fixture.seal();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut accepted = 0;

    let outcome = super::load_payload(&fixture.root, &budget, &operation, |_, _, _| {
        accepted += 1;
    })
    .unwrap();

    assert_eq!(accepted, 0);
    assert_eq!(outcome.left_out, None);
    let checked = outcome.checked_catalog.unwrap();
    assert_eq!(checked.first_ns, None);
    assert_eq!(checked.windows, 1);
    assert_eq!(checked.first_known_second, None);
    assert_eq!(checked.last_known_second, None);
    assert_eq!(checked.unknown_clock_ticks, Some(2));
    let mut terminal = outcome.terminal.unwrap();
    assert_eq!(terminal.machine.ticks.len(), 1);
    assert_eq!(terminal.machine.ticks[0].publication, 2);
    assert_eq!(terminal.machine.ticks[0].wall_ns, None);
    let (endpoint, frame) = terminal.process.take().unwrap().into_parts();
    assert_eq!(endpoint.identity, fixture.writer.identity().unwrap());
    assert_eq!(endpoint.stamp.publication, 2);
    assert_eq!(endpoint.wall_ns, None);
    drop(frame);
    drop(terminal);
    assert_eq!(budget.used(), 0);
}

fn mixed_wall_minute() -> Fixture {
    mixed_wall_minute_using(&crate::live::identity_fixture::Underived)
}

fn mixed_wall_minute_using(derive: &dyn persistence::worker::Derive) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_events(10_000, &[]);
    fixture.publish_with_events(10_001, &[]);
    fixture.seal_derived(
        &[persistence::Observation {
            publication: 1,
            wall_ns: None,
            continuous_ns: Some(0),
            awake_ns: Some(0),
        }],
        derive,
    );

    fixture
}

#[test]
fn a_capacity_stop_reports_incomplete_history_before_decoding() {
    // Goal: a bounded discovery cannot present a truncated source window as complete.
    // Method: seal 129 short windows inside the requested hour and load them.
    let fixture = short_windows(129);
    let mut accepted = 0;
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

    let outcome =
        super::load_payload(&fixture.root, &budget, &operation, |_, _, _| accepted += 1).unwrap();

    assert_eq!(
        outcome.left_out,
        Some(Failure::Commits(ErrorKind::InvalidData))
    );
    assert_eq!(outcome.checked_catalog, None);
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

    let result = super::catalog::find(
        &fixture.root,
        &mut found,
        &super::LoadWindow {
            since: None,
            seconds: 67,
            unread: false,
        },
    );

    assert!(result.is_ok());
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
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
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
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
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

#[test]
fn runs_the_registry_let_go_load_in_the_order_they_ran() {
    // Goal: a run's random identity cannot decide which of two retired runs a load takes first,
    // since a load keeps the seconds it takes last. Method: save the higher identity's minute first
    // and the lower identity's later, retire both, and read the clocks in the order loaded.
    let mut first = Fixture::new();
    let mut second = Fixture::new_mode(false, 2, Some(&first));
    let (lower, higher) = register_lower_run(&mut first, &mut second);
    higher.publish_with_events(10_000, &[]);
    higher.seal_minute(0);
    lower.publish_with_events(10_100, &[]);
    lower.seal_minute(0);
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 2,
    }
    .write(&lower.root)
    .unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut walls = Vec::new();

    super::load_payload(&lower.root, &budget, &operation, |_, second, _| {
        walls.push(second.wall_ns);
    })
    .unwrap();

    assert_eq!(walls, [10_000_000_000_000, 10_100_000_000_000]);
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
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
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

#[test]
fn a_catalog_begins_at_the_oldest_second_the_period_covers() {
    // Goal: the store says the recording begins at a second a read can still open, while the
    // expired readings stay listed until physical removal reaches them.
    // Method: place the cutoff inside a saved minute and load it.
    let fixture = crate::session::retention_tests::saved_seconds();
    crate::session::retention_tests::expire_before(&fixture.root, 5 * 1_000_000_000);
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

    let outcome = super::load_payload(&fixture.root, &budget, &operation, |_, _, _| {}).unwrap();

    let checked = outcome.checked_catalog.unwrap();
    assert_eq!(checked.first_known_second, Some(1006));
    assert_eq!(checked.last_known_second, Some(1008));
}
