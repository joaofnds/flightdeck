use super::*;
use collector::recording::bootstrap::codec::Phase;

#[test]
fn original_census_process_restart_partial_and_completed() {
    // Goal: selected complete facts, including unwritten absence/inodes, survive actual death.
    // Method: exit a separate owner at exact witness/plan boundaries, then reacquire Store.
    if let Some(root) = std::env::var_os("FLIGHTDECK_CENSUS_ROOT") {
        census_child(Path::new(&root));
        return;
    }
    for captured in [false, true] {
        for boundary in ["partial", "plan"] {
            for alteration in ["unchanged", "replacement", "insertion"] {
                census_process_case(captured, boundary, alteration);
            }
        }
    }
}
fn census_process_case(captured: bool, boundary: &str, alteration: &str) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = row_publication_seed(&mut fixture, &store, &budget);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let original = object.join("pending/events/1023.parquet");
    fs::rename(object.join("pending/events/4095.parquet"), &original).unwrap();
    let expected = fs::read(&original).unwrap();
    private::open_private_file(&fixture.base.join("expected.parquet"), true)
        .unwrap()
        .write_all(&expected)
        .unwrap();
    drop(attempt);
    drop(store);
    let mut first = OwnedChild(census_command(&fixture, boundary, "stop").spawn().unwrap());
    assert!(first.wait().success());
    let census = object.join("original-census");
    if boundary == "partial" {
        assert_eq!(fs::metadata(&census).unwrap().len(), 12544);
    }
    let slot = fixture.slot();
    let old_witness = fs::read(&census).unwrap();
    census_process_alter(&fixture, &object, &original, &expected, alteration);
    let mut second = OwnedChild(
        census_command(&fixture, boundary, alteration)
            .spawn()
            .unwrap(),
    );
    assert!(
        second.wait().success(),
        "{captured} {boundary} {alteration}"
    );
    if alteration == "unchanged" {
        assert!(!census.exists());
        assert!(!object.join("pending").exists());
    } else {
        assert_eq!(fixture.slot(), slot);
        assert_eq!(
            fs::metadata(&census).unwrap().len(),
            collector::u64_from_usize(old_witness.len()),
            "selected witness changed before refusal"
        );
        assert_eq!(fs::read(&census).unwrap(), old_witness);
        assert_eq!(fs::read(original).unwrap(), expected);
        if alteration == "insertion" {
            assert_eq!(
                fs::read(object.join("pending/events/4095.parquet")).unwrap(),
                expected
            );
        }
        assert!(!object.join("fragment-0/metadata").exists());
    }
}
fn census_process_alter(
    fixture: &Fixture,
    object: &Path,
    original: &Path,
    expected: &[u8],
    alteration: &str,
) {
    if alteration != "unchanged" {
        let replacement = fixture.base.join("replacement.parquet");
        private::open_private_file(&replacement, true)
            .unwrap()
            .write_all(expected)
            .unwrap();
        fs::rename(
            replacement,
            if alteration == "insertion" {
                object.join("pending/events/4095.parquet")
            } else {
                original.to_path_buf()
            },
        )
        .unwrap();
    }
}
fn census_command(fixture: &Fixture, boundary: &str, mode: &str) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::row_salvage_tests::",
                "census_tests::original_census_process_restart_partial_and_completed"
            ),
            "--nocapture",
        ])
        .env("FLIGHTDECK_CENSUS_ROOT", &fixture.root)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env("FLIGHTDECK_CENSUS_BOUNDARY", boundary)
        .env("FLIGHTDECK_CENSUS_MODE", mode);
    command
}
fn census_child(root: &Path) {
    let store = Store::open(root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let reader =
        RingReader::open_generation(&std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap())
            .unwrap();
    let captured = reader.dictionary_capacity().is_some();
    let expected = census_child_expected(root, &budget, captured);
    let mode = std::env::var("FLIGHTDECK_CENSUS_MODE").unwrap();
    let boundary = std::env::var("FLIGHTDECK_CENSUS_BOUNDARY").unwrap();
    let mut owner = store
        .resume_row_publication(Some(reader), &budget, &ledger)
        .unwrap()
        .unwrap();
    let object = fs::read_dir(root.join("data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    for _ in 0..20000 {
        match owner.step() {
            Err(error) => {
                assert_ne!(mode, "unchanged");
                assert_ne!(mode, "stop");
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                return;
            }
            Ok(RowPublicationStep::Idle) => {
                assert_eq!(mode, "unchanged");
                let reader = RingReader::open_generation(
                    &std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap(),
                )
                .unwrap();
                row_publication_native_oracle(root, &store, &reader, &budget, &expected);
                return;
            }
            Ok(RowPublicationStep::Unresolved) => panic!("proved ordinary prefix"),
            Ok(RowPublicationStep::Continue) => {}
        }
        if mode == "stop" {
            let state = crate::transaction::normal::NormalState::decode(
                &fs::read(root.join("foreground")).unwrap(),
                store.recording,
            )
            .unwrap();
            let stopped = if boundary == "partial" {
                state.census.kind == 1
                    && fs::metadata(object.join("original-census"))
                        .is_ok_and(|file| file.len() == 12544)
            } else {
                state.phase == Phase::SalvagePlanned
            };
            if stopped {
                eprintln!("census-process-stop {boundary} work={:?}", ledger.work());
                assert!(object.join("pending/events/1023.parquet").exists());
                std::process::exit(0);
            }
        }
    }
    panic!("census finite pass budget");
}

#[test]
fn original_census_metadata_restart_checks_all_originals_before_unlink() {
    // Goal: cold complete-metadata recovery cannot delete an earlier original before late refusal.
    // Method: reach durable gates, alter original identities, then reacquire Store.
    for captured in [false, true] {
        for insertion in [false, true] {
            census_metadata_case(captured, insertion);
        }
    }
}
fn census_metadata_case(captured: bool, insertion: bool) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = row_publication_seed(&mut fixture, &store, &budget);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let path = object.join("pending/events/1023.parquet");
    fs::rename(object.join("pending/events/4095.parquet"), &path).unwrap();
    let bytes = fs::read(&path).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .recover_rows(fixture.reader(), &budget, &ledger)
        .unwrap();
    for _ in 0..20000 {
        owner.step().unwrap();
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if state.phase == Phase::SalvageCommitted {
            break;
        }
    }
    drop(owner);
    drop(store);
    let replacement = fixture.base.join("replacement.parquet");
    private::open_private_file(&replacement, true)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    let target = if insertion {
        object.join("pending/events/4095.parquet")
    } else {
        path.clone()
    };
    fs::rename(replacement, &target).unwrap();
    let slot = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let mut owner = store
        .resume_row_publication(Some(fixture.reader()), &budget, &ledger)
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
            Ok(result) => panic!("substitution accepted: {result:?}"),
        }
    }
    assert!(refused);
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(fs::read(target).unwrap(), bytes);
    assert_eq!(fixture.slot(), slot);
}

