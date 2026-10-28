use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{AppsSecond, ByteBudget};
use crate::live::identity_fixture::Fixture;

#[test]
fn primary_startup_serves_selected_apps_before_historical_apps() {
    // Goal: the selected minute becomes queryable before older App derivations run.
    // Method: stage six real sealed observations across a gap and a consecutive boundary.
    let mut fixture = primary_fixture();
    let trace =
        persistence::work::ingestion::Trace::new(fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::with_payload(Arc::clone(&budget));
    let mut ring = super::native_acceptance::make_ring(&fixture.root, Some(&budget));

    held.startup = super::StartupState::Loading;
    let mut startup = super::recording::staged::Startup::new();
    let input = super::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    drive_primary(&mut startup, &input, &mut held, &mut ring);
    assert_primary_work(&trace);

    assert_eq!(
        held.apps.newest().unwrap().observation.unwrap().run_tick,
        121
    );
    assert_eq!(
        held.apps.get(0).unwrap().missing,
        Some(super::Missing::Unread)
    );
    assert_selected_minute(&held);
    let primary = query_context(&mut held, &mut ring);
    assert_eq!(primary.readiness(), super::QueryReadiness::Primary);
    assert!(primary.seconds().iter().all(super::QuerySecond::detached));
    assert_pending_histories(&primary, &budget);
    let machine = held.span.newest().unwrap().clone();
    drive_complete(&mut startup, &input, &mut held, &mut ring);
    assert!(held.startup == super::StartupState::Complete);
    assert_eq!(held.span.newest().unwrap(), &machine);
    assert!(
        held.apps
            .held()
            .all(|apps| apps.missing != Some(super::Missing::Unread))
    );
    let complete = query_context(&mut held, &mut ring);
    assert_eq!(complete.readiness(), super::QueryReadiness::Complete);
    assert_eq!(complete.source, primary.source);
    let mut completed_work = Vec::new();
    trace.snapshot(&mut completed_work).unwrap();
    assert_canonical_ticks_work(&completed_work);
    fixture.publish_with_usage(1122, &usage(122));
    drop(scope);
}

fn assert_selected_minute(held: &super::Held) {
    for index in 4..6 {
        let apps = held.apps.get(index).unwrap();
        assert_eq!(apps.elapsed_ns, Some(1_000_000_000));
        let payload = apps.processes.as_ref().unwrap();
        assert_eq!(
            payload.previous.unwrap().publication,
            u64::try_from(index).unwrap()
        );
        let process = payload.iter().find(|process| process.pid == 7).unwrap();
        assert_eq!(process.disk_read_bytes(), crate::apps::Amount::Known(17));
        assert_eq!(process.disk_written_bytes(), crate::apps::Amount::Known(43));
    }
}

fn assert_pending_histories(context: &Arc<super::QueryContext>, budget: &Arc<ByteBudget>) {
    use crate::live_app_response::KeyKind;
    let cancellation = Arc::new(crate::Cancellation::default());
    let used = budget.used();
    assert!(matches!(
        context.app_resident_history(&cancellation, 0),
        Err(super::HistoryError::Pending)
    ));
    assert!(matches!(
        context.app_attribution_history(
            crate::app_attribution_history::AppAttributionMetric::Cpu,
            &cancellation,
            0
        ),
        Err(super::HistoryError::Pending)
    ));
    assert!(matches!(
        context.app_history(KeyKind::Executable, b"N", &cancellation, 0),
        Err(super::HistoryError::Pending)
    ));
    assert!(matches!(
        context.app_process_history(KeyKind::Executable, b"N", b"N", &cancellation, 0),
        Err(super::HistoryError::Pending)
    ));
    assert!(matches!(
        context.app_process_groups_history(KeyKind::Executable, b"N", &cancellation, 0),
        Err(super::HistoryError::Pending)
    ));
    assert!(matches!(
        context.app_ranks(),
        Err(super::HistoryError::Pending)
    ));
    assert!(matches!(
        context.app_parts(KeyKind::Executable, b"N"),
        Err(super::HistoryError::Pending)
    ));
    assert_eq!(budget.used(), used);
}

fn assert_primary_work(trace: &persistence::work::ingestion::Trace) {
    use persistence::work::ingestion::{Key, Kind, Purpose};
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    assert_canonical_ticks_work(&events);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == Kind::AppDerivation && event.purpose == Purpose::Ordinary)
            .count(),
        0
    );
    assert_eq!(events.iter().filter(|event| event.kind == Kind::AppDerivation
        && event.purpose == Purpose::Selection).count(), 2);
    for publication in 1..=6 {
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == Kind::MachineReduction)
                .filter(
                    |event| matches!(event.key, Key::Publication { publication: value, .. }
                if value == publication)
                )
                .count(),
            1
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == Kind::CommittedFrameCapture
                && event.purpose == Purpose::Predecessor)
            .count(),
        2
    );
}

