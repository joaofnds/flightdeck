use std::sync::Arc;

use super::{
    CheckedCoverage, Queries, QueryNeedsLoad, QueryOutcome, QueryRequest, QuerySelection,
    QueryUnavailable, SourceGeneration,
};
use crate::live::identity_fixture::Fixture;
use crate::store::{ByteBudget, Held};

#[test]
fn followed_selection_uses_publication_order_after_the_clock_moves_back() {
    // Goal: following selects the latest accepted publication, even after a wall-clock rollback.
    // Method: query a real sealed fixture containing two publications in reverse wall order.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(500);
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();

    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);
    let context = ready(handle.take().unwrap().outcome);

    assert_eq!(context.selected().observation.stamp.publication, 2);
    assert_eq!(context.selected_second(), 100);
    assert_eq!(context.context_last_second(), 100);
}

#[test]
fn same_second_alternatives_keep_exact_identity_and_the_selected_endpoint() {
    // Goal: same-second siblings remain distinct and inspection cannot move their context.
    // Method: select the second sibling, advance live data, and inspect both saved identities.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(120);
    fixture.seal();
    let (mut held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::Second {
            second: 100,
            alternative: 1,
        }))
        .unwrap();
    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);
    let context = ready(handle.take().unwrap().outcome);
    let alternatives: Vec<_> = context.alternatives(100).collect();
    take(&mut held, &fixture, 4, 1_000);

    let first = context.inspect(&alternatives[0]).unwrap();
    let selected = context.inspect(&alternatives[1]).unwrap();
    let mut wrong_clock = alternatives[1];
    wrong_clock.stamp.awake_ns = Some(999);

    assert_eq!(alternatives.len(), 2);
    assert_eq!(first.observation.stamp.publication, 1);
    assert_eq!(selected.observation.stamp.publication, 2);
    assert_eq!(context.selected().observation, alternatives[1]);
    assert_eq!(context.context_last_second(), 120);
    assert!(matches!(
        context.inspect(&wrong_clock),
        Err(QueryNeedsLoad::MissingObservation)
    ));
}

#[test]
fn an_exact_pin_keeps_the_displayed_identity_and_endpoint_after_alternatives_change() {
    // Goal: live advancement cannot change the observation or endpoint being pinned.
    // Method: insert an earlier-ordered same-second sibling and a later live second before serving.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (mut held, budget) = load(&fixture);
    let observation = held.apps.newest().unwrap().observation.unwrap();
    let run = collector::ring::RunIdentity {
        run: collector::ring::Identity([0, 0]),
        ..fixture.writer.identity().unwrap()
    };
    let sibling = crate::store::MachineSecond {
        publication: 1,
        wall_ns: 100_000_000_000,
        ..crate::store::MachineSecond::default()
    };
    assert!(held.take(
        &run,
        &sibling,
        crate::store::apps::TickApps::Missing(&crate::store::Missing::Unread)
    ));
    let later = crate::store::MachineSecond {
        publication: 2,
        wall_ns: 120_000_000_000,
        ..crate::store::MachineSecond::default()
    };
    assert!(held.take(
        &run,
        &later,
        crate::store::apps::TickApps::Missing(&crate::store::Missing::Unread)
    ));
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::Exact {
            observation,
            context_last_second: 100,
        }))
        .unwrap();

    queries.serve(&held, Some(&coverage(&fixture, 1)), &budget);
    let context = ready(handle.take().unwrap().outcome);

    assert_eq!(context.alternatives(100).next().unwrap().stamp.run, [0, 0]);
    assert_eq!(context.selected().observation, observation);
    assert_eq!(context.context_last_second(), 100);
    assert!(context.selected().detached());
}

#[test]
fn a_verified_gap_is_unavailable_without_requesting_a_load() {
    // Goal: checked absence remains different from data outside retained checked coverage.
    // Method: query a missing second between two real loaded observations.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(102);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::Second {
            second: 101,
            alternative: 0,
        }))
        .unwrap();

    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);

    assert!(matches!(
        handle.take().unwrap().outcome,
        QueryOutcome::Unavailable(QueryUnavailable::NoObservation)
    ));
}

