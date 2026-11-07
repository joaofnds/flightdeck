use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::live::identity_fixture::Fixture;
use crate::live_app_response::{APPS_PAGE_ROWS_MAX, Page};
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
fn an_apps_page_lists_every_app_one_second_holds() {
    // Goal: the retained context admits a page as long as a second's most Apps, so the App
    // picker sorts and searches every App rather than one page of them.
    // Method: retain a real context, then request its selected second's Apps on one such page.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let context = retained(&fixture);
    let identity = context.catalog()["latest_observation"].clone();
    let observation = context.observation_for_identity(100, &identity).unwrap();
    let page = Page {
        offset: 0,
        limit: APPS_PAGE_ROWS_MAX,
    };

    let response = context
        .apps_page_for(&observation, page, &Arc::new(Cancellation::default()), 0)
        .unwrap();

    assert_eq!(
        response["page"]["total"],
        response["apps"].as_array().unwrap().len()
    );
    assert_eq!(response["page"]["next_offset"], serde_json::Value::Null);
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
