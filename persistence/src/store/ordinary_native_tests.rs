//! Faulted reconstruction with genuine Store and separate-process reacquisition.
use super::*;

const CHILD: &str = concat!(
    "store::registration_tests::loss_proof_tests::ordinary_recovery_tests::",
    "ordinary_complete_tests::ordinary_refusal_tests::ordinary_native_tests::",
    "ordinary_native_recovery_faults"
);

#[test]
fn ordinary_mapping_refusal_retries_numeric() {
    // Goal: a native mapping refusal preserves numeric recovery for same-owner retry.
    // Method: inject ENOMEM at the exact owned memory table and then remove the fault.
    native_case_run(false, "map-table", "retry");
}

#[test]
fn ordinary_mapping_refusal_retries_captured() {
    // Goal: captured recovery retains its raw payload across a native mapping refusal.
    // Method: inject ENOMEM at the exact owned memory table and retry the consuming owner.
    native_case_run(true, "map-table", "retry");
}

#[test]
fn ordinary_mapping_refusal_reopens_store_numeric() {
    // Goal: Store reacquisition retains numeric recovery authority after mapping refusal.
    // Method: inject ENOMEM, drop the owner and Store, then recover the unchanged payload.
    native_case_run(false, "map-table", "store");
}

#[test]
fn ordinary_mapping_refusal_reopens_store_captured() {
    // Goal: captured recovery survives Store reacquisition after mapping refusal.
    // Method: preserve the occupied slot and raw bytes, then reacquire Store and finish.
    native_case_run(true, "map-table", "store");
}

#[test]
fn ordinary_mapping_refusal_restarts_process_numeric() {
    // Goal: a mapping error leaves sufficient authority for a new recovery process.
    // Method: return ENOMEM in one child and complete numeric recovery in another.
    native_case_run(false, "map-table", "process");
}

#[test]
fn ordinary_mapping_refusal_restarts_process_captured() {
    // Goal: captured recovery can resume in another process after a mapping refusal.
    // Method: fail only the owned table mapping, then verify exact raw data and frontiers.
    native_case_run(true, "map-table", "process");
}

fn native_case_run(captured: bool, boundary: &str, recovery: &str) {
    let compiler = Fixture::new(false);
    let library = native_registration_build(&compiler);
    native_case(&library, captured, boundary, recovery);
}