fn census_child_expected(
    root: &Path,
    budget: &Arc<ByteBudget>,
    captured: bool,
) -> polars::prelude::DataFrame {
    crate::event_recovery::read::read_batch_file(
        private::open_private_file(&root.parent().unwrap().join("expected.parquet"), false)
            .unwrap(),
        &crate::event_recovery::Batch {
            first: 1,
            end: 3,
            rows: 2,
        },
        captured,
        true,
        budget,
    )
    .unwrap()
}

#[test]
fn original_census_bound_witness_refuses_absence_substitution_and_digest_change() {
    // Goal: complete file identity and the selected whole digest remain independent gates.
    // Method: drop Store after metadata, then remove, replace, link or rebind the witness.
    for captured in [false, true] {
        for alteration in 0..4 {
            census_bound_case(captured, alteration);
        }
    }
}
fn census_bound_case(captured: bool, alteration: u8) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = row_publication_seed(&mut fixture, &store, &budget);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let expected = row_publication_native_expected(&fixture.root, &budget, captured);
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .recover_rows(fixture.reader(), &budget, &ledger)
        .unwrap();
    for _ in 0..10000 {
        owner.step().unwrap();
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if state.row_plan.is_some_and(|plan| plan.rows == [2, 0]) {
            break;
        }
    }
    drop(owner);
    drop(store);
    let path = object.join("original-census");
    census_bound_alter(&fixture, &object, &path, alteration);
    // A selected digest, never refreshed from these changed bytes, must still refuse variant3.
    let slot = fixture.slot();
    let original = fs::read(object.join("pending/events/4095.parquet")).unwrap();
    let store = Store::open(&fixture.root).unwrap();
    let mut owner = store
        .resume_row_publication(Some(fixture.reader()), &budget, &ledger)
        .unwrap()
        .unwrap();
    let mut refused = false;
    for _ in 0..10000 {
        match owner.step() {
            Err(_) => {
                refused = true;
                break;
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(result) => panic!("invalid witness accepted: {result:?}"),
        }
    }
    assert!(refused);
    assert_eq!(fixture.slot(), slot);
    assert_eq!(
        fs::read(object.join("pending/events/4095.parquet")).unwrap(),
        original
    );
    assert!(
        store
            .source_frontier(&fixture.run, &budget, &mut ReadWork::default())
            .unwrap()
            .is_none()
    );
    census_bound_rows(&object, &budget, captured, &expected);
}
fn census_bound_rows(
    object: &Path,
    budget: &Arc<ByteBudget>,
    captured: bool,
    expected: &polars::prelude::DataFrame,
) {
    let actual = crate::event_recovery::read::read_batch_file(
        private::open_private_file(&object.join("fragment-0/0000.parquet"), false).unwrap(),
        &crate::event_recovery::Batch {
            first: 1,
            end: 3,
            rows: 2,
        },
        captured,
        true,
        budget,
    )
    .unwrap();
    assert!(actual.equals_missing(expected));
}
fn census_bound_alter(fixture: &Fixture, object: &Path, path: &Path, alteration: u8) {
    match alteration {
        0 => fs::remove_file(path).unwrap(),
        1 => {
            let replacement = fixture.base.join("witness-copy");
            private::open_private_file(&replacement, true)
                .unwrap()
                .write_all(&fs::read(path).unwrap())
                .unwrap();
            fs::rename(replacement, path).unwrap();
        }
        2 => fs::hard_link(path, fixture.base.join("linked-witness")).unwrap(),
        3 => census_rebind_original(fixture, object),
        _ => panic!("closed fixture variant"),
    }
}
fn census_rebind_original(fixture: &Fixture, object: &Path) {
    use std::io::{Seek, SeekFrom};
    let original = object.join("pending/events/4095.parquet");
    let bytes = fs::read(&original).unwrap();
    let replacement = fixture.base.join("original-copy");
    private::open_private_file(&replacement, true)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    fs::rename(replacement, &original).unwrap();
    let mut identity = [0; 56];
    FileIdentity::read(&private::open_private_file(&original, false).unwrap())
        .unwrap()
        .census_encode(&mut identity)
        .unwrap();
    let mut census = private::open_private_file_writable(&object.join("original-census")).unwrap();
    census.seek(SeekFrom::Start(256 + 4108 * 96 + 8)).unwrap();
    census.write_all(&identity).unwrap();
    let mut state =
        crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
            .unwrap();
    state.census.file = Some(FileIdentity::read(&census).unwrap());
    let mut slot = [0; 4096];
    state.encode(&mut slot).unwrap();
    private::open_private_file_writable(&fixture.root.join("foreground"))
        .unwrap()
        .write_all(&slot)
        .unwrap();
}

