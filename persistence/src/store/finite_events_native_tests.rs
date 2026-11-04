//! Process-isolated native errors on the public finite Store route.
use super::*;

#[test]
fn finite_events_native_faults_and_process_restart() {
    // Goal: native fault children use the production finite Store driver.
    // Method: dispatch only the isolated child's explicit environment to that driver.
    finite_events_native_faults_and_process_restart_child();
}
fn finite_events_native_faults_and_process_restart_child() {
    if let Some(root) = std::env::var_os("FLIGHTDECK_FINITE_ROOT") {
        finite_child(Path::new(&root));
    }
}

mod finite_events_native_fault_cases {
    use super::*;

    #[test]
    fn numeric_registered_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "registered", 0);
    }

    #[test]
    fn numeric_registered_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "registered", 2);
    }

    #[test]
    fn numeric_registered_write_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "registered-write", 0);
    }

    #[test]
    fn numeric_registered_write_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "registered-write", 2);
    }

    #[test]
    fn numeric_row_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "row", 0);
    }

    #[test]
    fn numeric_row_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "row", 2);
    }

    #[test]
    fn numeric_index_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "index", 0);
    }

    #[test]
    fn numeric_index_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "index", 2);
    }

    #[test]
    fn numeric_census_prefix_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "census-prefix", 0);
    }

    #[test]
    fn numeric_census_prefix_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "census-prefix", 2);
    }

    #[test]
    fn numeric_census_sync_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "census-sync", 0);
    }

    #[test]
    fn numeric_census_sync_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "census-sync", 2);
    }

    #[test]
    fn numeric_original_read_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "original-read", 0);
    }

    #[test]
    fn numeric_original_read_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "original-read", 2);
    }

    #[test]
    fn numeric_loss_meta_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "loss-meta", 0);
    }

    #[test]
    fn numeric_loss_meta_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "loss-meta", 2);
    }

    #[test]
    fn numeric_metadata_prefix_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "metadata-prefix", 0);
    }

    #[test]
    fn numeric_metadata_prefix_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "metadata-prefix", 2);
    }

    #[test]
    fn numeric_metadata_inner_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "metadata-inner", 0);
    }

    #[test]
    fn numeric_metadata_inner_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "metadata-inner", 2);
    }

    #[test]
    fn numeric_metadata_sync_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "metadata-sync", 0);
    }

    #[test]
    fn numeric_metadata_sync_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "metadata-sync", 2);
    }

    #[test]
    fn numeric_busy_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "busy", 0);
    }

    #[test]
    fn numeric_busy_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "busy", 2);
    }

    #[test]
    fn numeric_gate_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "gate", 0);
    }

    #[test]
    fn numeric_gate_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "gate", 2);
    }

    #[test]
    fn numeric_original_unlink_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "original-unlink", 0);
    }

    #[test]
    fn numeric_original_unlink_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "original-unlink", 2);
    }

    #[test]
    fn numeric_witness_unlink_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "witness-unlink", 0);
    }

    #[test]
    fn numeric_witness_unlink_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "witness-unlink", 2);
    }

    #[test]
    fn numeric_idle_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "idle", 0);
    }

    #[test]
    fn numeric_idle_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(false, "idle", 2);
    }

    #[test]
    fn captured_registered_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "registered", 0);
    }

    #[test]
    fn captured_registered_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "registered", 2);
    }

    #[test]
    fn captured_registered_write_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "registered-write", 0);
    }

    #[test]
    fn captured_registered_write_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "registered-write", 2);
    }

    #[test]
    fn captured_row_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "row", 0);
    }

    #[test]
    fn captured_row_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "row", 2);
    }

    #[test]
    fn captured_index_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "index", 0);
    }

    #[test]
    fn captured_index_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "index", 2);
    }

    #[test]
    fn captured_census_prefix_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "census-prefix", 0);
    }

    #[test]
    fn captured_census_prefix_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "census-prefix", 2);
    }

    #[test]
    fn captured_census_sync_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "census-sync", 0);
    }

    #[test]
    fn captured_census_sync_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "census-sync", 2);
    }

    #[test]
    fn captured_original_read_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "original-read", 0);
    }

    #[test]
    fn captured_original_read_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "original-read", 2);
    }

    #[test]
    fn captured_loss_meta_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "loss-meta", 0);
    }

    #[test]
    fn captured_loss_meta_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "loss-meta", 2);
    }

    #[test]
    fn captured_metadata_prefix_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "metadata-prefix", 0);
    }

    #[test]
    fn captured_metadata_prefix_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "metadata-prefix", 2);
    }

    #[test]
    fn captured_metadata_inner_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "metadata-inner", 0);
    }

    #[test]
    fn captured_metadata_inner_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "metadata-inner", 2);
    }

    #[test]
    fn captured_metadata_sync_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "metadata-sync", 0);
    }

    #[test]
    fn captured_metadata_sync_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "metadata-sync", 2);
    }

    #[test]
    fn captured_busy_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "busy", 0);
    }

    #[test]
    fn captured_busy_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "busy", 2);
    }

    #[test]
    fn captured_gate_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "gate", 0);
    }

    #[test]
    fn captured_gate_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "gate", 2);
    }

    #[test]
    fn captured_original_unlink_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "original-unlink", 0);
    }

    #[test]
    fn captured_original_unlink_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "original-unlink", 2);
    }

    #[test]
    fn captured_witness_unlink_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "witness-unlink", 0);
    }

    #[test]
    fn captured_witness_unlink_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "witness-unlink", 2);
    }

    #[test]
    fn captured_idle_same_owner() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "idle", 0);
    }

    #[test]
    fn captured_idle_cold() {
        // Goal: each interrupted finite transaction retains exact rows and reaches its disposition.
        // Method: inject one native failure, then retry the owner or reopen after process exit.
        run(true, "idle", 2);
    }

    fn run(captured: bool, boundary: &str, mode: u8) {
        let fixture = Fixture::new(false);
        let library = native_registration_build(&fixture);
        finite_case(&library, captured, boundary, mode);
    }
}
fn finite_case(library: &Path, captured: bool, boundary: &str, mode: u8) {
    let (mut fixture, _) = inherited_fixture(captured);
    let tick = encode_payload_tick(&mut fixture.reader(), 4);
    let expected =
        crate::event_recovery::batch::frame(&inherited_rows(&fixture, &tick)[..2], captured)
            .unwrap();
    if boundary.starts_with("loss-") {
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        drop(
            store
                .begin_finite_events(fixture.reader(), &budget, &ledger)
                .unwrap(),
        );
        drop(store);
        for number in 5..=6 {
            if captured {
                publish_captured_pair_at(&mut fixture, number, number);
            } else {
                publish_numeric_pair_at(&mut fixture, number, number);
            }
        }
    }
    finite_case_child(&fixture, library, boundary, mode);
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let store = Store::open(&fixture.root).unwrap();
    if boundary == "loss-prefix" && mode == 2 {
        finite_case_partial_zero(&fixture, &store, &budget, &ledger);
    }
    if matches!(boundary, "census-128" | "census-inode") {
        finite_case_census(&fixture, &store, &budget, &ledger, boundary, captured);
        finite_case_evidence(&fixture, boundary, captured, mode);
        return;
    }
    if matches!(boundary, "loss-row" | "loss-torn") {
        finite_case_scratch(
            &fixture, &store, &budget, &ledger, boundary, captured, &expected,
        );
        finite_case_evidence(&fixture, boundary, captured, mode);
        return;
    }
    finite_case_complete(&fixture, &store, &budget, &ledger, boundary, &expected);
    finite_case_evidence(&fixture, boundary, captured, mode);
}
fn finite_child(root: &Path) {
    let boundary = std::env::var("FLIGHTDECK_FINITE_BOUNDARY").unwrap();
    let mode = std::env::var("FLIGHTDECK_FINITE_MODE").unwrap();
    let arm = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    let ring = std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let store = Store::open(root).unwrap();
    finite_child_sync_control(root);
    if !matches!(
        boundary.as_str(),
        "busy" | "idle" | "events-child" | "events-parent"
    ) {
        fs::write(&arm, b"owned").unwrap();
    }
    let result = store.begin_finite_events(
        RingReader::open_generation(&ring).unwrap(),
        &budget,
        &ledger,
    );
    let (mut owner, failed) = match result {
        Ok(owner) => (owner, false),
        Err(error) => {
            assert_eq!(error.raw_os_error(), Some(5));
            finite_work(&budget, &ledger);
            if mode == "2" {
                std::process::exit(0);
            }
            fs::remove_file(&arm).unwrap();
            let owner = store
                .begin_finite_events(
                    RingReader::open_generation(&ring).unwrap(),
                    &budget,
                    &ledger,
                )
                .unwrap();
            (owner, true)
        }
    };
    if matches!(boundary.as_str(), "events-child" | "events-parent") {
        fs::write(&arm, b"owned").unwrap();
    }
    let child = FiniteChild {
        root,
        boundary,
        mode,
        arm,
        budget,
        ledger,
    };
    finite_child_drive(&child, &store, &mut owner, failed);
}
struct FiniteChild<'a> {
    root: &'a Path,
    boundary: String,
    mode: String,
    arm: PathBuf,
    budget: Arc<crate::resources::ByteBudget>,
    ledger: PublicationLedger,
}