fn assert_canonical_ticks_work(events: &[persistence::work::ingestion::Event]) {
    use persistence::work::ingestion::Kind;
    let captures: Vec<_> = events
        .iter()
        .filter(|event| event.kind == Kind::ContentCapture)
        .filter(|event| event.table == Some("ticks"))
        .collect();
    assert_eq!(captures.len(), 3);
    for capture in captures {
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, Kind::ProjectionDecode | Kind::NativeCollect))
                .filter(|event| event.table == Some("ticks"))
                .filter(|event| event.key == capture.key)
                .count(),
            1,
            "one canonical ticks projection for {:?}",
            capture.key
        );
    }
}

#[test]
fn cancellation_between_primary_and_complete_releases_only_unretained_startup_owners() {
    // Goal: a cancelled historical continuation cannot authorize Complete or drop a reader's data.
    // Method: retain a real detached Primary, cancel before history, then release each owner.
    let fixture = primary_fixture();
    let cancellation = Arc::new(crate::Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::with_payload(Arc::clone(&budget));
    held.startup = super::StartupState::Loading;
    let mut ring = super::native_acceptance::make_ring(&fixture.root, Some(&budget));
    let mut startup = super::recording::staged::Startup::new();
    let input = super::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    drive_primary(&mut startup, &input, &mut held, &mut ring);
    let primary = query_context(&mut held, &mut ring);
    cancellation.cancel().unwrap();

    let result = startup.advance(&input, &mut held, &mut ring);

    assert!(
        matches!(result, Err(super::recording::staged::Failure::Read(
        crate::recording::Refused::Decode(persistence::Error::Io(error))))
        if error.kind() == std::io::ErrorKind::Interrupted)
    );
    drop((startup, ring, held));
    assert!(budget.used() > 0);
    assert_eq!(primary.selected().observation.stamp.publication, 6);
    drop(primary);
    assert_eq!(budget.used(), 0);
}

#[test]
fn payload_construction_keeps_legacy_app_row_capacity_outside_the_opt_in_owner() {
    // Goal: opt-in buffers cannot preallocate unadmitted legacy App rows.
    // Method: inspect every free pool slot immediately after both real Held constructors.
    let mut legacy = super::Held::new();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut payload = super::Held::with_payload(Arc::clone(&budget));
    let mut legacy_count = 0;
    let mut payload_count = 0;

    legacy.release_views();
    payload.release_views();
    legacy.apps.release(|apps| {
        assert_eq!(apps.apps.capacity(), super::APPS_PER_SECOND_MAX);
        legacy_count += 1;
    });
    payload.apps.release(|apps| {
        assert_eq!(apps.apps.capacity(), 0);
        payload_count += 1;
    });

    assert_eq!(legacy_count, super::SECONDS_MAX + super::SECONDS_HELD_MAX);
    assert_eq!(payload_count, legacy_count);
    assert_eq!(budget.used(), 0);
}

pub(super) fn drive_primary(
    startup: &mut super::recording::staged::Startup,
    input: &super::recording::staged::Input<'_>,
    held: &mut super::Held,
    ring: &mut super::Ring,
) {
    for _ in 0..super::recording::MINUTES_MAX * 2 {
        if matches!(
            startup.advance(input, held, ring).unwrap(),
            super::recording::staged::Advance::Primary
        ) {
            super::step(
                held,
                ring,
                Ok(()),
                &super::Mailbox::new(),
                Some(input.operation),
                super::Publication::Deferred,
            )
            .unwrap();
            if ring.caught_up().unwrap() {
                startup.primary(held).unwrap();
                return;
            }
        }
    }
    panic!("bounded startup did not become Primary");
}

pub(super) fn query_context(
    held: &mut super::Held,
    ring: &mut super::Ring,
) -> Arc<super::QueryContext> {
    match query_outcome(held, ring) {
        super::QueryOutcome::Ready(context) => context,
        outcome => panic!("expected query context: {outcome:?}"),
    }
}

pub(super) fn query_outcome(held: &mut super::Held, ring: &mut super::Ring) -> super::QueryOutcome {
    let queries = super::query::Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(super::QueryRequest {
            epoch: 0,
            source: None,
            selection: super::QuerySelection::FollowLatest,
        })
        .unwrap();
    super::run_queries(held, ring, &queries);
    handle.take().unwrap().outcome
}

pub(super) fn primary_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1));
    fixture.seal_minute(0);
    fixture.skip_run_ticks(116);
    fixture.publish_with_usage(1118, &usage(118));
    fixture.publish_with_usage(1119, &usage(119));
    fixture.seal_minute(1);
    fixture.publish_with_usage(1120, &usage(120));
    fixture.publish_with_usage(1121, &usage(121));
    fixture.seal_minute(2);
    fixture
}

