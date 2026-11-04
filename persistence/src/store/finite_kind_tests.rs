//! Occupied operation authority is independent of overlapping standalone kind grammar.
use super::*;
use std::os::unix::fs::FileExt;

mod finite_events_occupied_kind_substitution_refuses_before_gate_or_cleanup {
    use super::*;

    #[test]
    fn numeric_rows_same_owner() {
        // Goal: valid kind3/4 bytes cannot replace metadata expected by an occupied operation3.
        // Method: same-inode mutation, same owner/Store reopen, both profiles and output kinds.
        run(false, false, false);
    }

    #[test]
    fn numeric_rows_reopen() {
        // Goal: valid kind3/4 bytes cannot replace metadata expected by an occupied operation3.
        // Method: same-inode mutation, same owner/Store reopen, both profiles and output kinds.
        run(false, false, true);
    }

    #[test]
    fn numeric_loss_same_owner() {
        // Goal: valid kind3/4 bytes cannot replace metadata expected by an occupied operation3.
        // Method: same-inode mutation, same owner/Store reopen, both profiles and output kinds.
        run(false, true, false);
    }

    #[test]
    fn numeric_loss_reopen() {
        // Goal: valid kind3/4 bytes cannot replace metadata expected by an occupied operation3.
        // Method: same-inode mutation, same owner/Store reopen, both profiles and output kinds.
        run(false, true, true);
    }

    #[test]
    fn captured_rows_same_owner() {
        // Goal: valid kind3/4 bytes cannot replace metadata expected by an occupied operation3.
        // Method: same-inode mutation, same owner/Store reopen, both profiles and output kinds.
        run(true, false, false);
    }

    #[test]
    fn captured_rows_reopen() {
        // Goal: valid kind3/4 bytes cannot replace metadata expected by an occupied operation3.
        // Method: same-inode mutation, same owner/Store reopen, both profiles and output kinds.
        run(true, false, true);
    }

    #[test]
    fn captured_loss_same_owner() {
        // Goal: valid kind3/4 bytes cannot replace metadata expected by an occupied operation3.
        // Method: same-inode mutation, same owner/Store reopen, both profiles and output kinds.
        run(true, true, false);
    }

    #[test]
    fn captured_loss_reopen() {
        // Goal: valid kind3/4 bytes cannot replace metadata expected by an occupied operation3.
        // Method: same-inode mutation, same owner/Store reopen, both profiles and output kinds.
        run(true, true, true);
    }