fn finite_child_drive(
    child: &FiniteChild<'_>,
    store: &Store,
    owner: &mut FiniteEvents<'_>,
    mut failed: bool,
) {
    for _ in 0..20000 {
        finite_child_arm(child.root, store, &child.boundary, failed, &child.arm);
        match owner.step() {
            Err(error) => {
                assert!(!failed, "second error: {error:?}");
                failed = true;
                if matches!(child.boundary.as_str(), "row" | "index") {
                    assert_eq!(error.raw_os_error(), Some(5));
                }
                assert!(
                    error.kind() == io::ErrorKind::Other
                        || error.kind() == io::Error::from_raw_os_error(5).kind()
                );
                finite_work(&child.budget, &child.ledger);
                if child.mode == "2" {
                    std::process::exit(0);
                }
                fs::remove_file(&child.arm).unwrap();
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(RowPublicationStep::Idle) => {
                assert!(failed);
                finite_work(&child.budget, &child.ledger);
                return;
            }
            Ok(RowPublicationStep::Unresolved) => panic!("unexpected unresolved finite proof"),
        }
    }
    panic!("finite native did not converge");
}

fn finite_child_sync_control(root: &Path) {
    if let Some(trace) = std::env::var_os("FLIGHTDECK_PUBLICATION_TRACE") {
        fs::write(&trace, b"").unwrap();
        private::full_sync(
            &PrivateDirectory::open(root)
                .unwrap()
                .open_file("foreground")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(fs::read(&trace).unwrap(), b"SS");
        fs::write(trace, b"").unwrap();
    }
}

fn finite_work(budget: &Arc<crate::resources::ByteBudget>, ledger: &PublicationLedger) {
    if let Some(trace) = std::env::var_os("FLIGHTDECK_PUBLICATION_TRACE") {
        let observed = fs::read(trace).unwrap();
        let syncs: u64 = observed.iter().map(|byte| u64::from(*byte == b'S')).sum();
        let work = ledger.work();
        let interrupted = if std::env::var_os("FLIGHTDECK_PRIVATE_SYNC_TARGET").is_some() {
            collector::u64_from_usize(
                fs::read(std::env::var_os("FLIGHTDECK_PRIVATE_SYNC_OBSERVED").unwrap())
                    .unwrap()
                    .len(),
            )
        } else {
            0
        };
        // Rust sync_all and the explicit barrier each issue F_FULLFSYNC on this toolchain.
        assert_eq!(
            2 * (work.full_syncs + work.canonical.full_syncs) - interrupted,
            syncs
        );
    }
    let footprint = crate::resources::Footprint::read().unwrap();
    let bytes = format!(
        "shared_peak={} footprint={} native_lifetime_peak={} resident={} work={:?}\n",
        budget.used_peak(),
        footprint.current_bytes,
        footprint.lifetime_bytes_max,
        footprint.resident_bytes,
        ledger.work()
    );
    fs::write(std::env::var_os("FLIGHTDECK_FINITE_WORK").unwrap(), bytes).unwrap();
}

mod finite_events_native_completion_and_descriptor_scope {
    use super::*;

    #[test]
    fn numeric_metadata_sync_same_owner() {
        // Goal: completed proof survives restart within the sampled descriptor bounds.
        // Method: observe the native fixture and exact canonical result; this is not a capacity
        // run.
        run(false, "metadata-sync", 0);
    }

    #[test]
    fn numeric_loss_meta_cold() {
        // Goal: completed proof survives restart within the sampled descriptor bounds.
        // Method: observe the native fixture and exact canonical result; this is not a capacity
        // run.
        run(false, "loss-meta", 2);
    }

    #[test]
    fn captured_metadata_sync_same_owner() {
        // Goal: completed proof survives restart within the sampled descriptor bounds.
        // Method: observe the native fixture and exact canonical result; this is not a capacity
        // run.
        run(true, "metadata-sync", 0);
    }

    #[test]
    fn captured_loss_meta_cold() {
        // Goal: completed proof survives restart within the sampled descriptor bounds.
        // Method: observe the native fixture and exact canonical result; this is not a capacity
        // run.
        run(true, "loss-meta", 2);
    }

    fn run(captured: bool, boundary: &str, mode: u8) {
        let fixture = Fixture::new(false);
        let library = native_registration_build(&fixture);
        finite_case(&library, captured, boundary, mode);
    }
}

mod finite_events_native_zero_metadata_inspects_candidate_scratch {
    use super::*;

    #[test]
    fn numeric_loss_row_cold() {
        // Goal: completed zero-row metadata cannot conceal a conflicting candidate survivor.
        // Method: reopen with valid or torn candidate scratch after a native metadata interruption.
        run(false, "loss-row", 2);
    }

    #[test]
    fn numeric_loss_torn_cold() {
        // Goal: completed zero-row metadata cannot conceal a conflicting candidate survivor.
        // Method: reopen with valid or torn candidate scratch after a native metadata interruption.
        run(false, "loss-torn", 2);
    }

    #[test]
    fn captured_loss_row_cold() {
        // Goal: completed zero-row metadata cannot conceal a conflicting candidate survivor.
        // Method: reopen with valid or torn candidate scratch after a native metadata interruption.
        run(true, "loss-row", 2);
    }

    #[test]
    fn captured_loss_torn_cold() {
        // Goal: completed zero-row metadata cannot conceal a conflicting candidate survivor.
        // Method: reopen with valid or torn candidate scratch after a native metadata interruption.
        run(true, "loss-torn", 2);
    }

    fn run(captured: bool, boundary: &str, mode: u8) {
        let fixture = Fixture::new(false);
        let library = native_registration_build(&fixture);
        finite_case(&library, captured, boundary, mode);
    }
}

mod finite_events_native_cold_census_keeps_selected_universe {
    use super::*;

    #[test]
    fn numeric_census_128_cold() {
        // Goal: a cold census refuses substituted or newly populated original material.
        // Method: change one original after partial witness publication and reopen the Store.
        run(false, "census-128", 2);
    }

    #[test]
    fn numeric_census_inode_cold() {
        // Goal: a cold census refuses substituted or newly populated original material.
        // Method: change one original after partial witness publication and reopen the Store.
        run(false, "census-inode", 2);
    }

    #[test]
    fn captured_census_128_cold() {
        // Goal: a cold census refuses substituted or newly populated original material.
        // Method: change one original after partial witness publication and reopen the Store.
        run(true, "census-128", 2);
    }

    #[test]
    fn captured_census_inode_cold() {
        // Goal: a cold census refuses substituted or newly populated original material.
        // Method: change one original after partial witness publication and reopen the Store.
        run(true, "census-inode", 2);
    }

    fn run(captured: bool, boundary: &str, mode: u8) {
        let fixture = Fixture::new(false);
        let library = native_registration_build(&fixture);
        finite_case(&library, captured, boundary, mode);
    }
}

fn finite_case_child(fixture: &Fixture, library: &Path, boundary: &str, mode: u8) {
    let root = fixture.root.canonicalize().unwrap();
    let log_path = fixture.base.join("finite-child.log");
    let log = File::create(&log_path).unwrap();
    let observed = fixture.base.join("finite-observed");
    fs::write(&observed, b"").unwrap();
    fs::write(fixture.base.join("finite-sync-trace"), b"").unwrap();
    fs::write(fixture.base.join("finite-fds"), b"").unwrap();
    let arm = fixture.base.join("finite-arm");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::inherited_events_tests::",
                "finite_events_native_tests::finite_events_native_faults_and_process_restart",
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_FINITE_ROOT", &root)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env(
            "FLIGHTDECK_FINITE_BOUNDARY",
            if boundary.starts_with("loss-") {
                "loss-meta"
            } else {
                boundary
            },
        )
        .env("FLIGHTDECK_FINITE_MODE", mode.to_string())
        .env("FLIGHTDECK_REGISTRATION_ARMED", &arm)
        .env("FLIGHTDECK_PRIVATE_SYNC_OBSERVED", &observed)
        .env("FLIGHTDECK_FD_ROOT", &root)
        .env("FLIGHTDECK_FD_LOG", fixture.base.join("finite-fds"))
        .env(
            "FLIGHTDECK_PUBLICATION_TRACE",
            fixture.base.join("finite-sync-trace"),
        )
        .env("FLIGHTDECK_FINITE_WORK", fixture.base.join("finite-work"))
        .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(log));
    finite_case_target(&mut command, fixture, boundary, &root);
    if boundary == "metadata-sync" && mode == 0 {
        fs::write(fixture.base.join("finite-fds"), b"").unwrap();
        command
            .env("FLIGHTDECK_FD_ROOT", &root)
            .env("FLIGHTDECK_FD_LOG", fixture.base.join("finite-fds"));
    }
    let mut child = OwnedChild(command.spawn().unwrap());
    let status = child.wait();
    assert!(
        status.success(),
        "{boundary} mode{mode} {status}: {}",
        fs::read_to_string(&log_path).unwrap()
    );
    assert!(
        !fs::read(&observed).unwrap().is_empty(),
        "native boundary did not fire: {boundary}"
    );
    if arm.exists() {
        fs::remove_file(&arm).unwrap();
    }
}
fn finite_case_target(
    command: &mut std::process::Command,
    fixture: &Fixture,
    boundary: &str,
    root: &Path,
) {
    match boundary {
        "events-child" | "events-parent" => {
            command.env(
                "FLIGHTDECK_PRIVATE_SYNC_TARGET",
                if boundary == "events-child" {
                    "@/pending/events"
                } else {
                    "@/pending"
                },
            );
        }
        "reopen-object-child"
        | "reopen-object-parent"
        | "reopen-pending-child"
        | "reopen-pending-parent"
        | "loss-child"
        | "loss-parent" => {
            finite_case_directory_target(command, fixture, boundary, root);
        }
        "loss-prefix" => {
            command
                .env("FLIGHTDECK_METADATA_WRITE_TARGET", "@/loss/metadata")
                .env("FLIGHTDECK_METADATA_WRITE_MODE", "prefix");
        }
        "original-read" => {
            command.env(
                "FLIGHTDECK_INVENTORY_READ_TARGET",
                "@/pending/events/0000.parquet",
            );
        }
        "registered" => {
            command
                .env("FLIGHTDECK_MUTATION_TARGET", root.join("foreground"))
                .env("FLIGHTDECK_MUTATION_KIND", "rename");
        }
        "registered-write" => {
            command
                .env(
                    "FLIGHTDECK_METADATA_WRITE_TARGET",
                    root.join("foreground.pending"),
                )
                .env("FLIGHTDECK_METADATA_WRITE_MODE", "complete");
        }
        "index" | "census-prefix" | "census-128" | "census-inode" | "metadata-prefix"
        | "metadata-inner" => {
            let suffix = match boundary {
                "index" => "@/pending/events/index",
                "census-prefix" | "census-128" | "census-inode" => "@/original-census",
                _ => "@/fragment-0/metadata",
            };
            command.env("FLIGHTDECK_METADATA_WRITE_TARGET", suffix).env(
                "FLIGHTDECK_METADATA_WRITE_MODE",
                if matches!(boundary, "census-128" | "census-inode") {
                    "complete"
                } else if boundary == "metadata-inner" {
                    "inner"
                } else {
                    "prefix"
                },
            );
        }
        _ => finite_case_mutation_target(command, fixture, boundary, root),
    }
}