#[test]
fn staged_history_and_native_clock_jumps_rebuild_cohorts_before_incremental_updates_resume() {
    // Goal: historical replacement cannot subtract rows that the incremental means never entered.
    // Method: upgrade old A, append real native N across its cutoff, finish, then roll back.
    let mut fixture = primary_fixture();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::with_payload(Arc::clone(&budget));
    held.startup = super::StartupState::Loading;
    let mut ring = super::native_acceptance::make_ring(&fixture.root, Some(&budget));
    let mut startup = super::recording::staged::Startup::new();
    let input = super::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    drive_primary(&mut startup, &input, &mut held, &mut ring);
    let primary = query_context(&mut held, &mut ring);
    startup.advance(&input, &mut held, &mut ring).unwrap();
    startup.advance(&input, &mut held, &mut ring).unwrap();
    assert_eq!(held.apps.get(0).unwrap().missing, None);
    assert_eq!(
        held.apps.get(2).unwrap().missing,
        Some(super::Missing::Unread)
    );

    publish_named(&mut fixture, 122, 2000);
    super::native_acceptance::read_native(&mut held, &mut ring, &operation).unwrap();
    let mut cohorts = super::Cohorts::with_room();
    held.context.choose(&mut cohorts);
    assert_eq!(cohorts.seconds, 0);
    let input = super::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    drive_complete(&mut startup, &input, &mut held, &mut ring);

    cohorts.clear();
    held.context.choose(&mut cohorts);
    assert_eq!(cohorts.seconds, 5);
    let mut names: Vec<_> = cohorts
        .resident
        .iter()
        .map(|app| app.bytes.as_slice())
        .collect();
    names.sort_unstable();
    assert_eq!(names, [b"/Apps/Current.app".as_slice(), b"N".as_slice()]);
    assert_eq!(primary.selected().observation.stamp.publication, 6);
    publish_named(&mut fixture, 123, 1000);
    super::native_acceptance::read_native(&mut held, &mut ring, &operation).unwrap();
    cohorts.clear();
    held.context.choose(&mut cohorts);
    assert_eq!(cohorts.seconds, 1);
    assert_eq!(cohorts.resident[0].bytes.as_slice(), b"N");
}

pub(super) fn drive_complete(
    startup: &mut super::recording::staged::Startup,
    input: &super::recording::staged::Input<'_>,
    held: &mut super::Held,
    ring: &mut super::Ring,
) {
    for _ in 0..super::recording::MINUTES_MAX * 2 {
        if matches!(
            startup.advance(input, held, ring).unwrap(),
            super::recording::staged::Advance::Complete
        ) {
            return;
        }
    }
    panic!("bounded startup did not become Complete");
}

fn publish_named(fixture: &mut Fixture, run_tick: u64, wall_second: u64) {
    let identity = publish_named_identity(fixture);
    let process = collector::processes::Process {
        pid: 7,
        usage: usage(run_tick),
        task: collector::processes::Task {
            threads: 1,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    };
    let counter = u32::try_from(run_tick).unwrap();
    fixture
        .writer
        .publish_with_identity(
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
                processes: &[process],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: collector::ring::Dropped::default(),
            },
            Some(&collector::ring::ObservationTimes {
                wall_ns: wall_second * 1_000_000_000,
                continuous_ns: run_tick * 1_000_000_000,
                awake_ns: run_tick * 1_000_000_000,
            }),
            &collector::ring::IdentityPublication {
                processes: &[identity],
                events: &[],
            },
        )
        .unwrap();
}

