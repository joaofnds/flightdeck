use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::live::identity_fixture::Fixture;
use crate::store::{ByteBudget, QueryHandle, QueryOutcome, QueryRequest, QuerySelection, Store};
use persistence::work::ingestion::{Kind, Trace};

#[test]
fn the_running_store_answers_a_covered_pin_without_replaying_its_sources() {
    // Goal: a real producer certificate serves immutable queries without another source reader.
    // Method: follow two native publications, pin their exact endpoint and compare work counts.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let trace = Trace::new(fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let store = Store::start_with_budget(&fixture.root, Arc::clone(&budget)).unwrap();
    let handle = store.query_handle().unwrap();
    let followed = ready(&handle, QuerySelection::FollowLatest);
    assert_eq!(followed.selected().observation.stamp.publication, 2);
    assert_eq!(followed.seconds().len(), 2);
    let selected = followed.selected().observation;
    let last = followed.context_last_second();
    let mut before = Vec::new();
    trace.snapshot(&mut before).unwrap();

    let pinned = ready(
        &handle,
        QuerySelection::Exact {
            observation: selected,
            context_last_second: last,
        },
    );

    assert_eq!(pinned.selected().observation, selected);
    assert_eq!(pinned.context_last_second(), last);
    assert!(pinned.selected().detached());
    let mut after = Vec::new();
    trace.snapshot(&mut after).unwrap();
    for kind in [
        Kind::RingRead,
        Kind::FrameCapture,
        Kind::AppDerivation,
        Kind::RawCapture,
        Kind::MachineReduction,
    ] {
        let count = |events: &[persistence::work::ingestion::Event]| {
            events.iter().filter(|event| event.kind == kind).count()
        };
        assert_eq!(count(&before), 2, "{kind:?}");
        assert_eq!(count(&after), 2, "{kind:?}");
    }
    drop(pinned);
    drop(followed);
    drop(handle);
    drop(store);
    assert_eq!(budget.used(), 0);
    drop(scope);
}

fn ready(handle: &QueryHandle, selection: QuerySelection) -> Arc<crate::store::QueryContext> {
    let deadline = Instant::now() + Duration::from_secs(5);
    handle
        .submit(QueryRequest {
            epoch: 0,
            source: None,
            selection,
        })
        .unwrap();
    for _ in 0..500 {
        if let Some(reply) = handle.take() {
            match reply.outcome {
                QueryOutcome::Ready(context) => return context,
                QueryOutcome::NeedsLoad(_) => {
                    handle
                        .submit(QueryRequest {
                            epoch: 0,
                            source: None,
                            selection,
                        })
                        .unwrap();
                }
                QueryOutcome::Unavailable(reason) => panic!("query unavailable: {reason:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "producer did not finish the checked context"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    panic!("bounded query attempts exhausted");
}
