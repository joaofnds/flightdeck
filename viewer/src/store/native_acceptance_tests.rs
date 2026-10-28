use super::*;
use crate::apps::Amount;
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::store::startup_tests::usage;
use crate::store::{AppRow, AppsSecond};
use persistence::work::ingestion::{Kind, Trace};

#[test]
fn sampled_ranking_admission_precedes_native_eviction_during_staged_history() {
    // Goal: a resolved saved Recent owner survives an older ranking reservation denial.
    // Method: use two real retained slots, deny after native preparation, then release and retry.
    use crate::store::startup_tests::{drive_complete, primary_fixture};
    let mut fixture = primary_fixture();
    let trace = Trace::new(fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let input = crate::store::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    let (mut held, mut ring, mut startup) = staged_small_span(&input);
    assert_eq!(recent_publication(&held, 0), 5);
    assert_eq!(
        held.sampled
            .as_ref()
            .unwrap()
            .newest()
            .unwrap()
            .observation
            .stamp
            .publication,
        3
    );
    fixture.publish_with_usage(1122, &usage(122));
    let lease = deny_sampled_take(&mut held, &mut ring, &budget, &operation);
    assert_eq!(held.newest.unwrap().publication, 6);
    assert_eq!(recent_publication(&held, 0), 5);
    assert_eq!(held.earlier.newest().unwrap().publication, 3);
    assert!(!ring.caught_up().unwrap());

    drop(lease);
    read_native(&mut held, &mut ring, &operation).unwrap();
    assert_eq!(held.newest.unwrap().publication, 7);
    let older = held.sampled.as_ref().unwrap().newest().unwrap();
    assert_eq!(older.observation.stamp.publication, 5);
    assert_eq!(older.ranks().unwrap().app_count, 2);
    let input = crate::store::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    drive_complete(&mut startup, &input, &mut held, &mut ring);

    assert!(held.startup == crate::store::StartupState::Complete);
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    for publication in [5, 7] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == Kind::AppDerivation)
                .filter(
                    |event| matches!(event.key, persistence::work::ingestion::Key::Publication {
                publication: value, .. } if value == publication)
                )
                .count(),
            1
        );
    }
    drop(scope);
}

fn recent_publication(held: &Held, index: usize) -> u64 {
    held.apps
        .get(index)
        .unwrap()
        .observation
        .unwrap()
        .stamp
        .publication
}

#[test]
fn retired_private_apps_release_budget_before_the_next_real_source_capture() {
    // Goal: a retired private pool owner cannot obstruct admitted input after its reader leaves.
    // Method: evict real native A with B and keep its reader across a denied sealed-input capture.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(100, &usage(0));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = Held::with_payload(Arc::clone(&budget));
    held.span = crate::store::Span::new(
        1,
        crate::store::Buffers::new(2, crate::store::MachineSecond::default),
    );
    held.apps = crate::store::Span::new(1, crate::store::Buffers::new(2, AppsSecond::default));
    let mut ring = make_ring(&fixture.root, Some(&budget));
    ring.follow().unwrap();
    read_native(&mut held, &mut ring, &operation).unwrap();
    let reader = Arc::clone(held.apps.newest_held().unwrap());
    fixture.publish_with_usage(101, &usage(1));
    read_native(&mut held, &mut ring, &operation).unwrap();
    fixture.seal();
    let probe = Arc::new(ByteBudget::new(usize::MAX));
    drop(capture_ticks(&fixture, &probe).unwrap());
    let required = probe.used_peak();
    assert!(required > 0);
    assert!(required < budget.maximum() - budget.used());
    let lease = budget
        .reserve(budget.maximum() - budget.used() - required + 1)
        .unwrap();
    assert!(matches!(
        capture_ticks(&fixture, &budget),
        Err(persistence::engine::AdmissionRefusal::Budget)
    ));

    held.release_views();
    assert!(matches!(
        capture_ticks(&fixture, &budget),
        Err(persistence::engine::AdmissionRefusal::Budget)
    ));
    assert_eq!(reader.observation.unwrap().stamp.publication, 1);
    assert_eq!(reader.processes.as_ref().unwrap().len(), 2);
    drop(reader);
    read_native(&mut held, &mut ring, &operation).unwrap();
    let input = capture_ticks(&fixture, &budget).unwrap();

    assert_eq!(held.newest.unwrap().publication, 2);
    assert_eq!(
        held.apps
            .newest()
            .unwrap()
            .processes
            .as_ref()
            .unwrap()
            .previous
            .unwrap()
            .publication,
        1
    );
    drop((input, lease, ring, held, fixture));
    assert_eq!(budget.used(), 0);
}

