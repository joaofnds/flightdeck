use super::*;
use collector::recording::bootstrap::codec::{NamespaceState, decode_state};

#[test]
fn finite_review_sync_counts_numeric() {
    // Goal: native sync attempts equal the shared ledger through construction and retry.
    // Method: trace actual barriers around an interrupted row-bearing metadata sync.
    finite_review_sync_counts(false, "metadata-sync");
}

#[test]
fn finite_review_sync_counts_captured() {
    // Goal: captured construction charges internal directory barriers and failed attempts.
    // Method: compare native sync calls with the cumulative ledger before and after retry.
    finite_review_sync_counts(true, "metadata-sync");
}

#[test]
fn finite_review_sync_counts_zero() {
    // Goal: zero-row refinement also charges its loss-directory barriers.
    // Method: fail after loss metadata sync and compare native attempts with the ledger.
    finite_review_sync_counts(false, "loss-meta");
}

fn finite_review_sync_counts(captured: bool, boundary: &str) {
    let fixture = Fixture::new(false);
    let library = native_registration_build(&fixture);
    finite_case(&library, captured, boundary, 0);
}

#[test]
fn finite_review_phase8_numeric_cold() {
    // Goal: completed zero-row proof survives an interrupted phase8-to-Busy transition.
    // Method: inject two native write effects and reopen without transport after child exit.
    finite_review_phase8(false, true);
}

#[test]
fn finite_review_phase8_captured_cold() {
    // Goal: captured zero-row proof resumes after Busy becomes visible in phase8.
    // Method: inject two native write effects and reopen without transport after child exit.
    finite_review_phase8(true, true);
}

#[test]
fn finite_review_phase8_numeric_same_owner() {
    // Goal: the same owner adopts its visible Busy effect without selecting new authority.
    // Method: disarm the native rename error and finish through that owner.
    finite_review_phase8(false, false);
}

#[test]
fn finite_review_phase8_captured_same_owner() {
    // Goal: captured same-owner retry keeps the completed finite loss unchanged.
    // Method: disarm the native rename error and finish through that owner.
    finite_review_phase8(true, false);
}

#[test]
fn finite_review_phase8_child() {
    // Goal: isolate the Busy rename fault from the parent test runner.
    // Method: dispatch only when the parent supplies its owned fixture namespace.
    finite_review_phase8_child_dispatch();
}

fn finite_review_phase8(captured: bool, cold: bool) {
    let (mut fixture, _) = inherited_fixture(captured);
    let library = native_registration_build(&fixture);
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let store = Store::open(&fixture.root).unwrap();
    drop(
        store
            .begin_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap(),
    );
    let transaction = finite_state(&fixture).transaction;
    drop(store);
    for number in 5..=6 {
        if captured {
            publish_captured_pair_at(&mut fixture, number, number);
        } else {
            publish_numeric_pair_at(&mut fixture, number, number);
        }
    }
    finite_case_child(&fixture, &library, "loss-prefix", 2);
    let store = Store::open(&fixture.root).unwrap();
    finite_case_partial_zero(&fixture, &store, &budget, &ledger);
    drop(store);
    let object = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(transaction).to_string());
    let witness = fs::read(object.join("original-census")).unwrap();

    finite_review_phase8_launch(&fixture, &library, cold);

    if cold {
        let state = finite_state(&fixture);
        assert_eq!(state.phase, Phase::UnresolvedExtent);
        assert_eq!(state.sequence, 0);
        assert_eq!(state.transaction, transaction);
        assert!(state.salvage.is_some());
        assert_eq!(fs::read(object.join("original-census")).unwrap(), witness);
        assert!(object.join("pending").exists());
        assert_eq!(
            finite_metadata(&fixture, transaction, "loss")
                .loss
                .events_new,
            4
        );
        finite_review_phase8_resume(&fixture, &budget, &ledger);
    }
    finite_domain_oracle(&fixture, transaction, 1, 5, &[], &budget);
    assert!(!object.join("original-census").exists());
    assert!(!object.join("pending").exists());
    assert!(matches!(
        bootstrap::codec::decode_slot(
            &fixture.slot(),
            fixture.run.recording,
            bootstrap::codec::SlotKind::Foreground,
        )
        .unwrap(),
        bootstrap::codec::Slot::Idle
    ));
    let store = Store::open(&fixture.root).unwrap();
    let frontier = finite_frontier(&store, &fixture, &budget);
    assert_eq!(frontier.event_end(), 5);
    assert_eq!(frontier.publication_end(), 1);
    assert!(!frontier.complete());
}