#[test]
fn a_kept_context_allows_twenty_minutes_of_subsequent_publications() {
    // Goal: a pinned context cannot consume the Store's 64 spare pool slots.
    // Method: pin a full span, accept 1,200 more seconds, and inspect the original identity.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1);
    fixture.seal();
    let (mut held, budget) = load(&fixture);
    let full = u64::try_from(crate::store::SECONDS_MAX).unwrap();
    for publication in 2..=full {
        take(&mut held, &fixture, publication, publication);
    }
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::Second {
            second: full,
            alternative: 0,
        }))
        .unwrap();
    let mut checked = coverage(&fixture, 0);
    checked.range.as_mut().unwrap().end_ns = 10_000_000_000_000;
    queries.serve(&held, Some(&checked), &budget);
    let context = ready(handle.take().unwrap().outcome);
    let selected = context.selected().observation;

    for offset in 1..=1_200 {
        take(&mut held, &fixture, full + offset, full + offset);
    }

    assert_eq!(held.newest.as_ref().unwrap().publication, full + 1_200);
    assert_eq!(context.inspect(&selected).unwrap().observation, selected);
    assert_eq!(context.context_last_second(), full);
    drop(context);
    assert_eq!(handle.contexts_held(), 0);
}

#[test]
fn admission_refusal_preserves_both_existing_contexts() {
    // Goal: admission cannot discard an old displayed context when its replacement does not fit.
    // Method: keep the displayed and replacement contexts, then request a third one.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    let checked = coverage(&fixture, 0);
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();
    queries.serve(&held, Some(&checked), &budget);
    let displayed = ready(handle.take().unwrap().outcome);
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();
    queries.serve(&held, Some(&checked), &budget);
    let replacement = ready(handle.take().unwrap().outcome);

    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();
    queries.serve(&held, Some(&checked), &budget);
    let refused = handle.take().unwrap();

    assert!(matches!(
        refused.outcome,
        QueryOutcome::Unavailable(QueryUnavailable::ContextLimit)
    ));
    assert_eq!(displayed.selected().observation.stamp.publication, 1);
    assert_eq!(
        replacement.selected().observation,
        displayed.selected().observation
    );
    assert_eq!(handle.contexts_held(), 2);
    drop(replacement);
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();
    queries.serve(&held, Some(&checked), &budget);
    assert!(matches!(
        handle.take().unwrap().outcome,
        QueryOutcome::Ready(_)
    ));
}

#[test]
fn queries_perform_no_source_reads_frames_or_app_derivations() {
    // Goal: a held query uses canonical data without deriving or reading the source again.
    // Method: trace actual query construction after the real sealed loader has finished.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let trace =
        persistence::work::ingestion::Trace::new(fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    let retained_bytes = budget.used();
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();

    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);
    let context = ready(handle.take().unwrap().outcome);
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    drop(scope);

    assert!(events.is_empty());
    assert!(!context.selected().detached());
    assert_eq!(budget.used(), retained_bytes + context.bytes());
    assert!(std::ptr::eq(
        held.span.newest().unwrap(),
        context.selected().machine()
    ));
    assert!(std::ptr::eq(
        held.apps.newest().unwrap(),
        context.selected().apps()
    ));
    assert!(Arc::ptr_eq(
        held.apps.newest().unwrap().processes.as_ref().unwrap(),
        context.selected().apps().processes.as_ref().unwrap(),
    ));
    assert!(std::ptr::eq(
        held.span.newest().unwrap().complete().unwrap(),
        context.selected().machine().complete().unwrap()
    ));
}

#[test]
fn fixed_context_payloads_keep_their_budget_until_the_last_reader_releases_them() {
    // Goal: a selected context's complete allocations retain their original budget owners.
    // Method: detach a real loaded observation, drop the Store, then release the context.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::Second {
            second: 100,
            alternative: 0,
        }))
        .unwrap();
    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);
    let context = ready(handle.take().unwrap().outcome);
    assert!(context.selected().detached());
    assert!(Arc::ptr_eq(
        held.apps.newest().unwrap().processes.as_ref().unwrap(),
        context.selected().apps().processes.as_ref().unwrap()
    ));

    drop(held);

    assert!(budget.used() > context.bytes());
    assert_eq!(
        context.selected().apps().observation.unwrap(),
        context.selected().observation
    );
    drop(context);
    assert_eq!(budget.used(), 0);
}