fn publish_named_identity(fixture: &mut Fixture) -> collector::ring::IdentityObservation {
    use collector::ring::{
        IdentityObservation, ProcessCreation, ProcessIdentity, ProgramBytes, UsageAvailability,
    };
    let mut program = None;
    fixture
        .writer
        .intern_program(
            &ProgramBytes {
                executable_name: b"N",
                app_path: None,
            },
            &mut program,
        )
        .unwrap();
    IdentityObservation {
        pid: 7,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: 7,
                pid_version: -7,
            },
            program: program.unwrap(),
        },
    }
}

#[test]
fn selected_apps_use_an_available_source_predecessor_below_wall_pruning() {
    // Goal: pruning by wall time cannot turn an available consecutive App boundary into a gap.
    // Method: jump the wall clock between run ticks 59 and 60 in two real sealed minutes.
    let fixture = pruned_boundary_fixture();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::with_payload(Arc::clone(&budget));
    held.startup = super::StartupState::Loading;
    let mut ring = super::native_acceptance::make_ring(&fixture.root, Some(&budget));
    let mut startup = super::recording::staged::Startup::new();
    let input = super::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };

    drive_primary(&mut startup, &input, &mut held, &mut ring);

    assert_eq!(held.apps.get(0).unwrap().observation.unwrap().run_tick, 60);
    let payload = held.apps.get(0).unwrap().processes.as_ref().unwrap();
    assert_eq!(payload.previous.unwrap().publication, 1);
    assert_eq!(
        payload
            .iter()
            .find(|process| process.pid == 7)
            .unwrap()
            .disk_read_bytes(),
        crate::apps::Amount::Known(17)
    );
}

#[test]
fn the_first_historical_sample_uses_its_own_pruned_source_boundary() {
    // Goal: history's first retained older sample has a separate exact source predecessor.
    // Method: keep only two recent slots after a wall-pruned boundary and replay its older rank.
    let mut fixture = pruned_boundary_fixture();
    fixture.skip_run_ticks(58);
    fixture.publish_with_usage(5060, &usage(120));
    fixture.publish_with_usage(5061, &usage(121));
    fixture.seal_minute(2);
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::with_payload(Arc::clone(&budget));
    held.span = super::Span::new(2, super::Buffers::new(3, super::MachineSecond::default));
    held.apps = super::Span::new(2, super::Buffers::new(3, AppsSecond::default));
    held.startup = super::StartupState::Loading;
    let mut ring = super::native_acceptance::make_ring(&fixture.root, Some(&budget));
    let mut startup = super::recording::staged::Startup::new();
    let input = super::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    drive_primary(&mut startup, &input, &mut held, &mut ring);

    drive_complete(&mut startup, &input, &mut held, &mut ring);

    let older = held.sampled.as_ref().unwrap().get(0).unwrap();
    assert_eq!(older.observation.stamp.publication, 2);
    let ranks = older.ranks().unwrap();
    let disk = ranks
        .metrics
        .iter()
        .find(|rank| rank.metric == crate::live_app_response::Metric::Disk)
        .unwrap();
    let app = disk
        .entries()
        .find(|entry| entry.app.bytes.as_slice() == b"/Apps/Current.app")
        .unwrap();
    assert_eq!(app.amount, 60);
    assert!((app.rate.unwrap() - 60.0).abs() <= f64::EPSILON * 60.0);
}

fn pruned_boundary_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_usage(1000, &usage(59));
    fixture.seal_minute(0);
    fixture.publish_with_usage(5000, &usage(60));
    fixture.publish_with_usage(5001, &usage(61));
    fixture.seal_minute(1);
    fixture
}

#[test]
fn unread_machine_retention_needs_no_admission_even_after_recent_and_older_evictions() {
    // Goal: the pre-Primary pool installation is infallible after machine admission.
    // Method: fill and evict the real default pools with zero free admitted bytes and no readers.
    let fixture = Fixture::new();
    let run = fixture.writer.identity().unwrap();
    let budget = Arc::new(ByteBudget::new(1024));
    let lease = budget.reserve(budget.maximum()).unwrap();
    let mut held = super::Held::with_payload(Arc::clone(&budget));
    held.startup = super::StartupState::Loading;

    for publication in 1..=1200 {
        let second = super::MachineSecond {
            publication,
            run_tick: publication - 1,
            wall_ns: publication * 2_000_000_000,
            ..super::MachineSecond::default()
        };
        let mut apps = AppsSecond {
            observation: Some(super::take_observation(&run, &second)),
            ..AppsSecond::default()
        };
        assert!(held.take(&run, &second, super::TickApps::Loaded(&mut apps)));
    }

    assert_eq!(held.span.len(), super::SECONDS_MAX);
    assert_eq!(held.apps.len(), super::SECONDS_MAX);
    assert_eq!(held.earlier.len(), 25);
    assert!(
        held.sampled
            .as_ref()
            .unwrap()
            .held()
            .all(|sample| matches!(sample.ranks(), Err(super::Missing::Unread)))
    );
    assert_eq!(budget.used(), budget.maximum());
    drop(lease);
}

