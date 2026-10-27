use super::*;
use crate::apps::Amount;
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::store::startup_tests::usage;
use crate::store::{AppRow, AppsSecond};
use persistence::work::ingestion::{Kind, Trace};

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