fn finite_case_directory_target(
    command: &mut std::process::Command,
    fixture: &Fixture,
    boundary: &str,
    root: &Path,
) {
    let state = finite_state(fixture);
    let base = root.join("recovery-data");
    let object = base.join(collector::recording::identity_text(state.transaction).to_string());
    let target = match boundary {
        "reopen-object-child" => object.clone(),
        "reopen-object-parent" => base,
        "reopen-pending-child" => object.join("pending"),
        "reopen-pending-parent" => {
            command.env("FLIGHTDECK_SYNC_AFTER_EXISTS", object.join("pending"));
            object
        }
        "loss-child" => object.join("loss"),
        "loss-parent" => {
            command.env("FLIGHTDECK_SYNC_AFTER_EXISTS", object.join("loss"));
            object
        }
        _ => unreachable!(),
    };
    command.env("FLIGHTDECK_PRIVATE_SYNC_TARGET", target);
}

fn finite_case_mutation_target(
    command: &mut std::process::Command,
    fixture: &Fixture,
    boundary: &str,
    root: &Path,
) {
    match boundary {
        "original-unlink" | "witness-unlink" | "gate" => {
            let suffix = match boundary {
                "original-unlink" => "@/pending/events/0000.parquet".to_owned(),
                "witness-unlink" => "@/original-census".to_owned(),
                _ => format!(
                    "@/{}",
                    crate::canonical::CanonicalKey::source(
                        crate::canonical::Namespace::Fragments,
                        &fixture.run,
                        1
                    )
                    .unwrap()
                    .file_name()
                ),
            };
            command.env("FLIGHTDECK_MUTATION_TARGET", suffix).env(
                "FLIGHTDECK_MUTATION_KIND",
                if boundary == "gate" { "link" } else { "unlink" },
            );
        }
        "busy" | "idle" => {
            command
                .env(
                    "FLIGHTDECK_MUTATION_TARGET",
                    root.join(if boundary == "busy" {
                        "namespace-state"
                    } else {
                        "foreground"
                    }),
                )
                .env("FLIGHTDECK_MUTATION_KIND", "rename");
        }
        _ => {
            let suffix = match boundary {
                "row" => "@/pending/events/0000.parquet",
                "census-sync" => "@/original-census",
                "metadata-sync" => "@/fragment-0/metadata",
                "loss-meta" | "loss-row" | "loss-torn" => "@/loss/metadata",
                _ => unreachable!(),
            };
            command
                .env("FLIGHTDECK_PRIVATE_SYNC_TARGET", suffix)
                .env("FLIGHTDECK_SYNC_EFFECT", "1");
        }
    }
}