#[test]
fn history_begins_at_its_first_represented_minute_without_opening_an_evicted_boundary() {
    // Goal: an unused prefix cannot consume a predecessor-only schedule slot.
    // Method: retain one recent/older pair from three loaded minutes after a pruned source minute.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_usage(1000, &usage(59));
    fixture.seal_minute(0);
    for minute in 1..=3 {
        if minute > 1 {
            fixture.skip_run_ticks(59);
        }
        fixture.publish_with_usage(5000 + minute * 60, &usage(minute * 60));
        fixture.seal_minute(minute);
    }
    let trace =
        persistence::work::ingestion::Trace::new(fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::with_payload(Arc::clone(&budget));
    held.span = super::Span::new(1, super::Buffers::new(2, super::MachineSecond::default));
    held.apps = super::Span::new(1, super::Buffers::new(2, AppsSecond::default));
    held.earlier = super::Span::new(1, super::Buffers::new(2, super::MachineSecond::default));
    held.sampled = Some(super::Span::new(
        1,
        super::Buffers::new(2, super::SampledSecond::empty),
    ));
    held.startup = super::StartupState::Loading;
    let mut ring = super::native_acceptance::make_ring(&fixture.root, Some(&budget));
    let mut startup = super::recording::staged::Startup::new();
    let input = super::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };

    drive_primary(&mut startup, &input, &mut held, &mut ring);
    drive_complete(&mut startup, &input, &mut held, &mut ring);

    assert_eq!(
        held.sampled
            .as_ref()
            .unwrap()
            .get(0)
            .unwrap()
            .observation
            .stamp
            .publication,
        3
    );
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    assert!(events.iter().all(|event| !matches!(
        event.key,
        persistence::work::ingestion::Key::Publication { publication: 1, .. }
    )));
    drop(scope);
}

#[test]
fn a_clockless_source_tail_is_handed_off_once_after_an_earlier_displayable_selection() {
    // Goal: selection and the source terminal remain independent across a clockless minute.
    // Method: select A, seal clockless B, then read real native C using B's exact predecessor.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(100, &usage(0));
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_usage(160, &usage(60));
    fixture.seal_window(
        Some(1),
        &[persistence::Observation {
            publication: 2,
            wall_ns: None,
            continuous_ns: Some(60_000_000_000),
            awake_ns: Some(60_000_000_000),
        }],
        &crate::live::identity_fixture::Underived,
    );
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = super::Held::with_payload(Arc::clone(&budget));
    held.startup = super::StartupState::Loading;
    let mut ring = super::native_acceptance::make_ring(&fixture.root, Some(&budget));
    let mut startup = super::recording::staged::Startup::new();
    let input = super::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    drive_primary(&mut startup, &input, &mut held, &mut ring);
    assert_eq!(held.newest.unwrap().publication, 1);

    fixture.publish_with_usage(161, &usage(61));
    super::native_acceptance::read_native(&mut held, &mut ring, &operation).unwrap();

    assert_eq!(held.newest.unwrap().publication, 3);
    assert_eq!(held.span.newest().unwrap().cpu_percent, None);
    assert!(!held.span.newest().unwrap().follows);
    let apps = held.apps.newest().unwrap();
    let payload = apps.processes.as_ref().unwrap();
    assert_eq!(payload.previous.unwrap().publication, 2);
    assert_eq!(apps.elapsed_ns, None);
    assert_eq!(
        payload
            .iter()
            .find(|process| process.pid == 7)
            .unwrap()
            .disk_read_bytes(),
        crate::apps::Amount::Unavailable(crate::apps::Unavailable::Interval)
    );
    let input = super::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    drive_complete(&mut startup, &input, &mut held, &mut ring);
}

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
    let charge = retained_app_charge(&reader);
    cancellation.cancel().unwrap();

    let result = super::step(
        &mut held,
        &mut ring,
        Ok(()),
        &mailbox,
        Some(&operation),
        super::Publication::Complete,
    );

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
    let charge = retained_app_charge(&reader);
    let mut ring = super::Ring::new(fixture.root.clone());
    let mailbox = super::Mailbox::new();
    let result = super::startup_accept(outcome, &mut held, &mut ring, &operation);
    let published = super::step(
        &mut held,
        &mut ring,
        Ok(()),
        &mailbox,
        Some(&operation),
        super::Publication::Complete,
    );

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
    assert_eq!(process.disk_read_bytes(), crate::apps::Amount::Known(17));
    assert_eq!(process.disk_written_bytes(), crate::apps::Amount::Known(43));
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
    let captures = AtomicUsize::new(0);
    let capture = |file, budget: &Arc<ByteBudget>| {
        let entry = budget.used();
        let input = persistence::engine::AdmittedTable::from_file_budgeted(file, budget)?;
        if input.contains_column("wall_ns") {
            assert_eq!(
                entry, 0,
                "only consumed completions may precede the next minute"
            );
            captures.fetch_add(1, Ordering::Relaxed);
        }

        Ok(input)
    };

    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let outcome = super::recording::load_payload_using(
        &fixture.root,
        &budget,
        &operation,
        &capture,
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
    assert_eq!(captures.load(Ordering::Relaxed), 3);
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
        let directory = fixture.root.join(manifest.data_directory);
        let path = directory.join("ticks.parquet");
        let file = std::fs::File::open(path).unwrap();
        let input = persistence::engine::AdmittedTable::from_file_budgeted(file, &budget).unwrap();
        peak = peak.max(43 * 1024 * 1024 + budget.used());
        for name in [
            "cpus",
            "memory",
            "interfaces",
            "gpu_states",
            "core_states",
            "fans",
            "processes",
            "events",
        ] {
            let file = std::fs::File::open(directory.join(format!("{name}.parquet"))).unwrap();
            let table =
                persistence::engine::AdmittedTable::from_file_budgeted(file, &budget).unwrap();
            peak = peak.max(43 * 1024 * 1024 + budget.used());
            drop(table);
        }
        drop(input);
    }
    assert_eq!(budget.used(), 0);
    peak
}

