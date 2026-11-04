//! Native faults after the consuming handoff must join the caller's cumulative ledger.
use super::*;

#[test]
fn ordinary_handoff_events_numeric_counts_failed_retries() {
    // Goal: numeric directory readmission charges every failed and repeated open.
    // Method: fault the exact events directory after reconstruction hands off, then recover.
    native_case_run(false, "work-events", "retry");
}

#[test]
fn ordinary_handoff_events_captured_counts_failed_retries() {
    // Goal: captured directory readmission retains cumulative attempted work.
    // Method: fault three events opens after the consuming handoff and compare native counts.
    native_case_run(true, "work-events", "retry");
}

#[test]
fn ordinary_handoff_ticks_numeric_counts_failed_retries() {
    // Goal: numeric canonical tick readmission charges actual failed descriptor reads.
    // Method: fail three tick reads after table validation and then finish the same owner.
    native_case_run(false, "work-ticks", "retry");
}

#[test]
fn ordinary_handoff_ticks_captured_counts_failed_retries() {
    // Goal: captured canonical tick readmission does not discard failed raw work.
    // Method: compare three native read refusals with ledger deltas after normal handoff.
    native_case_run(true, "work-ticks", "retry");
}

#[test]
fn ordinary_handoff_ready_numeric_counts_failed_retries() {
    // Goal: Ready placement charges failed namespace acquisition after durable proof.
    // Method: refuse the data directory three times after metadata, then finish publication.
    native_case_run(false, "work-ready", "retry");
}

#[test]
fn ordinary_handoff_ready_captured_counts_failed_retries() {
    // Goal: captured Ready placement retains cumulative failure accounting.
    // Method: compare three native data-directory refusals with attempted-open increments.
    native_case_run(true, "work-ready", "retry");
}

pub(super) fn child(
    store: &Store,
    source: &str,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
    armed: &Path,
    boundary: &str,
) {
    let mut owner = store
        .resume_ordinary_proof(RingReader::open_generation(source).unwrap(), budget, ledger)
        .unwrap();
    child_reach_handoff(&mut owner);
    if boundary == "work-ticks" {
        // The fixture contains the nine required metric tables and its event index.
        for _ in 0..crate::schema::TABLES.len() {
            assert_eq!(owner.step().unwrap(), PublicationStep::Continue);
        }
    } else if boundary == "work-ready" {
        child_reach_metadata(&mut owner, &store.root);
    }
    fs::write(armed, []).unwrap();
    let occupied = fs::read(store.root.join("foreground")).unwrap();
    let before = ledger.work();
    let observed = PathBuf::from(std::env::var_os("FLIGHTDECK_PRIVATE_SYNC_OBSERVED").unwrap());
    let prior = fs::metadata(&observed).unwrap().len();
    for _ in 0..3 {
        assert_eq!(owner.step().unwrap_err().raw_os_error(), Some(5));
        assert_eq!(fs::read(store.root.join("foreground")).unwrap(), occupied);
    }
    let after = ledger.work();
    let attempts = fs::metadata(&observed).unwrap().len() - prior;
    assert_eq!(attempts, 3);
    let charged = if boundary == "work-ticks" {
        after.raw_read_operations - before.raw_read_operations
    } else {
        after.inventory.open_attempts - before.inventory.open_attempts
            + after.canonical.namespace_attempts
            - before.canonical.namespace_attempts
    };
    assert_eq!(
        charged, attempts,
        "uncharged post-handoff work at {boundary}"
    );
    fs::remove_file(armed).unwrap();
    finish_recovery(&mut owner);
    let completed = ledger.work();
    if boundary != "work-ready" {
        assert!(completed.decode_admission_bytes > after.decode_admission_bytes);
        assert!(completed.raw_read_operations > after.raw_read_operations);
    }
    drop(owner);
    let released = ledger.work();
    assert_eq!(released.raw_read_operations, completed.raw_read_operations);
    assert_eq!(released.proof_operations, completed.proof_operations);
    assert_eq!(
        released.canonical.namespace_attempts,
        completed.canonical.namespace_attempts
    );
    eprintln!("ORDINARY-HANDOFF boundary={boundary} attempts={attempts} charged={charged}");
}

fn child_reach_handoff(owner: &mut OrdinaryRecovery<'_>) {
    for _ in 0..20_000 {
        let before = owner.work().canonical.namespace_attempts;
        assert_eq!(owner.step().unwrap(), PublicationStep::Continue);
        let attempts = owner.work().canonical.namespace_attempts - before;
        if attempts != 0 {
            // Only normal handoff opens canonical placement namespaces during reconstruction.
            assert_eq!(attempts, 2);
            return;
        }
    }
    panic!("normal handoff was not reached within the fixture bound");
}

fn child_reach_metadata(owner: &mut OrdinaryRecovery<'_>, root: &Path) {
    let objects: Vec<_> = fs::read_dir(root.join("data"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(objects.len(), 1);
    let metadata = objects[0].join("pending/metadata");
    for _ in 0..20_000 {
        assert_eq!(owner.step().unwrap(), PublicationStep::Continue);
        if metadata.exists() {
            crate::canonical::CanonicalMetadata::read(&mut File::open(&metadata).unwrap()).unwrap();
            return;
        }
    }
    panic!("normal metadata was not reached within the fixture bound");
}