#[test]
fn original_census_complete_pending_plan_never_repairs_a_partial_bound_file() {
    // Goal: complete pending kind2 is immutable witness authority even while main still has kind1.
    // Method: retain a Store plan as pending, restore its predecessor and truncate the witness.
    for captured in [false, true] {
        census_pending_bound_case(captured);
    }
}
fn census_pending_bound_case(captured: bool) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = row_publication_seed(&mut fixture, &store, &budget);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .recover_rows(fixture.reader(), &budget, &ledger)
        .unwrap();
    let mut planned = None;
    for _ in 0..10000 {
        owner.step().unwrap();
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if state.phase == Phase::SalvagePlanned {
            planned = Some(state);
            break;
        }
    }
    let planned = planned.unwrap();
    drop(owner);
    drop(store);
    let plan = fixture.slot();
    let bytes = census_pending_write(&fixture, &object, &planned, &plan);
    let original = fs::read(object.join("pending/events/4095.parquet")).unwrap();
    let witness = fs::read(object.join("original-census")).unwrap();
    let store = Store::open(&fixture.root).unwrap();
    let mut owner = store
        .resume_row_publication(Some(fixture.reader()), &budget, &ledger)
        .unwrap()
        .unwrap();
    let error = (0..10000)
        .find_map(|_| owner.step().err())
        .expect("bound partial witness must refuse");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fixture.slot(), bytes);
    assert_eq!(
        fs::read(fixture.root.join("foreground.pending")).unwrap(),
        plan
    );
    assert_eq!(fs::read(object.join("original-census")).unwrap(), witness);
    assert_eq!(
        fs::read(object.join("pending/events/4095.parquet")).unwrap(),
        original
    );
    assert!(!object.join("fragment-0").exists());
}

fn census_pending_write(
    fixture: &Fixture,
    object: &Path,
    planned: &crate::transaction::normal::NormalState,
    plan: &[u8],
) -> [u8; 4096] {
    let mut selected = crate::transaction::normal::NormalState::registered(
        planned.transaction,
        planned.input.clone(),
    );
    selected.census.kind = 1;
    selected.census.digest = planned.census.digest;
    let mut bytes = [0; 4096];
    selected.encode(&mut bytes).unwrap();
    private::open_private_file_writable(&fixture.root.join("foreground"))
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    private::open_private_file(&fixture.root.join("foreground.pending"), true)
        .unwrap()
        .write_all(plan)
        .unwrap();
    private::open_private_file_writable(&object.join("original-census"))
        .unwrap()
        .set_len(12544)
        .unwrap();
    bytes
}