fn capture_ticks(
    fixture: &Fixture,
    budget: &Arc<ByteBudget>,
) -> Result<persistence::engine::AdmittedTable, persistence::engine::AdmissionRefusal> {
    let marker = persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let file = collector::recording::private::open_private_file(
        &fixture
            .root
            .join(manifest.data_directory)
            .join("ticks.parquet"),
        false,
    )
    .unwrap();
    persistence::engine::AdmittedTable::from_file_budgeted(file, budget)
}

fn staged_small_span(
    input: &crate::store::recording::staged::Input<'_>,
) -> (Held, Ring, crate::store::recording::staged::Startup) {
    let (mut held, mut ring, mut startup) = staged_before_primary(input);
    crate::store::startup_tests::drive_primary(&mut startup, input, &mut held, &mut ring);
    (held, ring, startup)
}

fn staged_before_primary(
    input: &crate::store::recording::staged::Input<'_>,
) -> (Held, Ring, crate::store::recording::staged::Startup) {
    let mut held = Held::with_payload(Arc::clone(input.budget));
    held.span = crate::store::Span::new(
        2,
        crate::store::Buffers::new(3, crate::store::MachineSecond::default),
    );
    held.apps = crate::store::Span::new(2, crate::store::Buffers::new(3, AppsSecond::default));
    held.startup = crate::store::StartupState::Loading;
    let mut ring = make_ring(input.root, Some(input.budget));
    let mut startup = crate::store::recording::staged::Startup::new();
    for _ in 0..crate::store::recording::MINUTES_MAX * 2 {
        if matches!(
            startup.advance(input, &mut held, &mut ring).unwrap(),
            crate::store::recording::staged::Advance::Primary
        ) {
            crate::store::step(
                &mut held,
                &mut ring,
                Ok(()),
                &crate::store::Mailbox::new(),
                Some(input.operation),
                crate::store::Publication::Deferred,
            )
            .unwrap();
            return (held, ring, startup);
        }
    }
    panic!("bounded startup never reached its native barrier");
}

#[test]
fn the_primary_barrier_records_native_retention_backpressure_before_retrying() {
    // Goal: a fully prepared pre-Primary candidate cannot turn rank denial into a busy loop.
    // Method: deny a real departing saved rank, then drive the actual Store startup turn.
    let mut fixture = crate::store::startup_tests::primary_fixture();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let input = crate::store::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    let (mut held, mut ring, mut startup) = staged_before_primary(&input);
    assert!(held.startup == crate::store::StartupState::Loading);
    fixture.publish_with_usage(1122, &usage(122));
    let lease = deny_sampled_take(&mut held, &mut ring, &budget, &operation);
    let shared = startup_shared();
    let mut blocked = None;
    let input = crate::store::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };

    let complete = crate::store::startup_staged_turn(
        &input,
        &mut held,
        &mut ring,
        &shared,
        &mut startup,
        &mut blocked,
    )
    .unwrap();

    assert!(!complete);
    assert_eq!(held.newest.unwrap().publication, 6);
    assert_eq!(held.failure, None);
    assert!(
        blocked.is_some(),
        "native retention denial was lost at the Primary barrier"
    );
    drop(lease);
    fixture.publish_with_usage(1123, &usage(123));
    let input = crate::store::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    assert!(
        !crate::store::startup_staged_turn(
            &input,
            &mut held,
            &mut ring,
            &shared,
            &mut startup,
            &mut blocked
        )
        .unwrap()
    );
    assert!(blocked.is_none());
    assert!(held.startup == crate::store::StartupState::Primary);
    assert_eq!(held.newest.unwrap().publication, 8);
    assert!(ring.caught_up().unwrap());
}