fn finite_case_census(
    fixture: &Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
    boundary: &str,
    captured: bool,
) {
    let state =
        crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
            .unwrap();
    assert_eq!(state.phase, Phase::Registered);
    assert_eq!(state.census.kind, 1);
    let object = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(state.transaction).to_string());
    let witness = fs::read(object.join("original-census")).unwrap();
    assert_eq!(witness.len(), 256 + 128 * 96);
    let events = PrivateDirectory::open(&object.join("pending/events")).unwrap();
    let changed = if boundary == "census-128" {
        let mut frame =
            crate::event_recovery::batch::frame(&finite_expected_rows(fixture, 5..7), captured)
                .unwrap();
        let mut file = events.create_file("4095.parquet").unwrap();
        crate::encoder::encode_write(&mut frame, &mut file).unwrap();
        private::full_sync(&file).unwrap();
        events.full_sync().unwrap();
        object.join("pending/events/4095.parquet")
    } else {
        let path = object.join("pending/events/0000.parquet");
        let bytes = fs::read(&path).unwrap();
        fs::rename(&path, fixture.base.join("prior-original.parquet")).unwrap();
        let mut file = events.create_file("0000.parquet").unwrap();
        file.write_all(&bytes).unwrap();
        private::full_sync(&file).unwrap();
        events.full_sync().unwrap();
        path
    };
    drop(events);
    let bytes = fs::read(&changed).unwrap();
    let slot = fixture.slot();
    let mut owner = store
        .resume_finite_events(Some(fixture.reader()), budget, ledger)
        .unwrap()
        .unwrap();
    let mut refused = false;
    for _ in 0..20000 {
        match owner.step() {
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                refused = true;
                break;
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(result) => panic!("changed census admitted: {result:?}"),
        }
    }
    assert!(refused);
    assert_eq!(fixture.slot(), slot);
    assert_eq!(fs::read(&changed).unwrap(), bytes);
    assert_eq!(fs::read(object.join("original-census")).unwrap(), witness);
    assert!(!object.join("fragment-0").exists());
    eprintln!("finite cold {boundary} captured={captured}: full digest refused before append");
}

