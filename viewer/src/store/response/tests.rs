use std::sync::Arc;

use crate::live::identity_fixture::Fixture;
use crate::live::window::fixture::saved_only;
use crate::live_app_response::{KeyKind, Page};
use crate::store::query::{CheckedCoverage, Queries};
use crate::store::{
    ByteBudget, Held, QueryContext, QueryOutcome, QueryRequest, QuerySelection, SourceGeneration,
};
use crate::{LiveSession, ObservationKey};

#[test]
fn retained_selected_and_window_ranks_match_the_saved_live_session() {
    // Goal: saved context ranking preserves every metric, identity and legacy tie order.
    // Method: compare selected and window ranks with LiveSession over the real saved fixture.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let selected = session.selected_ranks().unwrap();
    session.rank_window().unwrap();
    let expected = session.app_ranks();
    let context = saved_context(&fixture);
    drop(session);
    drop(fixture);

    let shown = context.app_ranks().unwrap();

    assert_eq!(&*context.selected_ranks().unwrap(), &selected);
    assert_eq!(selected.selected.stamp.publication, 2);
    assert_eq!(selected.metrics[0].entries[0].amount, 1);
    assert_eq!(selected.metrics[1].entries[0].amount, 11);
    assert_window_ranks(&context, &shown, &expected);
}

#[test]
fn retained_selected_and_window_ranks_match_the_native_live_session() {
    // Goal: real native App rankings need no source after the context is admitted.
    // Method: compare native LiveSession ranks after retiring the fixture and session.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.poll().unwrap();
    session.select(101, 0).unwrap();
    let expected = session.selected_ranks().unwrap();
    let context = native_context(&fixture);
    drop(session);
    drop(fixture);

    let shown = context.selected_ranks().unwrap();

    assert_eq!(&*shown, &expected);
    assert_eq!(shown.selected.stamp.publication, 2);
}

#[test]
fn a_selected_ranking_keeps_its_names_without_retaining_the_source_context() {
    // Goal: independently owned ranking values survive without pinning Store pool slots.
    // Method: release its window and source, then inspect names and the retired context.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.poll().unwrap();
    session.select(101, 0).unwrap();
    let expected = session.selected_ranks().unwrap();
    let context = native_context(&fixture);
    let budget = context.response_budget();
    let owner = Arc::downgrade(&context);
    let window = context.app_ranks().unwrap();

    let shown = window
        .selected_ranks_for(&context.selected().observation)
        .unwrap();
    drop(window);
    drop(context);
    drop(session);
    drop(fixture);

    assert_eq!(&*shown, &expected);
    assert!(owner.upgrade().is_none());
    assert!(shown.metrics[0].entries[0].app.reserved_bytes() > 0);
    drop(shown);
    assert!(owner.upgrade().is_none());
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_ranking_window_refuses_changed_clocks_even_when_the_publication_key_matches() {
    // Goal: a cached hover never substitutes its own observation for a caller's exact identity.
    // Method: change only the wall clock of a retained saved observation.
    let fixture = saved_only();
    let context = saved_context(&fixture);
    let window = context.app_ranks().unwrap();
    let mut observation = context.selected().observation;
    observation.wall_ns = observation.wall_ns.map(|wall| wall + 1);

    let result = window.selected_ranks_for(&observation);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound("selected ranks"))
    ));
}

#[test]
fn a_cached_ranking_retries_after_its_copy_admission_is_refused() {
    // Goal: copying a cached ranking respects the same budget without damaging its source.
    // Method: reserve the remaining bytes, refuse a hover copy, then release and retry.
    let fixture = saved_only();
    let context = saved_context(&fixture);
    let window = context.app_ranks().unwrap();
    let observation = context.selected().observation;
    let budget = context.response_budget();
    let blocked = budget.reserve(budget.maximum() - budget.used()).unwrap();

    let refused = window.selected_ranks_for(&observation);
    drop(blocked);
    let retried = window.selected_ranks_for(&observation).unwrap();

    assert!(matches!(
        refused,
        Err(persistence::Error::Bound(
            "Store presentation admission bytes"
        ))
    ));
    assert_eq!(retried.selected, observation);
}