#[test]
fn ordinary_native_recovery_faults() {
    // Goal: real descriptor I/O and durable publication effects reconcile with exact raw data.
    // Method: native fault at an owned boundary, then retry, reacquire Store, or exit the process.
    if let Some(root) = std::env::var_os("FD_ORDINARY_NATIVE_ROOT") {
        native_child(Path::new(&root));
        return;
    }
    native_case_run(false, "constructor-open", "process");
}
fn native_case(library: &Path, captured: bool, boundary: &str, recovery: &str) {
    let (mut fixture, object, budget) = staged(captured, true);
    eprintln!(
        "ORDINARY-NATIVE fixture={} source={}",
        fixture.base.display(),
        fixture.writer.name()
    );
    let raw = raw_bytes(&object.join("pending"));
    let armed = fixture.base.join("ordinary-armed");
    let observed = fixture.base.join("ordinary-observed");
    let descriptors = fixture.base.join("ordinary-fds");
    fs::write(&observed, []).unwrap();
    fs::write(&descriptors, []).unwrap();
    let mut command = native_command(&fixture, library, boundary, recovery);
    command
        .env("FLIGHTDECK_REGISTRATION_ARMED", &armed)
        .env("FLIGHTDECK_PRIVATE_SYNC_OBSERVED", &observed)
        .env("FLIGHTDECK_FD_ROOT", fixture.root.canonicalize().unwrap())
        .env("FLIGHTDECK_FD_LOG", &descriptors);
    let target = native_case_target(&fixture, &object, boundary);
    native_case_faults(&mut command, boundary, &target);
    let mut child = OwnedChild(command.spawn().unwrap());
    assert!(
        child.wait().success(),
        "fault profile={captured} boundary={boundary}"
    );
    assert!(
        !fs::read(&observed).unwrap().is_empty(),
        "fault did not fire: {boundary}"
    );
    assert!(!armed.exists());
    if recovery == "process" {
        if matches!(boundary, "complete" | "ready" | "busy" | "gate" | "idle") {
            collector::ring::unlink_generation(fixture.writer.name()).unwrap();
            fixture.transport_removed = true;
            assert_eq!(
                RingReader::open_generation(fixture.writer.name())
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::NotFound
            );
        }
        let mut command = native_command(&fixture, library, boundary, "resume");
        command
            .env("FLIGHTDECK_FD_ROOT", fixture.root.canonicalize().unwrap())
            .env("FLIGHTDECK_FD_LOG", &descriptors);
        let mut child = OwnedChild(command.spawn().unwrap());
        assert!(child.wait().success(), "process resume {boundary}");
    }
    native_case_observe(
        &fixture, &object, &budget, &raw, captured, boundary, recovery,
    );
}
fn native_case_target(fixture: &Fixture, object: &Path, boundary: &str) -> PathBuf {
    let native_object = object.canonicalize().unwrap();
    let native_root = fixture.root.canonicalize().unwrap();
    match boundary {
        "constructor-open" | "work-ready" => native_root.join("data"),
        "work-events" => native_object.join("pending/events"),
        "work-ticks" => native_object.join("pending/ticks.parquet"),
        "open-batch" => native_object.join("pending/events/0000.parquet"),
        "read-table" | "map-table" => native_object.join("pending/memory.parquet"),
        "read-index" => native_object.join("pending/events/index"),
        "create" | "prefix" | "inner" | "complete" | "metadata-sync" => {
            native_object.join("pending/metadata")
        }
        "ready" | "idle" => native_root.join("foreground.pending"),
        "rename" | "ready-directory" => native_object.join("ready"),
        "busy" | "quiet" => native_root.join("namespace-state.pending"),
        "gate" => {
            let transaction = object.file_name().unwrap().to_str().unwrap();
            let key = crate::canonical::CanonicalKey::source(
                crate::canonical::Namespace::Ordinary,
                &fixture.run,
                2,
            )
            .unwrap();
            native_root
                .join("ordinary")
                .join(format!("txn-{transaction}-build"))
                .join(key.file_name())
        }
        _ => unreachable!(),
    }
}
fn native_case_faults(command: &mut std::process::Command, boundary: &str, target: &Path) {
    match boundary {
        "constructor-open" | "open-batch" | "work-events" | "work-ready" => {
            command
                .env("FLIGHTDECK_MUTATION_TARGET", target)
                .env("FLIGHTDECK_MUTATION_KIND", "open");
        }
        "read-table" | "read-index" | "work-ticks" => {
            command.env("FLIGHTDECK_INVENTORY_READ_TARGET", target);
        }
        "map-table" => {
            command.env("FLIGHTDECK_MAPPING_TARGET", target);
        }
        "create" => {
            command
                .env("FLIGHTDECK_MUTATION_TARGET", target)
                .env("FLIGHTDECK_MUTATION_KIND", "create-effect");
        }
        "prefix" | "inner" | "complete" => {
            command.env("FLIGHTDECK_METADATA_WRITE_TARGET", target).env(
                "FLIGHTDECK_METADATA_WRITE_MODE",
                if boundary == "complete" {
                    "complete-inner"
                } else {
                    boundary
                },
            );
        }
        "rename" | "gate" => {
            command.env("FLIGHTDECK_MUTATION_TARGET", target).env(
                "FLIGHTDECK_MUTATION_KIND",
                if boundary == "rename" {
                    "rename"
                } else {
                    "link"
                },
            );
        }
        _ => {
            command
                .env("FLIGHTDECK_PRIVATE_SYNC_TARGET", target)
                .env("FLIGHTDECK_SYNC_EFFECT", "1");
        }
    }
}
fn native_case_observe(
    fixture: &Fixture,
    object: &Path,
    budget: &Arc<crate::resources::ByteBudget>,
    raw: &[(String, Vec<u8>)],
    captured: bool,
    boundary: &str,
    recovery: &str,
) {
    assert_eq!(raw_bytes(&object.join("ready")).as_slice(), raw);
    check_metrics(object, captured, 1, budget);
    let records = fs::read(fixture.base.join("ordinary-fds")).unwrap();
    assert!(!records.is_empty());
    assert_eq!(records.len() % 4, 0);
    let namespace_max = records
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .max()
        .unwrap();
    let raw_max = records
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes([bytes[2], bytes[3]]))
        .max()
        .unwrap();
    assert!(namespace_max <= 8, "{boundary}: {namespace_max}");
    let store = Store::open(&fixture.root).unwrap();
    let frontier = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 3);
    assert_eq!(frontier.event_end(), 3);
    assert!(!frontier.complete());
    let metadata = crate::canonical::CanonicalMetadata::read(
        &mut File::open(object.join("ready/metadata")).unwrap(),
    )
    .unwrap();
    assert_eq!(metadata.loss.inherited.publications, 1);
    assert_eq!(metadata.loss.publications_new, 0);
    assert_eq!(metadata.loss.inherited.events, 0);
    assert_eq!(metadata.loss.events_new, 0);
    assert_eq!(metadata.binding, fixture.binding);
    assert_eq!(metadata.lifecycle, crate::canonical::Lifecycle::Active);
    eprintln!(
        "ORDINARY-NATIVE captured={captured} boundary={boundary} recovery={recovery} \
            namespace_fds={namespace_max} raw_fds={raw_max} exact_raw=true exact_loss=true"
    );
}

