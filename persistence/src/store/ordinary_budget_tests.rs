//! Public recovery under pressure from another owner of the shared allowance.
use super::*;

#[test]
fn ordinary_process_chunks_replace_workspace_under_pressure_numeric() {
    // Goal: exhausted expected rows release their workspace before the next chunk needs it.
    // Method: recover 128 processes with room for one chunk at the replacement boundary.
    process_chunks_replace_workspace_under_pressure(false);
}

#[test]
fn ordinary_process_chunks_replace_workspace_under_pressure_captured() {
    // Goal: captured process recovery advances under the same one-chunk allowance.
    // Method: constrain chunk replacement through the shared budget and verify exact raw bytes.
    process_chunks_replace_workspace_under_pressure(true);
}

fn process_chunks_replace_workspace_under_pressure(captured: bool) {
    let mut fixture = metric_fixture_with_processes(captured, 1, 128);
    metric_publish(&mut fixture, 0, true, true);
    fixture.writer.finish();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(&complete_input(&fixture), &mut fixture.reader(), &budget)
        .unwrap();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    complete_payload(&attempt, &fixture, 1);
    drop(attempt);
    let raw = raw_bytes(&object.join("pending"));
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();

    process_chunks_replace_workspace_under_pressure_recover(&mut owner, &budget);
    drop(owner);

    assert_eq!(raw_bytes(&object.join("ready")), raw);
    let directory = PrivateDirectory::open(&object.join("ready")).unwrap();
    let processes = crate::engine::AdmittedTable::from_file_budgeted(
        directory.open_file("processes.parquet").unwrap(),
        &budget,
    )
    .unwrap();
    assert_eq!(processes.row_count(), 128);
    let frontier = store
        .source_frontier(
            &fixture.run,
            &budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 3);
    assert_eq!(frontier.event_end(), 3);
}

fn process_chunks_replace_workspace_under_pressure_recover(
    owner: &mut OrdinaryRecovery<'_>,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let expected_workspace =
        64 * (2 * collector::ring::PROGRAM_PATH_BYTES_MAX + 1024) + 8 * 1024 * 1024;
    for _ in 0..20_000 {
        let before = budget.used();
        let step = owner.step().unwrap();
        let after = budget.used();
        assert_eq!(step, PublicationStep::Continue);
        if after.checked_sub(before) == Some(expected_workspace) {
            // Decode the current chunk before constraining its replacement continuation.
            assert_eq!(owner.step().unwrap(), PublicationStep::Continue);
            let pressure = budget
                .reserve(budget.maximum() - budget.used() - 4 * 1024 * 1024)
                .unwrap();
            assert_eq!(owner.step().unwrap(), PublicationStep::Continue);
            drop(pressure);
            finish_recovery(owner);
            return;
        }
    }

    panic!("ordinary pressure recovery exceeded its fixture work bound");
}