#[test]
fn borrowed_context_parts_match_saved_details_without_a_new_context_lease() {
    // Goal: hover parts preserve exact saved JSON while sharing canonical context ownership.
    // Method: freeze legacy detail, create the borrowed view and drop every other source owner.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1000, 0).unwrap();
    let expected = session
        .app_detail(KeyKind::Bundle, b"/Apps/Current.app", 0)
        .unwrap();
    let context = saved_context(&fixture);
    let identity = key(&context.seconds()[0].observation);
    let parts = context
        .app_parts(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();
    drop(context);
    drop(session);
    drop(fixture);

    let part = parts.get(&identity).unwrap();
    let shown = part
        .detail(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();

    assert_eq!(&*shown, &expected);
    assert_eq!(part.selected().stamp.publication, 1);
    assert!(
        parts
            .get(&ObservationKey {
                publication: 999,
                ..identity
            })
            .is_none()
    );
}

#[test]
fn a_borrowed_part_of_an_absent_app_is_null_and_preserves_apps_paging() {
    // Goal: absent App detail is null without changing the surrounding sample's paging facts.
    // Method: request a nonexistent key from a complete real retained context.
    let fixture = saved_only();
    let context = saved_context(&fixture);
    let identity = key(&context.selected().observation);
    let parts = context.app_parts(KeyKind::Executable, b"absent").unwrap();

    let part = parts.get(&identity).unwrap();
    let detail = part
        .detail(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();
    let paging = part
        .apps_paging(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();

    assert_eq!(&*detail, &serde_json::Value::Null);
    assert_eq!(paging["page"]["total"], 2);
    assert_eq!(paging["selected"], context.selected().observation.json());
}

#[test]
fn missing_middle_app_ranks_preserve_complete_sibling_observations() {
    // Goal: one explicitly missing App facet cannot erase the other seconds' complete ranks.
    // Method: retain a refusal only at the middle of three real native observations.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.publish_with_system_rows(102);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = Held::new();
    held.enable_payload(Arc::clone(&budget));
    let mut ring = crate::store::ring::Ring::new(fixture.root.clone());
    ring.follow().unwrap();
    ring.read(|run, second, apps| {
        if second.publication == 2 {
            held.take(
                run,
                second,
                crate::store::apps::TickApps::Missing(&crate::store::Missing::Refused(
                    String::from("fixture App refusal"),
                )),
            )
        } else {
            held.take(run, second, apps)
        }
    })
    .unwrap();
    let context = context(&fixture, held, &budget);

    let ranks = context.app_ranks().unwrap();

    assert!(ranks.get(&key(&context.seconds()[0].observation)).is_some());
    assert!(ranks.get(&key(&context.seconds()[1].observation)).is_none());
    assert!(ranks.get(&key(&context.seconds()[2].observation)).is_some());
    assert!(matches!(
        context.selected_ranks_for(&context.seconds()[1].observation),
        Err(persistence::Error::Bound("selected ranks"))
    ));
}

fn assert_window_ranks(
    context: &QueryContext,
    shown: &crate::WindowRanks,
    expected: &crate::WindowRanks,
) {
    for second in context.seconds() {
        let identity = key(&second.observation);
        assert_eq!(
            shown.get(&identity),
            expected.get(&identity),
            "{identity:?}"
        );
    }
}

pub(super) fn saved_context(fixture: &Fixture) -> Arc<QueryContext> {
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = Held::new();
    held.enable_payload(Arc::clone(&budget));
    let outcome = crate::store::recording::load_payload(
        &fixture.root,
        &budget,
        &operation,
        |run, second, apps| assert!(held.take(run, second, apps)),
    )
    .unwrap();
    assert!(outcome.left_out.is_none());
    held.checked_catalog = outcome.checked_catalog;

    context(fixture, held, &budget)
}

pub(super) fn native_context(fixture: &Fixture) -> Arc<QueryContext> {
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = Held::new();
    held.enable_payload(Arc::clone(&budget));
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    crate::store::native_acceptance::read_native(&mut held, &mut ring, &operation).unwrap();

    context(fixture, held, &budget)
}

pub(super) fn context(
    fixture: &Fixture,
    mut held: Held,
    budget: &Arc<ByteBudget>,
) -> Arc<QueryContext> {
    held.ring_complete = true;
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(QueryRequest {
            epoch: 0,
            source: None,
            selection: QuerySelection::FollowLatest,
        })
        .unwrap();
    let coverage = CheckedCoverage::new(
        SourceGeneration {
            recording: fixture.writer.identity().unwrap().recording,
            generation: 0,
        },
        crate::session::ContextRange {
            first_ns: 0,
            end_ns: 10_000_000_000_000,
        },
    );
    queries.serve(&held, Some(&coverage), budget);

    match handle.take().unwrap().outcome {
        QueryOutcome::Ready(context) => context,
        outcome => panic!("expected retained context, found {outcome:?}"),
    }
}

fn key(observation: &crate::live_app_response::Observation) -> ObservationKey {
    ObservationKey {
        run: observation.stamp.run,
        boot: observation.stamp.boot,
        publication: observation.stamp.publication,
    }
}
