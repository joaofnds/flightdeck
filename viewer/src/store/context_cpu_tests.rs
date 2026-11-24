use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cpu_history::{CpuHistory, CpuHistoryIdentity, CpuHistoryObservation, CpuHistoryRow};
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
    let selected = crate::store::context_response::identity(
        &context.selected().observation,
        context.selected().apps().origin(),
    );
    let selected_publication = context.selected().observation.stamp.publication;

    let actual_detail = context.cpu_detail(&cancellation, 0).unwrap();
    let actual_history = context.cpu_history(0, &cancellation, 0).unwrap();

    assert_eq!(*actual_detail, detail);
    assert_eq!(actual_history.json(&selected), history);
    assert_eq!(
        actual_history.observations[actual_history.selected_index]
            .identity
            .publication,
        selected_publication
    );
}

#[test]
fn a_cpu_history_whose_charge_passes_the_response_bound_is_refused() {
    // Goal: the charge bound still holds the history and what the page builds beside it, which
    // the JSON path held by sizing the JSON.
    // Method: charge a history whose one row has room for half the bound in readings and holds
    // none, so that twice the row's room passes the bound by the size of the rest.
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let reservation = budget.reserve(super::HISTORY_CHARGE_BYTES_MAX).unwrap();
    let readings = super::HISTORY_CHARGE_BYTES_MAX / 2 / size_of::<Option<f64>>();
    let history = CpuHistory {
        selected_index: 0,
        first_ns: 0,
        end_ns: 0,
        offset: 0,
        total: 1,
        retention_limited: false,
        observations: Vec::new(),
        cpus: vec![CpuHistoryRow {
            cpu: 0,
            busy_percent: Vec::with_capacity(readings),
        }],
    };

    let result = super::cpu_history_response(reservation, history);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound("CPU history response bytes"))
    ));
}

#[test]
fn the_largest_cpu_history_the_store_builds_is_admitted() {
    // Goal: the charge bound admits every history the Store can build, so no context is refused
    // for its size alone.
    // Method: charge a history of the most observations, the page's 32 rows and identity texts
    // of the most bytes `Identity::read` admits, each a predecessor's too.
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let reservation = budget.reserve(super::HISTORY_CHARGE_BYTES_MAX).unwrap();
    let identity = || CpuHistoryIdentity {
        run: "0".repeat(super::IDENTITY_TEXT_BYTES_MAX),
        boot: "0".repeat(super::IDENTITY_TEXT_BYTES_MAX),
        publication: 0,
        run_tick: 0,
        wall_ns: 0,
    };
    let history = CpuHistory {
        selected_index: 0,
        first_ns: 0,
        end_ns: 0,
        offset: 0,
        total: 32,
        retention_limited: false,
        observations: (0..crate::session::OBSERVATIONS_MAX)
            .map(|_| CpuHistoryObservation {
                identity: identity(),
                previous: Some(identity()),
                interval_available: true,
                cpu_count: 32,
            })
            .collect(),
        cpus: (0..32)
            .map(|cpu| CpuHistoryRow {
                cpu,
                busy_percent: vec![Some(0.0); crate::session::OBSERVATIONS_MAX],
            })
            .collect(),
    };

    let result = super::cpu_history_response(reservation, history);

    assert!(result.is_ok());
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
