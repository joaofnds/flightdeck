use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::live::identity_fixture::Fixture;
use crate::store::{ByteBudget, QueryContext, QueryOutcome, QueryRequest, QuerySelection, Store};
use crate::{Cancellation, LiveSession};

#[test]
fn a_native_selection_matches_legacy_measurements_after_its_source_is_gone() {
    // Goal: the GUI compatibility response contains exactly the original selected facts.
    // Method: freeze real LiveSession output, then remove every producer before formatting.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let mut legacy = LiveSession::open(&fixture.root).unwrap();
    legacy.poll().unwrap();
    let expected = legacy.follow_latest().unwrap();
    let context = retained(&fixture);
    drop(legacy);
    drop(fixture);

    let actual = context
        .selection(&Arc::new(Cancellation::default()), 0)
        .unwrap();

    assert_eq!(*actual, expected);
}

#[test]
fn all_eleven_raw_surfaces_match_the_native_selected_observation() {
    // Goal: raw demand preserves schemas, order, totals and bytes independently of the mapping.
    // Method: freeze all real native pages, then format them after producer/mapping release.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let mut legacy = LiveSession::open(&fixture.root).unwrap();
    legacy.poll().unwrap();
    legacy.select(100, 0).unwrap();
    let mut expected: Vec<_> = persistence::schema::TABLES
        .into_iter()
        .map(|table| (table.name(), legacy.page(table.name(), 0).unwrap()))
        .collect();
    expected.push((
        "core_level_states",
        legacy.page("core_level_states", 0).unwrap(),
    ));
    let context = retained(&fixture);
    drop(legacy);
    drop(fixture);
    let cancellation = Arc::new(Cancellation::default());

    for (table, expected) in expected {
        let actual = context.raw_page(table, 0, &cancellation, 0).unwrap();
        assert_eq!(*actual, expected, "{table}");
    }
}

#[test]
fn a_cancelled_selection_returns_no_response_or_new_retained_owner() {
    // Goal: superseded formatting cannot retain a partial result.
    // Method: cancel the operation before reading one complete canonical context.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let context = retained(&fixture);
    let cancellation = Arc::new(Cancellation::default());
    cancellation.cancel().unwrap();

    let result = context.selection(&cancellation, 0);

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
}

#[test]
fn supplied_wrong_clocks_cannot_relabel_a_retained_observation() {
    // Goal: the presentation boundary keeps exact clocks alongside run/boot/publication.
    // Method: alter only the wall clock of an otherwise matching real selected identity.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let context = retained(&fixture);
    let mut identity = context.catalog()["latest_observation"].clone();
    identity["wall_ns"] = serde_json::json!(100_000_000_001_u64);

    assert!(context.observation_for_identity(100, &identity).is_err());
}

#[test]
fn a_small_raw_page_needs_only_its_requested_rows_admitted() {
    // Goal: a tiny fan page remains available with only a small amount of free budget.
    // Method: retain a real context, reserve all but 64 KiB, then request its fan page.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let context = retained_with_budget(&fixture, &budget);
    let held = budget
        .reserve(budget.maximum() - budget.used() - 64 * 1024)
        .unwrap();
    let cancellation = Arc::new(Cancellation::default());

    let page = context.raw_page("fans", 0, &cancellation, 0).unwrap();

    assert_eq!(page["name"], "fans");
    assert!(page.bytes() < 64 * 1024);
    drop(page);
    drop(held);
}

fn retained(fixture: &Fixture) -> Arc<QueryContext> {
    retained_with_budget(fixture, &Arc::new(ByteBudget::new(256 * 1024 * 1024)))
}

fn retained_with_budget(fixture: &Fixture, budget: &Arc<ByteBudget>) -> Arc<QueryContext> {
    let store = Store::start_with_budget(&fixture.root, Arc::clone(budget)).unwrap();
    let handle = store.query_handle().unwrap();
    let request = QueryRequest {
        epoch: 0,
        source: None,
        selection: QuerySelection::FollowLatest,
    };
    handle.submit(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    for _ in 0..500 {
        if let Some(reply) = handle.take() {
            match reply.outcome {
                QueryOutcome::Ready(context) => return context,
                QueryOutcome::NeedsLoad(_) => {
                    handle.submit(request).unwrap();
                }
                QueryOutcome::Unavailable(reason) => panic!("unavailable context: {reason:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "the actual Store did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    panic!("query bound exhausted");
}