#[test]
fn original_census_header_matches_literal_registered_fields_and_reserved_bytes() {
    // Goal: pin the entire256-byte header independently of its encoder's offset helpers.
    // Method: actual registration and descriptor facts, explicit big-endian literal assembly.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let mut attempt = row_publication_seed(&mut fixture, &store, &budget);
        let object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        let parent = PrivateDirectory::open(&object).unwrap();
        let slot = fixture.slot();
        let transaction = attempt.transaction();
        let input = attempt.input().clone();
        let census = attempt.original_inventory(&budget).unwrap();
        let header = census
            .census_header(parent.device_inode().unwrap())
            .unwrap();
        let literal = census_header_literal(&input, transaction, &parent, &object, &slot);
        let mut actual = [0; 256];
        header.encode(&mut actual).unwrap();
        assert_eq!(actual, literal);
        assert_eq!(
            inventory::census::CensusHeader::decode(
                &literal,
                &input,
                transaction,
                parent.device_inode().unwrap()
            )
            .unwrap(),
            header
        );
        for offset in [0, 8, 12, 16, 20, 24, 28, 148, 212, 216, 220, 248, 255] {
            let mut bad = literal;
            bad[offset] = bad[offset].wrapping_add(1);
            assert!(
                inventory::census::CensusHeader::decode(
                    &bad,
                    &input,
                    transaction,
                    parent.device_inode().unwrap()
                )
                .is_err()
            );
        }
    }
}
fn census_header_literal_inode(path: &Path) -> (u64, u64) {
    PrivateDirectory::open(path)
        .unwrap()
        .device_inode()
        .unwrap()
}

fn census_header_literal(
    input: &AttemptInput,
    transaction: Identity,
    parent: &PrivateDirectory,
    object: &Path,
    slot: &[u8],
) -> [u8; 256] {
    let mut literal = [0; 256];
    literal[..8].copy_from_slice(b"FDCENS02");
    for (offset, value) in [
        (8, 1u32),
        (12, 96),
        (16, 4109),
        (20, u32::from(input.captured)),
        (24, 1),
        (28, 1),
        (144, 1),
        (208, 4095),
        (216, 1),
    ] {
        literal[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    for (offset, identity) in [
        (32, input.run.recording),
        (48, input.run.run),
        (64, input.run.boot),
        (80, transaction),
    ] {
        literal[offset..offset + 8].copy_from_slice(&identity.0[0].to_be_bytes());
        literal[offset + 8..offset + 16].copy_from_slice(&identity.0[1].to_be_bytes());
    }
    for (offset, (device, inode)) in [
        (96, parent.device_inode().unwrap()),
        (112, census_header_literal_inode(&object.join("pending"))),
        (
            128,
            census_header_literal_inode(&object.join("pending/events")),
        ),
    ] {
        literal[offset..offset + 8].copy_from_slice(&device.to_be_bytes());
        literal[offset + 8..offset + 16].copy_from_slice(&inode.to_be_bytes());
    }
    literal[152..200].copy_from_slice(&slot[1352..1400]);
    literal[200..208].copy_from_slice(&input.source.layout.to_be_bytes());
    literal[224..232].copy_from_slice(&input.source.origin_continuous_ns.unwrap().to_be_bytes());
    literal[240..248].copy_from_slice(&1u64.to_be_bytes());
    literal
}

#[test]
fn original_census_missing_recorded_derivative_refuses_replay() {
    // Goal: a selected present file cannot disappear before cleanup authority.
    // Method: remove one derivative after plan selection, then reconstruct the Store owner.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        row_publication_make_derivatives(&fixture);
        let attempt = row_publication_seed(&mut fixture, &store, &budget);
        row_publication_copy_derivatives(&attempt, &fixture);
        let object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        let ledger = PublicationLedger::new(&budget).unwrap();
        let mut candidates = attempt
            .row_candidates(fixture.reader(), &budget, &ledger)
            .unwrap();
        row_candidates_finish(&mut candidates);
        drop(candidates);
        drop(store);
        let slot = fixture.slot();
        let witness = fs::read(object.join("original-census")).unwrap();
        fs::remove_file(object.join(format!(
            "pending/{}.parquet",
            crate::schema::DERIVED_TABLES[0]
        )))
        .unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .resume_row_publication(Some(fixture.reader()), &budget, &ledger)
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
                Ok(result) => panic!("missing original accepted: {result:?}"),
            }
        }
        assert!(refused);
        assert_eq!(fixture.slot(), slot);
        assert_eq!(fs::read(object.join("original-census")).unwrap(), witness);
        assert!(object.join("pending/events/4095.parquet").exists());
        assert!(!object.join("fragment-0/metadata").exists());
    }
}