fn finite_case_scratch(
    fixture: &Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
    boundary: &str,
    captured: bool,
    expected: &polars::prelude::DataFrame,
) {
    let state =
        crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
            .unwrap();
    assert!(state.row_plan.is_some());
    assert_eq!(state.census.kind, 2);
    let object = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(state.transaction).to_string());
    finite_case_scratch_write(&object, boundary, expected);
    let bytes = fs::read(object.join("fragment-0/0000.parquet")).unwrap();
    let slot = fixture.slot();
    let mut owner = store
        .resume_finite_events(None, budget, ledger)
        .unwrap()
        .unwrap();
    finite_case_scratch_drive(&mut owner, fixture, &object, boundary);
    drop(owner);
    if boundary == "loss-row" {
        assert_eq!(fixture.slot(), slot);
        assert_eq!(
            fs::read(object.join("fragment-0/0000.parquet")).unwrap(),
            bytes
        );
        assert!(
            store
                .source_frontier(
                    &fixture.run,
                    budget,
                    &mut crate::canonical::ReadWork::default()
                )
                .unwrap()
                .is_none()
        );
    } else {
        finite_domain_oracle(fixture, state.transaction, 1, 5, &[], budget);
        assert!(!object.join("fragment-0").exists());
    }
    eprintln!("finite native {boundary} captured={captured}: count0 scratch control passed");
}