fn native_command(
    fixture: &Fixture,
    library: &Path,
    boundary: &str,
    recovery: &str,
) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD, "--nocapture", "--test-threads=1"])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env(
            "FD_ORDINARY_NATIVE_ROOT",
            fixture.root.canonicalize().unwrap(),
        )
        .env("FD_ORDINARY_NATIVE_BOUNDARY", boundary)
        .env("FD_ORDINARY_NATIVE_RECOVERY", recovery)
        .env("FD_ORDINARY_NATIVE_SOURCE", fixture.writer.name());
    command
}
fn native_child(root: &Path) {
    let boundary = std::env::var("FD_ORDINARY_NATIVE_BOUNDARY").unwrap();
    let recovery = std::env::var("FD_ORDINARY_NATIVE_RECOVERY").unwrap();
    let source = std::env::var("FD_ORDINARY_NATIVE_SOURCE").unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let store = Store::open(root).unwrap();
    if recovery == "resume" {
        resume_child(&store, &source, &budget, &ledger);
        return;
    }
    let armed = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    if boundary.starts_with("work-") {
        ordinary_work_tests::child(&store, &source, &budget, &ledger, &armed, &boundary);
        return;
    }
    if boundary == "constructor-open" {
        constructor_fault(&store, &source, &budget, &ledger, &armed);
        return;
    }
    let mut owner = store
        .resume_ordinary_proof(
            RingReader::open_generation(&source).unwrap(),
            &budget,
            &ledger,
        )
        .unwrap();
    reach_fault_phase(&mut owner, &store, root, &boundary);
    native_child_refusal(&mut owner, root, &boundary, &ledger, &armed);
    match recovery.as_str() {
        "retry" => finish_recovery(&mut owner),
        "store" => {
            drop(owner);
            drop(store);
            let store = Store::open(root).unwrap();
            resume_child(&store, &source, &budget, &ledger);
        }
        "process" => {
            drop(owner);
            drop(store);
        }
        _ => unreachable!(),
    }
    eprintln!(
        "ORDINARY-NATIVE-CHILD boundary={boundary} recovery={recovery} \
            work={:?} peak={} footprint={:?}",
        ledger.work(),
        budget.used_peak(),
        crate::resources::Footprint::read().unwrap()
    );
}
fn native_child_refusal(
    owner: &mut OrdinaryRecovery<'_>,
    root: &Path,
    boundary: &str,
    ledger: &PublicationLedger,
    armed: &Path,
) {
    let raw = if boundary == "map-table" {
        Some(native_child_mapping_raw())
    } else {
        None
    };
    fs::write(armed, []).unwrap();
    let occupied = fs::read(root.join("foreground")).unwrap();
    let before = ledger.work();
    let error = failure(owner);
    let expected_error = if boundary == "map-table" { 12 } else { 5 };
    assert_eq!(error.raw_os_error(), Some(expected_error), "{error:?}");
    if boundary == "open-batch" {
        let once = ledger.work();
        assert_eq!(owner.step().unwrap_err().raw_os_error(), Some(5));
        assert!(ledger.work().inventory.open_attempts > once.inventory.open_attempts);
        assert_eq!(fs::read(root.join("foreground")).unwrap(), occupied);
    }
    if let Some(raw) = raw {
        assert_eq!(native_child_mapping_raw(), raw);
        assert_eq!(fs::read(root.join("foreground")).unwrap(), occupied);
        let once = ledger.work();
        assert_eq!(owner.step().unwrap_err().raw_os_error(), Some(12));
        assert!(ledger.work().decode_admission_bytes > once.decode_admission_bytes);
        assert_eq!(native_child_mapping_raw(), raw);
    }
    let failed = ledger.work();
    assert!(failed.proof_operations >= before.proof_operations);
    fs::remove_file(armed).unwrap();
    if boundary == "read-table" {
        assert_eq!(fs::read(root.join("foreground")).unwrap(), occupied);
        assert_eq!(failed.proof_event_next, before.proof_event_next);
        assert!(failed.raw_read_operations > before.raw_read_operations);
    }
}

