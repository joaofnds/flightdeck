use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::live::identity_fixture::Fixture;
use crate::store::context_response::tests::losses_comparable;
use crate::store::{ByteBudget, QueryContext, QueryOutcome, QueryRequest, QuerySelection, Store};
use crate::{Cancellation, LiveSession};

#[test]
fn native_cpu_presentations_match_the_saved_minutes_after_their_source_is_gone() {
    // Goal: CPU detail and history preserve the exact accepted measurements and identities.
    // Method: read the saved minutes' CPU output, then drop every source before the Store answers.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal();
    let mut saved = LiveSession::open(&fixture.root).unwrap();
    fixture.seal();
    saved.list_saved().unwrap();
    crate::live::select_newest(&mut saved);
    let detail = saved.cpu_detail().unwrap();
    let history = saved.cpu_history(0).unwrap();
    let context = retained(&fixture);
    drop(saved);
    drop(fixture);
    let cancellation = Arc::new(Cancellation::default());

    let actual_detail = context.cpu_detail(&cancellation, 0).unwrap();
    let actual_history = context.cpu_history(0, &cancellation, 0).unwrap();

    assert_eq!(*actual_detail, detail);
    assert_eq!(*actual_history, history);
}

#[test]
fn cpu_detail_rejects_a_changed_selected_clock() {
    // Goal: borrowed CPU results cannot be attached to a relabelled observation.
    // Method: change only the selected wall clock of an otherwise valid retained identity.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let context = retained(&fixture);
    let mut observation = context.selected().observation;
    observation.wall_ns = Some(100_000_000_001);

    let result = context.cpu_detail_for(&observation, &Arc::new(Cancellation::default()), 0);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound("CPU selection"))
    ));
}

#[test]
fn cpu_detail_preserves_a_predecessor_above_the_displayed_wall_range() {
    // Goal: clock rollback preserves the true previous identity without inventing a rate.
    // Method: follow a lower-wall publication and compare detail with the saved minute's.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(10_000);
    fixture.publish_with_system_rows(100);
    let context = retained(&fixture);
    fixture.seal();
    let mut saved = LiveSession::open(&fixture.root).unwrap();
    saved.select(100, 0).unwrap();
    let mut expected = saved.cpu_detail().unwrap();

    let mut actual = context
        .cpu_detail(&Arc::new(Cancellation::default()), 0)
        .unwrap()
        .clone();

    losses_comparable(&mut actual);
    losses_comparable(&mut expected);
    assert_eq!(actual, expected);
}

fn retained(fixture: &Fixture) -> Arc<QueryContext> {
    let store =
        Store::start_with_budget(&fixture.root, Arc::new(ByteBudget::new(256 * 1024 * 1024)))
            .unwrap();
    let handle = store.query_handle();
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
            "the Store did not finish its CPU context"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    panic!("bounded CPU query attempts exhausted");
}