fn finite_case_complete(
    fixture: &Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
    boundary: &str,
    expected: &polars::prelude::DataFrame,
) {
    if !matches!(
        bootstrap::codec::decode_slot(
            &fixture.slot(),
            fixture.run.recording,
            bootstrap::codec::SlotKind::Foreground
        )
        .unwrap(),
        bootstrap::codec::Slot::Idle
    ) || fs::read_dir(fixture.root.join("event-fragments"))
        .unwrap()
        .next()
        .is_none()
    {
        let mut owner = if boundary.starts_with("loss-")
            && !matches!(boundary, "loss-prefix" | "loss-child" | "loss-parent")
        {
            store
                .resume_finite_events(None, budget, ledger)
                .unwrap()
                .unwrap()
        } else {
            store
                .begin_finite_events(fixture.reader(), budget, ledger)
                .unwrap()
        };
        finish_finite(&mut owner);
        drop(owner);
    }
    let frontier = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    let Predecessor::Canonical {
        transaction,
        kind,
        origin,
    } = *frontier.predecessor()
    else {
        panic!("finite gate");
    };
    assert_eq!(
        kind,
        if boundary.starts_with("loss-") {
            crate::transaction::CanonicalKind::FiniteEventLoss
        } else {
            crate::transaction::CanonicalKind::FiniteEvents
        }
    );
    assert_eq!(origin, 1);
    assert_eq!(frontier.event_end(), 5);
    assert_eq!(frontier.publication_end(), 1);
    assert_eq!(frontier.prior_window(), None);
    if boundary.starts_with("loss-") {
        finite_domain_oracle(fixture, transaction, 1, 5, &[], budget);
    } else {
        finite_rows_oracle(fixture, transaction, expected, budget);
    }
}