fn native_child_mapping_raw() -> Vec<(String, Vec<u8>)> {
    let target = PathBuf::from(std::env::var_os("FLIGHTDECK_MAPPING_TARGET").unwrap());
    raw_bytes(target.parent().unwrap())
}

fn resume_child(
    store: &Store,
    source: &str,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
) {
    let root = PrivateDirectory::open(&store.root).unwrap();
    let bytes = fs::read(store.root.join("foreground")).unwrap();
    if matches!(
        bootstrap::codec::decode_slot(
            &bytes,
            store.recording(),
            bootstrap::codec::SlotKind::Foreground
        )
        .unwrap(),
        bootstrap::codec::Slot::Idle
    ) {
        return;
    }
    let state = crate::transaction::normal::NormalState::decode(&bytes, store.recording()).unwrap();
    let base = root.open_directory("data").unwrap();
    let object = base
        .open_directory(&collector::recording::identity_text(state.transaction).to_string())
        .unwrap();
    let (directory, _) = inventory::select_original(&object).unwrap().unwrap();
    let complete = match directory.open_file("metadata") {
        Ok(mut file) => match crate::canonical::CanonicalMetadata::read(&mut file) {
            Ok(_) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
                ) =>
            {
                false
            }
            Err(error) => panic!("metadata read: {error}"),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => panic!("metadata open: {error}"),
    };
    drop(directory);
    drop(object);
    drop(base);
    drop(root);
    if complete {
        // No reader is opened in this branch: durable normal metadata supplies authority.
        let mut owner = store.resume_publication_counted(budget, ledger).unwrap();
        finish_publication(&mut owner);
    } else {
        let mut owner = store
            .resume_ordinary_proof(RingReader::open_generation(source).unwrap(), budget, ledger)
            .unwrap();
        finish_recovery(&mut owner);
    }
}

fn constructor_fault(
    store: &Store,
    source: &str,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
    armed: &Path,
) {
    fs::write(armed, []).unwrap();
    let occupied = fs::read(store.root.join("foreground")).unwrap();
    let before = ledger.work();
    for _ in 0..3 {
        let error = store
            .resume_ordinary_proof(RingReader::open_generation(source).unwrap(), budget, ledger)
            .err()
            .unwrap();
        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(fs::read(store.root.join("foreground")).unwrap(), occupied);
    }
    let failed = ledger.work();
    assert_eq!(
        failed.inventory.open_attempts - before.inventory.open_attempts,
        3
    );
    assert_eq!(failed.source_observations - before.source_observations, 3);
    assert_eq!(failed.inventory.positions, before.inventory.positions);
    fs::remove_file(armed).unwrap();
    eprintln!("ORDINARY-CONSTRUCTOR repeated=3 work={failed:?}");
}

fn reach_fault_phase(owner: &mut OrdinaryRecovery<'_>, store: &Store, root: &Path, boundary: &str) {
    if matches!(boundary, "idle" | "quiet") {
        let expected = if boundary == "idle" {
            bootstrap::codec::Phase::Cleanup
        } else {
            bootstrap::codec::Phase::Published
        };
        let mut reached = false;
        for _ in 0..20_000 {
            owner.step().unwrap();
            let bytes = fs::read(root.join("foreground")).unwrap();
            let state =
                crate::transaction::normal::NormalState::decode(&bytes, store.recording()).unwrap();
            if state.phase == expected {
                reached = true;
                break;
            }
        }
        assert!(reached);
    }
}

#[path = "ordinary_native_cases.rs"]
mod ordinary_native_cases;

#[path = "ordinary_work_tests.rs"]
mod ordinary_work_tests;