#[test]
fn a_blocked_primary_barrier_rechecks_the_existing_source_without_discarding_its_pending_run() {
    // Goal: capacity waiting cannot suspend the ring's existing source-control contract.
    // Method: keep old B pending across a newer registration and corrupt/restore root identity.
    let mut fixture = crate::store::startup_tests::primary_fixture();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let input = crate::store::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    let (mut held, mut ring, mut startup) = staged_before_primary(&input);
    fixture.publish_with_usage(1122, &usage(122));
    let lease = deny_sampled_take(&mut held, &mut ring, &budget, &operation);
    let shared = startup_shared();
    let mut blocked = None;
    let mut later = Fixture::new_mode(false, 2, Some(&fixture));
    later.publish_with_system_rows(2000);
    let identity = fixture.writer.identity().unwrap();
    ring.resume().unwrap();
    assert_eq!(ring.identity(), Some(identity));
    let original = std::fs::read(fixture.root.join("identity")).unwrap();
    std::fs::write(fixture.root.join("identity"), b"broken").unwrap();
    let input = crate::store::recording::staged::Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };

    poll_blocked_source(
        &input,
        &mut held,
        &mut ring,
        &shared,
        &mut startup,
        &mut blocked,
    );

    assert!(blocked.is_some());
    assert_eq!(held.newest.unwrap().publication, 6);
    assert_eq!(ring.identity(), Some(identity));
    assert_eq!(
        held.failure,
        Some(Failure::Recording(std::io::ErrorKind::InvalidData))
    );
    std::fs::write(fixture.root.join("identity"), original).unwrap();
    poll_blocked_source(
        &input,
        &mut held,
        &mut ring,
        &shared,
        &mut startup,
        &mut blocked,
    );
    assert_eq!(held.failure, None);
    assert_eq!(ring.identity(), Some(identity));
    assert_eq!(held.newest.unwrap().publication, 6);
    drop((lease, later));
}

fn poll_blocked_source(
    input: &crate::store::recording::staged::Input<'_>,
    held: &mut Held,
    ring: &mut Ring,
    shared: &crate::store::Shared,
    startup: &mut crate::store::recording::staged::Startup,
    blocked: &mut Option<crate::store::StartupCapacity>,
) {
    for _ in 0..crate::store::ring::FOLLOW_STEPS.saturating_mul(2) {
        // A mailbox wake uses the same existing park token; no timing threshold is asserted.
        std::thread::current().unpark();
        assert!(
            !crate::store::startup_staged_turn(input, held, ring, shared, startup, blocked)
                .unwrap()
        );
    }
}

fn startup_shared() -> crate::store::Shared {
    use std::sync::atomic::AtomicBool;
    crate::store::Shared {
        mailbox: crate::store::Mailbox::new(),
        queries: Some(crate::store::query::Queries::new()),
        stop: AtomicBool::new(false),
        active: AtomicBool::new(true),
        catch_up: AtomicBool::new(false),
        catching_up: AtomicBool::new(true),
        paused: AtomicBool::new(false),
        cancellation: Arc::new(Cancellation::default()),
    }
}

fn deny_sampled_take(
    held: &mut Held,
    ring: &mut Ring,
    budget: &Arc<ByteBudget>,
    operation: &Operation,
) -> crate::store::ByteReservation {
    let mut lease = None;
    let previous = held.apps.newest_held().map(Arc::clone);
    let mut refused = false;
    let result = ring
        .read_prepared(budget, operation, previous.as_deref(), |run, prepared| {
            lease = Some(budget.reserve(budget.maximum() - budget.used()).unwrap());
            assert!(!take(held, run, prepared, &mut refused));
            assert_eq!(prepared.apps.observation.unwrap().stamp.publication, 7);
            false
        })
        .unwrap();
    assert_eq!(result, NativeRead::Blocked);
    assert!(!refused);
    lease.unwrap()
}

#[test]
fn accepted_app_rows_keep_the_capacity_their_reservation_admitted() {
    // Goal: transferring native facts into retention cannot enlarge their charged capacity.
    // Method: ingest one real publication and compare the accepted rows with its admission.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(100, &usage(0));
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut held = Held::new();
    held.enable_payload(Arc::clone(&budget));
    let mut ring = make_ring(&fixture.root, Some(&budget));
    ring.follow().unwrap();

    read_native(&mut held, &mut ring, &operation).unwrap();

    let apps = held.apps.newest().unwrap();
    assert!(!apps.apps.is_empty());
    let capacity = apps.apps.capacity() * size_of::<AppRow>();
    assert_eq!(apps.apps.reserved_bytes(), capacity);
    assert!(apps.metadata.bytes() >= size_of::<AppsSecond>());
}