fn finite_case_evidence(fixture: &Fixture, boundary: &str, captured: bool, mode: u8) {
    if boundary == "metadata-sync" && mode == 0 {
        let bytes = fs::read(fixture.base.join("finite-fds")).unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(bytes.len() % 4, 0);
        let namespace = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .max()
            .unwrap();
        let raw = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u16::from_le_bytes([bytes[2], bytes[3]]))
            .max()
            .unwrap();
        assert!(namespace <= 8, "finite namespace {namespace}");
        eprintln!("finite native FD captured={captured} namespace={namespace} raw={raw}");
    }
    if let Some(output) = std::env::var_os("FLIGHTDECK_FINITE_EVIDENCE") {
        let output = PathBuf::from(output);
        fs::create_dir_all(&output).unwrap();
        for file in [
            "finite-child.log",
            "finite-work",
            "finite-observed",
            "finite-fds",
        ] {
            if let Ok(bytes) = fs::read(fixture.base.join(file)) {
                fs::write(
                    output.join(format!("{captured}-{boundary}-{mode}-{file}")),
                    bytes,
                )
                .unwrap();
            }
        }
    }
    eprintln!(
        "finite native captured={captured} boundary={boundary} mode={mode}: {}",
        fs::read_to_string(fixture.base.join("finite-work")).unwrap()
    );
}

fn finite_child_arm(root: &Path, store: &Store, boundary: &str, failed: bool, arm: &Path) {
    if matches!(boundary, "busy" | "idle") && !failed && !arm.exists() {
        let state = crate::transaction::normal::NormalState::decode(
            &fs::read(root.join("foreground")).unwrap(),
            store.recording,
        )
        .unwrap();
        if (boundary == "busy"
            && state.phase == Phase::SalvagePlanned
            && state.row_plan.unwrap().rows != [0, 0])
            || (boundary == "idle" && state.cleanup.region == 8)
        {
            fs::write(arm, b"owned").unwrap();
        }
    }
}