    fn run(captured: bool, loss: bool, reopen: bool) {
        finite_kind_case(captured, loss, reopen);
    }
}
fn finite_kind_case(captured: bool, loss: bool, reopen: bool) {
    let (mut fixture, _) = inherited_fixture(captured);
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut store = Some(Store::open(&fixture.root).unwrap());
    let mut owner = store
        .as_ref()
        .unwrap()
        .begin_finite_events(fixture.reader(), &budget, &ledger)
        .unwrap();
    let transaction = finite_state(&fixture).transaction;
    if loss {
        finite_kind_overwrite(&mut fixture, captured);
    }
    let object = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(transaction).to_string());
    let child = if loss { "loss" } else { "fragment-0" };
    let path = object.join(child).join("metadata");
    finite_kind_wait_metadata(&mut owner, &path);
    let expected = if loss {
        Vec::new()
    } else {
        finite_expected_rows(&fixture, 3..5)
    };
    finite_domain_oracle(&fixture, transaction, 1, 5, &expected, &budget);
    let mut bytes = fs::read(&path).unwrap();
    finite_kind_grammar_control(&fixture, &bytes, if loss { 4 } else { 3 });
    bytes[15] = if loss { 4 } else { 3 };
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all_at(&bytes, 0).unwrap();
    private::full_sync(&file).unwrap();
    drop(file);
    let originals = finite_kind_originals(&object);
    if reopen {
        drop(owner);
        drop(store.take());
        store = Some(Store::open(&fixture.root).unwrap());
        match store
            .as_ref()
            .unwrap()
            .resume_finite_events(Some(fixture.reader()), &budget, &ledger)
        {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::InvalidData),
            Ok(Some(mut resumed)) => finite_kind_refuses(&mut resumed),
            Ok(None) => panic!("occupied finite substitution became Idle"),
        }
    } else {
        finite_kind_refuses(&mut owner);
        drop(owner);
    }
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(finite_kind_originals(&object), originals);
    let state = finite_state(&fixture);
    assert_eq!(state.transaction, transaction);
    assert_eq!(state.row_plan.map_or(0, |plan| plan.committed), 0);
    assert!(object.join("pending").exists());
    assert_eq!(
        fs::read_dir(fixture.root.join("event-fragments"))
            .unwrap()
            .count(),
        0
    );
}
fn finite_kind_wait_metadata(owner: &mut FiniteEvents<'_>, path: &Path) {
    for _ in 0..20000 {
        if path.exists() {
            return;
        }
        assert_eq!(owner.step().unwrap(), RowPublicationStep::Continue);
    }
    panic!("finite metadata boundary");
}
fn finite_kind_refuses(owner: &mut FiniteEvents<'_>) {
    for _ in 0..20000 {
        match owner.step() {
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                return;
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(result) => panic!("substituted metadata admitted: {result:?}"),
        }
    }
    panic!("kind substitution did not refuse");
}
fn finite_kind_originals(object: &Path) -> Vec<(String, Vec<u8>)> {
    let mut result = Vec::new();
    for name in [
        "original-census",
        "pending/events/index",
        "pending/events/0000.parquet",
        "fragment-0/0000.parquet",
    ] {
        match fs::read(object.join(name)) {
            Ok(bytes) => result.push((name.to_owned(), bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("original {name}: {error}"),
        }
    }
    result
}

mod finite_events_cleared_canonical_preserves_encoded_kind_and_locator_rules {
    use super::*;

    #[test]
    fn numeric_rows() {
        // Goal: preserve declared grammar and refuse terminal loss at a finite event gate.
        // Method: clear actual finite Store output, mutate only kind, then check locator and full
        // rows.
        run(false, false);
    }

    #[test]
    fn numeric_loss() {
        // Goal: preserve declared grammar and refuse terminal loss at a finite event gate.
        // Method: clear actual finite Store output, mutate only kind, then check locator and full
        // rows.
        run(false, true);
    }

    #[test]
    fn captured_rows() {
        // Goal: preserve declared grammar and refuse terminal loss at a finite event gate.
        // Method: clear actual finite Store output, mutate only kind, then check locator and full
        // rows.
        run(true, false);
    }

    #[test]
    fn captured_loss() {
        // Goal: preserve declared grammar and refuse terminal loss at a finite event gate.
        // Method: clear actual finite Store output, mutate only kind, then check locator and full
        // rows.
        run(true, true);
    }

    fn run(captured: bool, loss: bool) {
        finite_kind_cleared(captured, loss);
    }
}
fn finite_kind_cleared(captured: bool, loss: bool) {
    let (mut fixture, _) = inherited_fixture(captured);
    let expected = finite_expected_rows(&fixture, 3..5);
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let store = Store::open(&fixture.root).unwrap();
    let mut owner = store
        .begin_finite_events(fixture.reader(), &budget, &ledger)
        .unwrap();
    let transaction = finite_state(&fixture).transaction;
    if loss {
        finite_kind_overwrite(&mut fixture, captured);
    }
    finish_finite(&mut owner);
    drop(owner);
    finite_domain_oracle(
        &fixture,
        transaction,
        1,
        5,
        if loss { &[] } else { &expected },
        &budget,
    );
    let child = if loss { "loss" } else { "fragment-0" };
    let path = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(transaction).to_string())
        .join(child)
        .join("metadata");
    let original = fs::read(&path).unwrap();
    finite_kind_grammar_control(&fixture, &original, if loss { 4 } else { 3 });
    let mut bytes = original.clone();
    bytes[15] = if loss { 4 } else { 3 };
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all_at(&bytes, 0).unwrap();
    private::full_sync(&file).unwrap();
    finite_kind_declared(
        &store,
        &fixture,
        transaction,
        child,
        &expected,
        &budget,
        loss,
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    file.write_all_at(&original, 0).unwrap();
    private::full_sync(&file).unwrap();
    assert_eq!(finite_frontier(&store, &fixture, &budget).event_end(), 5);
}

fn finite_kind_declared(
    store: &Store,
    fixture: &Fixture,
    transaction: Identity,
    child: &str,
    expected: &[crate::event_recovery::batch::Row],
    budget: &Arc<crate::resources::ByteBudget>,
    loss: bool,
) {
    if loss {
        assert_eq!(
            store
                .source_frontier(
                    &fixture.run,
                    budget,
                    &mut crate::canonical::ReadWork::default()
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    } else {
        let frontier = finite_frontier(store, fixture, budget);
        assert!(matches!(
            frontier.predecessor(),
            Predecessor::Canonical {
                kind: crate::transaction::CanonicalKind::Fragment,
                ..
            }
        ));
        assert_eq!(frontier.event_end(), 5);
        assert!(!frontier.complete());
        let metadata = finite_metadata(fixture, transaction, child);
        finite_domain_rows(fixture, transaction, child, &metadata, expected, budget);
    }
}

fn finite_kind_overwrite(fixture: &mut Fixture, captured: bool) {
    for number in 5..=6 {
        if captured {
            publish_captured_pair_at(fixture, number, number);
        } else {
            publish_numeric_pair_at(fixture, number, number);
        }
    }
}