#[test]
fn cleared_native_app_storage_remains_charged_until_its_capacity_is_freed() {
    // Goal: clearing facts cannot release the charge for row storage that remains allocated.
    // Method: capture native rows, clear them, and compare retained capacity with its budget.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(100, &usage(0));
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut second = AppsSecond::with_room();
    capture_native_rows(&fixture, &budget, &mut second, 1);
    assert!(!second.apps.is_empty());
    assert!(second.metadata.bytes() > 0);

    second.take(
        TickApps::Missing(&crate::store::Missing::Unread),
        None,
        None,
    );

    assert!(second.apps.is_empty());
    let retained_rows = second.apps.capacity() * size_of::<AppRow>();
    assert!(
        budget.used() >= retained_rows,
        "cleared rows retain {retained_rows} bytes with only {} bytes reserved",
        budget.used()
    );
    drop(second);
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_native_app_pool_slot_accepts_the_rows_of_a_saved_observation() {
    // Goal: a slot reused after native ingestion can accept a saved observation's App rows.
    // Method: capture native rows, seal that tick, and load its saved facts into the same slot.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(100, &usage(0));
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut second = AppsSecond::with_room();
    capture_native_rows(&fixture, &budget, &mut second, 1);
    let expected_count = second.apps.len();
    fixture.seal();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut loaded_count = 0;

    let outcome =
        crate::store::recording::load_payload(&fixture.root, &budget, &operation, |_, _, apps| {
            second.take(apps, None, Some(&budget));
            loaded_count += 1;
        })
        .unwrap();

    assert_eq!(outcome.left_out, None);
    assert_eq!(loaded_count, 1);
    assert_eq!(second.apps.len(), expected_count);
    assert_eq!(
        second.metadata.origin,
        crate::store::native::Origin::Saved(persistence::Losses::default())
    );
    assert_eq!(
        second.apps.reserved_bytes(),
        expected_count * size_of::<AppRow>()
    );
    drop(outcome);
    fixture.publish_with_usage(101, &usage(1));
    capture_native_rows(&fixture, &budget, &mut second, 2);
    assert_eq!(second.observation.unwrap().stamp.publication, 2);
    assert_eq!(
        second.metadata.origin,
        crate::store::native::Origin::Live(persistence::Losses::default())
    );
    assert_eq!(
        second.apps.reserved_bytes(),
        expected_count * size_of::<AppRow>()
    );
    drop(second);
    assert_eq!(budget.used(), 0);
}

#[test]
fn refused_app_row_growth_preserves_the_previous_allocation_and_can_retry() {
    // Goal: capacity refusal leaves admitted facts intact and a later retry can replace them.
    // Method: capture real rows, exhaust growth room, then free pressure and retry admission.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(100, &usage(0));
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut second = AppsSecond::with_room();
    capture_native_rows(&fixture, &budget, &mut second, 1);
    let pointer = second.apps.as_ptr();
    let old_count = second.apps.len();
    let old_bytes = second.apps.reserved_bytes();
    let count = second.apps.capacity() + 1;
    let bytes = count * size_of::<AppRow>();
    let pressure = budget
        .reserve(budget.maximum() - budget.used() - bytes + 1)
        .unwrap();
    let before = budget.used();

    let refused = second.apps.replace_admitted(count, &budget);

    assert_eq!(refused, Err(crate::store::Missing::Budget));
    assert_eq!(second.apps.as_ptr(), pointer);
    assert_eq!(second.apps.len(), old_count);
    assert_eq!(second.apps.reserved_bytes(), old_bytes);
    assert_eq!(budget.used(), before);
    drop(pressure);
    second.apps.replace_admitted(count, &budget).unwrap();
    assert_eq!(second.apps.capacity(), count);
    assert_eq!(second.apps.reserved_bytes(), bytes);
    assert!(second.apps.is_empty());
    drop(second);
    assert_eq!(budget.used(), 0);
}

#[test]
fn resetting_the_store_preserves_charges_for_retained_app_row_capacity() {
    // Goal: resetting an epoch cannot release reservations for reusable pool allocations.
    // Method: ingest real rows, reset, and inspect every released slot's physical row owner.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(100, &usage(0));
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut held = Held::new();
    held.enable_payload(Arc::clone(&budget));
    let mut ring = make_ring(&fixture.root, Some(&budget));
    ring.follow().unwrap();
    read_native(&mut held, &mut ring, &operation).unwrap();
    let bytes = held.apps.newest().unwrap().apps.reserved_bytes();
    assert!(bytes > 0);
    drop(ring);

    held.reset();

    assert!(held.apps.is_empty());
    let mut retained = 0;
    held.apps
        .release(|apps| retained += apps.apps.reserved_bytes());
    assert_eq!(retained, bytes);
    assert!(budget.used() >= retained);
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn unreserved_app_row_conversion_refuses_an_oversized_capacity() {
    // Goal: a public transfer cannot introduce row storage past the existing App bound.
    // Method: transfer an empty Vec whose capacity exceeds that bound.
    let rows = Vec::with_capacity(crate::store::APPS_PER_SECOND_MAX + 1);

    let result = crate::store::AppRows::try_from(rows);

    assert_eq!(result, Err(crate::store::Missing::TooMany));
}

fn capture_native_rows(
    fixture: &Fixture,
    budget: &Arc<ByteBudget>,
    second: &mut AppsSecond,
    count: u64,
) {
    assert!(count <= 2);
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = make_ring(&fixture.root, Some(budget));
    ring.follow().unwrap();

    for _ in 0..count {
        let read = ring
            .read_prepared(budget, &operation, None, |_, prepared| {
                second.take(TickApps::Loaded(&mut prepared.apps), None, Some(budget));
                true
            })
            .unwrap();

        assert_eq!(read, NativeRead::Accepted);
    }
}

#[test]
fn the_complete_store_retains_native_facets_from_one_ingestion() {
    // Goal: ordinary complete Store steps retain measured facts without a second producer.
    // Method: ingest two real publications, then read summary/process/raw facts and work counts.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(100, &usage(0));
    fixture.publish_with_usage(101, &usage(1));
    let identity = fixture.writer.identity().unwrap();
    let trace = Trace::new(identity.recording);
    let scope = trace.enter();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let mut held = Held::new();
    held.enable_payload(Arc::clone(&budget));
    let mut ring = make_ring(&fixture.root, Some(&budget));
    crate::store::startup(&fixture.root, &mut held, &mut ring, &operation).unwrap();
    let mailbox = crate::store::Mailbox::new();
    for _ in 0..2 {
        crate::store::step(
            &mut held,
            &mut ring,
            Ok(()),
            &mailbox,
            Some(&operation),
            crate::store::Publication::Complete,
        )
        .unwrap();
    }
    let view = mailbox.take().unwrap();
    assert_eq!(view.seconds.len(), 2);
    assert_eq!(view.apps.len(), 2);
    assert!(view.ring_complete);
    assert_eq!(view.seconds[1].cpu_percent, Some(50.0));
    let apps = &view.apps[1];
    let process = apps
        .processes
        .as_ref()
        .unwrap()
        .iter()
        .find(|process| process.pid == 7)
        .unwrap();
    assert_eq!(process.cpu_ns(), Amount::Known(100_000_000));
    let observation = apps.observation.unwrap();
    let raw = held.raw.as_ref().unwrap().find(&observation).unwrap();
    let page = raw.raw().page("ticks", 0, &operation).unwrap();
    assert_eq!(page.integer(&page.rows[0], "publication").unwrap(), Some(2));
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    for kind in [
        Kind::RingRead,
        Kind::FrameCapture,
        Kind::AppDerivation,
        Kind::RawCapture,
        Kind::MachineReduction,
        Kind::Accepted,
    ] {
        assert_eq!(
            events.iter().filter(|event| event.kind == kind).count(),
            2,
            "{kind:?}"
        );
    }
    drop(raw);
    drop(view);
    drop(ring);
    drop(held);
    assert_eq!(budget.used(), 0);
    drop(scope);
}