fn finite_case_scratch_write(object: &Path, boundary: &str, expected: &polars::prelude::DataFrame) {
    let candidate = PrivateDirectory::open(object)
        .unwrap()
        .create_directory("fragment-0")
        .unwrap();
    let mut file = candidate.create_file("0000.parquet").unwrap();
    if boundary == "loss-row" {
        let mut frame = expected.clone();
        crate::encoder::encode_write(&mut frame, &mut file).unwrap();
    } else {
        file.write_all(b"PAR1 interrupted candidate").unwrap();
    }
    private::full_sync(&file).unwrap();
    candidate.full_sync().unwrap();
    drop(file);
    drop(candidate);
}

fn finite_case_scratch_drive(
    owner: &mut FiniteEvents<'_>,
    fixture: &Fixture,
    object: &Path,
    boundary: &str,
) {
    let mut result = false;
    for _ in 0..20000 {
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if !matches!(state.phase, Phase::SalvageCommitted) {
            assert!(object.join("fragment-0/0000.parquet").exists());
            assert!(object.join("pending").exists());
        }
        match owner.step() {
            Err(error) => {
                assert_eq!(boundary, "loss-row");
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                result = true;
                break;
            }
            Ok(RowPublicationStep::Idle) => {
                assert_eq!(boundary, "loss-torn");
                result = true;
                break;
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(RowPublicationStep::Unresolved) => panic!("durable zero proof unresolved"),
        }
    }
    assert!(result);
}

mod finite_events_native_partial_zero_metadata_cannot_settle_count {
    use super::*;

    #[test]
    fn numeric_loss_prefix_same_owner() {
        // Goal: torn zero-row metadata grants no source-free loss authority.
        // Method: interrupt the native write and recover with the original matching source.
        run(false, "loss-prefix", 0);
    }

    #[test]
    fn numeric_loss_prefix_cold() {
        // Goal: torn zero-row metadata grants no source-free loss authority.
        // Method: interrupt the native write and recover with the original matching source.
        run(false, "loss-prefix", 2);
    }

    #[test]
    fn captured_loss_prefix_same_owner() {
        // Goal: torn zero-row metadata grants no source-free loss authority.
        // Method: interrupt the native write and recover with the original matching source.
        run(true, "loss-prefix", 0);
    }

    #[test]
    fn captured_loss_prefix_cold() {
        // Goal: torn zero-row metadata grants no source-free loss authority.
        // Method: interrupt the native write and recover with the original matching source.
        run(true, "loss-prefix", 2);
    }

    fn run(captured: bool, boundary: &str, mode: u8) {
        let fixture = Fixture::new(false);
        let library = native_registration_build(&fixture);
        finite_case(&library, captured, boundary, mode);
    }
}
fn finite_case_partial_zero(
    fixture: &Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
) {
    let state = finite_state(fixture);
    assert!(state.row_plan.is_some());
    assert!(state.salvage.is_none());
    let object = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(state.transaction).to_string());
    let metadata = object.join("loss/metadata");
    let bytes = fs::read(&metadata).unwrap();
    let witness = fs::read(object.join("original-census")).unwrap();
    assert_eq!(bytes.len(), 137);
    let mut owner = store
        .resume_finite_events(None, budget, ledger)
        .unwrap()
        .unwrap();
    let mut unresolved = false;
    for _ in 0..20000 {
        match owner.step().unwrap() {
            RowPublicationStep::Continue => {}
            RowPublicationStep::Unresolved => {
                unresolved = true;
                break;
            }
            RowPublicationStep::Idle => panic!("partial loss metadata became authority"),
        }
    }
    assert!(unresolved);
    assert_eq!(finite_state(fixture).phase, Phase::UnresolvedExtent);
    assert_eq!(finite_state(fixture).sequence, 0);
    assert_eq!(fs::read(&metadata).unwrap(), bytes);
    assert!(object.join("pending").exists());
    assert_eq!(fs::read(object.join("original-census")).unwrap(), witness);
    assert_eq!(
        fs::read_dir(fixture.root.join("event-fragments"))
            .unwrap()
            .count(),
        0
    );
}

#[path = "finite_review_native_tests.rs"]
mod finite_review_native_tests;