fn finite_review_phase8_resume(
    fixture: &Fixture,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
) {
    let store = Store::open(&fixture.root).unwrap();
    let mut owner = store
        .resume_finite_events(None, budget, ledger)
        .unwrap()
        .unwrap();
    finish_finite(&mut owner);
}

fn finite_review_phase8_launch(fixture: &Fixture, library: &Path, cold: bool) {
    let root = fixture.root.canonicalize().unwrap();
    let arm = fixture.base.join("phase8-arm");
    let observed = fixture.base.join("phase8-observed");
    fs::write(&observed, b"").unwrap();
    let log_path = fixture.base.join("phase8-child.log");
    let log = File::create(&log_path).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", concat!(
        "store::registration_tests::loss_proof_tests::inherited_events_tests::",
        "finite_events_native_tests::finite_review_native_tests::finite_review_phase8_child",
    ), "--nocapture"])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_PHASE8_ROOT", &root)
        .env("FLIGHTDECK_PHASE8_COLD", if cold { "1" } else { "0" })
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env("FLIGHTDECK_REGISTRATION_ARMED", &arm)
        .env("FLIGHTDECK_PRIVATE_SYNC_OBSERVED", &observed)
        .env("FLIGHTDECK_MUTATION_TARGET", root.join("namespace-state"))
        .env("FLIGHTDECK_MUTATION_KIND", "rename")
        .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(log));
    let mut child = OwnedChild(command.spawn().unwrap());
    let status = child.wait();
    assert!(
        status.success(),
        "{}",
        fs::read_to_string(&log_path).unwrap()
    );
    assert_eq!(fs::read(&observed).unwrap(), b"x");
    if arm.exists() {
        fs::remove_file(arm).unwrap();
    }
}

fn finite_review_phase8_child_dispatch() {
    let Some(root) = std::env::var_os("FLIGHTDECK_PHASE8_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let ring = std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap();
    let arm = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    let cold = std::env::var("FLIGHTDECK_PHASE8_COLD").unwrap() == "1";
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let store = Store::open(&root).unwrap();
    let mut owner = store
        .resume_finite_events(
            Some(RingReader::open_generation(&ring).unwrap()),
            &budget,
            &ledger,
        )
        .unwrap()
        .unwrap();
    fs::write(&arm, b"owned").unwrap();
    let mut failed = false;
    for _ in 0..20000 {
        match owner.step() {
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(5));
                failed = true;
                break;
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(other) => panic!("Busy interruption missing: {other:?}"),
        }
    }
    assert!(failed);
    let state = crate::transaction::normal::NormalState::decode(
        &fs::read(root.join("foreground")).unwrap(),
        store.recording,
    )
    .unwrap();
    assert_eq!(state.phase, Phase::UnresolvedExtent);
    assert_eq!(state.sequence, 0);
    assert!(state.salvage.is_some());
    let NamespaceState::Busy {
        transaction,
        sequence,
    } = decode_state(
        &fs::read(root.join("namespace-state")).unwrap(),
        store.recording,
    )
    .unwrap()
    else {
        panic!("Busy effect missing");
    };
    assert_eq!(transaction, state.transaction);
    assert!(sequence > 0);
    if cold {
        std::process::exit(0);
    }
    fs::remove_file(arm).unwrap();
    finish_finite(&mut owner);
}

#[test]
fn finite_review_native_io_numeric_row() {
    // Goal: native row failures retain their typed operating error.
    // Method: fail the real write or durability boundary, then retry through Store.
    finite_review_sync_counts(false, "row");
}

#[test]
fn finite_review_native_io_numeric_index() {
    // Goal: native index failures retain their typed operating error.
    // Method: fail the real write or durability boundary, then retry through Store.
    finite_review_sync_counts(false, "index");
}

#[test]
fn finite_review_native_io_captured_row() {
    // Goal: native row failures retain their typed operating error.
    // Method: fail the real write or durability boundary, then retry through Store.
    finite_review_sync_counts(true, "row");
}

#[test]
fn finite_review_native_io_captured_index() {
    // Goal: native index failures retain their typed operating error.
    // Method: fail the real write or durability boundary, then retry through Store.
    finite_review_sync_counts(true, "index");
}