fn fixture_payload_charges(fixture: &Fixture) -> Vec<usize> {
    let budget = Arc::new(ByteBudget::new(usize::MAX));
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut charges = Vec::with_capacity(3);
    let outcome =
        super::recording::load_payload(&fixture.root, &budget, &operation, |_, _, apps| {
            let super::apps::TickApps::Loaded(apps) = apps else {
                panic!("valid fixture must load complete saved App rows");
            };
            assert_eq!(apps.processes.as_ref().unwrap().len(), 2);
            charges.push(retained_app_charge(apps));
        })
        .unwrap();
    assert_eq!(outcome.left_out, None);
    drop(outcome);
    assert!(!charges.is_empty());
    assert_eq!(budget.used(), 0);
    charges
}

pub(super) fn retained_app_charge(apps: &AppsSecond) -> usize {
    let payload = apps.processes.as_ref().unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut names = 0;
    for name in payload
        .iter()
        .filter_map(super::ProcessRow::executable_name)
    {
        if seen.insert(std::ptr::from_ref(name)) {
            names += name.reserved_bytes();
        }
    }
    payload.bytes()
        + names
        + apps.metadata.bytes()
        + apps.apps.reserved_bytes()
        + apps
            .apps
            .iter()
            .map(|app| app.name.reserved_bytes())
            .sum::<usize>()
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
    let captures = AtomicUsize::new(0);
    let capture = |file, budget: &Arc<ByteBudget>| {
        let entry = budget.used();
        let input = persistence::engine::AdmittedTable::from_file_budgeted(file, budget)?;
        if input.contains_column("wall_ns") {
            let index = captures.fetch_add(1, Ordering::Relaxed);
            assert_eq!(
                entry, expected[index],
                "only reader/pool owners may precede capture"
            );
        }

        Ok(input)
    };

    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let outcome = super::recording::load_payload_using(
        &fixture.root,
        &budget,
        &operation,
        &capture,
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
    assert_eq!(captures.load(Ordering::Relaxed), 4);
    let reader = reader.unwrap();
    assert_eq!(reader.observation.unwrap().stamp.publication, 1);
    assert_eq!(reader.processes.as_ref().unwrap().len(), 2);
    assert_eq!(budget.used(), charges[0] + charges[3]);
    drop(span);
    assert_eq!(budget.used(), charges[0]);
    drop(reader);
    assert_eq!(budget.used(), 0);
}
