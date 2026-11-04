//! Saved-window presence and finite order floor have different canonical meanings.
use super::*;

mod finite_events_preserves_nonzero_order_floor_without_inventing_saved_window {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: retain a nonzero floor independently of publication progress and saved window.
        // Method: actual window1 LossOnly, finite preservation, then an ordinary window3 seal.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: retain a nonzero floor independently of publication progress and saved window.
        // Method: actual window1 LossOnly, finite preservation, then an ordinary window3 seal.
        run(true);
    }

    fn run(captured: bool) {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let prior = finite_floor_loss(&mut fixture, &store, &budget);
        assert_eq!(prior.order_floor(), 1);
        assert_eq!(prior.prior_window(), None);
        assert_eq!(prior.publication_end(), 3);
        for tick in 121..=122 {
            if captured {
                publish_captured_pair_at(&mut fixture, tick, tick * 1_000_000_000);
            } else {
                publish_numeric_pair_at(&mut fixture, tick, tick * 1_000_000_000);
            }
        }
        let expected = finite_expected_rows(&fixture, 1..5);
        publish_at(&mut fixture.writer, 3);
        let mut owner = store
            .begin_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        let state = finite_state(&fixture);
        assert!(matches!(
            state.input.operation,
            AttemptOperation::Finite { order_floor: 1, .. }
        ));
        finish_finite(&mut owner);
        drop(owner);
        finite_domain_oracle(&fixture, state.transaction, 1, 5, &expected, &budget);
        let frontier = finite_frontier(&store, &fixture, &budget);
        assert_eq!(frontier.publication_end(), 3);
        assert_eq!(frontier.event_end(), 5);
        assert_eq!(frontier.prior_window(), None);
        assert_eq!(frontier.order_floor(), 1);
        finite_floor_ordinary(&mut fixture, &store, &budget, &frontier);
        let next = finite_frontier(&store, &fixture, &budget);
        assert_eq!(next.publication_end(), 7);
        assert_eq!(next.event_end(), 5);
        assert_eq!(next.prior_window(), Some(3));
    }
}
fn finite_floor_loss(
    fixture: &mut Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
) -> crate::canonical::SourceFrontier {
    publish_at(&mut fixture.writer, 1);
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.operation = AttemptOperation::Ordinary {
        window: 1,
        attempted_origin: 2,
    };
    input.losses.publication_inherited_end = 2;
    input.losses.publications = 1;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), budget)
        .unwrap();
    crate::event_table::index::Index {
        identity: WindowIdentity {
            run: fixture.run,
            window: 1,
        },
        first: 1,
        end: 1,
        rows: 0,
        identities: input.captured,
        batches: Vec::new(),
    }
    .write_file(&mut attempt.create_event_index().unwrap())
    .unwrap();
    publish_at(&mut fixture.writer, 2);
    let mut owner = attempt.prove_loss(fixture.reader(), budget).unwrap();
    let mut complete = false;
    for _ in 0..20000 {
        if owner.step().unwrap() == PublicationStep::Idle {
            complete = true;
            break;
        }
    }
    assert!(complete);
    drop(owner);
    finite_frontier(store, fixture, budget)
}
fn finite_floor_ordinary(
    fixture: &mut Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
    frontier: &crate::canonical::SourceFrontier,
) {
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.operation = AttemptOperation::Ordinary {
        window: 3,
        attempted_origin: 6,
    };
    input.losses.predecessor = frontier.predecessor().clone();
    input.losses.publication_start = 3;
    input.losses.publication_inherited_end = 6;
    input.losses.publications = 3;
    input.losses.event_start = 5;
    input.losses.event_inherited_end = 5;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), budget)
        .unwrap();
    let manifest = encode_payload(&attempt, fixture, 6, false);
    assert_eq!(manifest.events_first, 5);
    assert_eq!(manifest.losses.events, 0);
    publish_at(&mut fixture.writer, 4);
    finish_publication(&mut issue(attempt, &manifest, fixture.reader(), budget).unwrap());
}

#[path = "finite_census_wire_tests.rs"]
mod finite_census_wire_tests;