#[test]
fn finite_review_directory_numeric_reopen_object_child_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "reopen-object-child", 0);
}

#[test]
fn finite_review_directory_numeric_reopen_object_child_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "reopen-object-child", 2);
}

#[test]
fn finite_review_directory_numeric_reopen_object_parent_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "reopen-object-parent", 0);
}

#[test]
fn finite_review_directory_numeric_reopen_object_parent_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "reopen-object-parent", 2);
}

#[test]
fn finite_review_directory_numeric_reopen_pending_child_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "reopen-pending-child", 0);
}

#[test]
fn finite_review_directory_numeric_reopen_pending_child_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "reopen-pending-child", 2);
}

#[test]
fn finite_review_directory_numeric_reopen_pending_parent_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "reopen-pending-parent", 0);
}

#[test]
fn finite_review_directory_numeric_reopen_pending_parent_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "reopen-pending-parent", 2);
}

#[test]
fn finite_review_directory_numeric_events_child_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "events-child", 0);
}

#[test]
fn finite_review_directory_numeric_events_child_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "events-child", 2);
}

#[test]
fn finite_review_directory_numeric_events_parent_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "events-parent", 0);
}

#[test]
fn finite_review_directory_numeric_events_parent_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "events-parent", 2);
}

#[test]
fn finite_review_directory_numeric_loss_child_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "loss-child", 0);
}

#[test]
fn finite_review_directory_numeric_loss_child_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "loss-child", 2);
}

#[test]
fn finite_review_directory_numeric_loss_parent_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "loss-parent", 0);
}

#[test]
fn finite_review_directory_numeric_loss_parent_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(false, "loss-parent", 2);
}

#[test]
fn finite_review_directory_captured_reopen_object_child_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "reopen-object-child", 0);
}

#[test]
fn finite_review_directory_captured_reopen_object_child_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "reopen-object-child", 2);
}

#[test]
fn finite_review_directory_captured_reopen_object_parent_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "reopen-object-parent", 0);
}

#[test]
fn finite_review_directory_captured_reopen_object_parent_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "reopen-object-parent", 2);
}

#[test]
fn finite_review_directory_captured_reopen_pending_child_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "reopen-pending-child", 0);
}

#[test]
fn finite_review_directory_captured_reopen_pending_child_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "reopen-pending-child", 2);
}

#[test]
fn finite_review_directory_captured_reopen_pending_parent_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "reopen-pending-parent", 0);
}

#[test]
fn finite_review_directory_captured_reopen_pending_parent_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "reopen-pending-parent", 2);
}

#[test]
fn finite_review_directory_captured_events_child_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "events-child", 0);
}

#[test]
fn finite_review_directory_captured_events_child_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "events-child", 2);
}

#[test]
fn finite_review_directory_captured_events_parent_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "events-parent", 0);
}

#[test]
fn finite_review_directory_captured_events_parent_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "events-parent", 2);
}

#[test]
fn finite_review_directory_captured_loss_child_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "loss-child", 0);
}

#[test]
fn finite_review_directory_captured_loss_child_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "loss-child", 2);
}

#[test]
fn finite_review_directory_captured_loss_parent_same_owner() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "loss-parent", 0);
}

#[test]
fn finite_review_directory_captured_loss_parent_cold() {
    // Goal: count a failing internal directory barrier and converge after interruption.
    // Method: inject EIO at the named child/parent sync and compare the native work trace.
    finite_review_directory(true, "loss-parent", 2);
}

fn finite_review_directory(captured: bool, boundary: &str, mode: u8) {
    let library_fixture = Fixture::new(false);
    let library = native_registration_build(&library_fixture);
    if !boundary.starts_with("reopen-") {
        finite_case(&library, captured, boundary, mode);
        return;
    }

    let (fixture, _) = inherited_fixture(captured);
    let expected =
        crate::event_recovery::batch::frame(&finite_expected_rows(&fixture, 3..5), captured)
            .unwrap();
    finite_case_child(&fixture, &library, "registered", 2);
    let transaction = finite_state(&fixture).transaction;
    let object = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(transaction).to_string());
    assert!(!object.exists());
    finite_case_child(&fixture, &library, boundary, mode);
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let store = Store::open(&fixture.root).unwrap();
    finite_case_complete(&fixture, &store, &budget, &ledger, boundary, &expected);
    finite_case_evidence(&fixture, boundary, captured, mode);
}