#[test]
fn budget_refusal_keeps_the_displayed_context_usable() {
    // Goal: denied replacement memory never invalidates a successfully displayed context.
    // Method: hold a context, fill its shared budget, and request a second context.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();
    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);
    let displayed = ready(handle.take().unwrap().outcome);
    let occupied = budget.reserve(budget.maximum() - budget.used()).unwrap();

    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();
    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);

    assert!(matches!(
        handle.take().unwrap().outcome,
        QueryOutcome::Unavailable(QueryUnavailable::Budget)
    ));
    assert_eq!(displayed.selected().observation.stamp.publication, 1);
    assert_eq!(handle.contexts_held(), 1);
    drop(occupied);
}

#[test]
fn a_request_for_an_old_source_generation_cannot_use_a_new_certificate() {
    // Goal: a selection cannot cross a checked source/catalog generation by timestamp alone.
    // Method: submit generation 0 against a valid generation 1 certificate of the same recording.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(QueryRequest {
            source: Some(coverage(&fixture, 0).source),
            ..request(QuerySelection::FollowLatest)
        })
        .unwrap();

    queries.serve(&held, Some(&coverage(&fixture, 1)), &budget);

    assert!(matches!(
        handle.take().unwrap().outcome,
        QueryOutcome::NeedsLoad(QueryNeedsLoad::SourceChanged)
    ));
}

#[test]
fn requests_outside_checked_coverage_return_needs_load() {
    // Goal: timestamp presence never certifies source coverage or silently yields empty data.
    // Method: ask without a certificate and with a certificate excluding the requested lead-in.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1_000);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();

    queries.serve(&held, None, &budget);
    assert!(matches!(
        handle.take().unwrap().outcome,
        QueryOutcome::NeedsLoad(QueryNeedsLoad::UncheckedCoverage)
    ));
    let checked = CheckedCoverage::new(
        coverage(&fixture, 0).source,
        crate::session::ContextRange {
            first_ns: 999_000_000_000,
            end_ns: 1_001_000_000_000,
        },
    );
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();
    queries.serve(&held, Some(&checked), &budget);

    assert!(matches!(
        handle.take().unwrap().outcome,
        QueryOutcome::NeedsLoad(QueryNeedsLoad::OutsideCoverage)
    ));
}

#[test]
fn latest_requests_replace_untaken_work_and_cancellation_keeps_displayed_data() {
    // Goal: bursty navigation has one pending request and cancellation preserves prior results.
    // Method: overwrite 1,200 requests, serve only the last token, then cancel a replacement.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    let mut latest = None;
    for epoch in 0..1_200 {
        latest = Some(
            handle
                .submit(QueryRequest {
                    epoch,
                    ..request(QuerySelection::FollowLatest)
                })
                .unwrap(),
        );
    }

    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);
    let reply = handle.take().unwrap();
    assert_eq!(Some(reply.token), latest);
    let displayed = ready(reply.outcome);
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();
    handle.cancel().unwrap();
    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);

    assert!(handle.take().is_none());
    assert!(!handle.pending());
    assert_eq!(displayed.selected().observation.stamp.publication, 1);
}

#[test]
fn cancellation_during_detachment_stops_at_the_next_observation_and_releases_admission() {
    // Goal: obsolete detachment does not keep copying the remaining context's App rows.
    // Method: supersede a real three-observation build after its first detached observation.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.publish_with_system_rows(102);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let retained_bytes = budget.used();
    let contexts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let checks = std::cell::Cell::new(0_u32);

    let outcome = super::build(
        &held,
        Some(&coverage(&fixture, 0)),
        &request(QuerySelection::Second {
            second: 100,
            alternative: 0,
        }),
        &budget,
        &contexts,
        || {
            checks.set(checks.get() + 1);
            checks.get() < 3
        },
    );

    assert!(outcome.is_none());
    assert_eq!(checks.get(), 3);
    assert_eq!(budget.used(), retained_bytes);
    assert_eq!(contexts.load(std::sync::atomic::Ordering::Acquire), 0);
}

#[test]
fn a_query_leaves_the_display_view_mailbox_and_version_untouched() {
    // Goal: a Worker query cannot steal a pending display View or claim its version as coverage.
    // Method: publish a real held View, query through the other mailbox, then take the View.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (mut held, budget) = load(&fixture);
    let mailbox = crate::store::Mailbox::new();
    held.publish(&mailbox);
    let version = held.version;
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();

    queries.serve(&held, None, &budget);

    assert!(matches!(
        handle.take().unwrap().outcome,
        QueryOutcome::NeedsLoad(QueryNeedsLoad::UncheckedCoverage)
    ));
    assert!(mailbox.pending());
    assert_eq!(mailbox.take().unwrap().version, version);
    assert_eq!(held.version, version);
}

#[test]
fn a_closed_store_refuses_new_queries_without_invalidating_a_displayed_context() {
    // Goal: a query handle cannot keep accepting work after its Store producer has closed.
    // Method: keep a result, drop the producer, and submit another request through its handle.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (held, budget) = load(&fixture);
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();
    queries.serve(&held, Some(&coverage(&fixture, 0)), &budget);
    let displayed = ready(handle.take().unwrap().outcome);

    drop(queries);

    assert_eq!(
        handle.submit(request(QuerySelection::FollowLatest)),
        Err(super::QuerySubmitError::Closed)
    );
    assert!(!handle.pending());
    assert_eq!(displayed.selected().observation.stamp.publication, 1);
}

#[test]
fn a_producer_exit_answers_an_accepted_request_with_closed() {
    // Goal: startup or run failure cannot leave an already accepted request pending forever.
    // Method: close the producer while its request is waiting, keeping its object alive.
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    let token = handle
        .submit(request(QuerySelection::FollowLatest))
        .unwrap();

    queries.close();
    let reply = handle.take().unwrap();

    assert_eq!(reply.token, token);
    assert!(matches!(
        reply.outcome,
        QueryOutcome::Unavailable(QueryUnavailable::Closed)
    ));
    assert!(!handle.pending());
    assert_eq!(
        handle.submit(request(QuerySelection::FollowLatest)),
        Err(super::QuerySubmitError::Closed)
    );
}

fn take(held: &mut Held, fixture: &Fixture, publication: u64, second: u64) {
    let machine = crate::store::MachineSecond {
        publication,
        run_tick: publication - 1,
        wall_ns: second * 1_000_000_000,
        ..crate::store::MachineSecond::default()
    };
    assert!(held.take(
        &fixture.writer.identity().unwrap(),
        &machine,
        crate::store::apps::TickApps::Missing(&crate::store::Missing::Unread)
    ));
}

fn load(fixture: &Fixture) -> (Held, Arc<ByteBudget>) {
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = Held::new();
    held.enable_payload(Arc::clone(&budget));
    crate::store::recording::load_payload(
        &fixture.root,
        &budget,
        &operation,
        |run, second, apps| assert!(held.take(run, second, apps)),
    )
    .unwrap();
    held.ring_complete = true;

    (held, budget)
}

fn coverage(fixture: &Fixture, generation: u64) -> CheckedCoverage {
    CheckedCoverage::new(
        SourceGeneration {
            recording: fixture.writer.identity().unwrap().recording,
            generation,
        },
        crate::session::ContextRange {
            first_ns: 0,
            end_ns: 10_000_000_000_000,
        },
    )
}

fn request(selection: QuerySelection) -> QueryRequest {
    QueryRequest {
        epoch: 7,
        source: None,
        selection,
    }
}

fn ready(outcome: QueryOutcome) -> Arc<super::QueryContext> {
    match outcome {
        QueryOutcome::Ready(context) => context,
        outcome => panic!("expected retained context, found {outcome:?}"),
    }
}
