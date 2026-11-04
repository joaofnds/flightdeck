//! Refusal, retry, closure and reconstruction authority controls.
use super::*;

#[path = "ordinary_leading_gap_tests.rs"]
mod ordinary_leading_gap_tests;
use polars::prelude::*;

#[path = "ordinary_schema_tests.rs"]
mod ordinary_schema_tests;

#[path = "ordinary_order_tests.rs"]
mod ordinary_order_tests;

#[path = "ordinary_column_tests.rs"]
mod ordinary_column_tests;

fn staged(captured: bool, complete: bool) -> (Fixture, PathBuf, Arc<crate::resources::ByteBudget>) {
    let mut fixture = metric_fixture(captured, 1);
    metric_publish(&mut fixture, 0, true, true);
    if complete {
        fixture.writer.finish();
    }
    let budget = registration_budget();
    let object;
    {
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(&complete_input(&fixture), &mut fixture.reader(), &budget)
            .unwrap();
        object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        complete_payload(&attempt, &fixture, 1);
    }
    (fixture, object, budget)
}
fn failure(owner: &mut OrdinaryRecovery<'_>) -> io::Error {
    for _ in 0..20_000 {
        match owner.step() {
            Err(error) => return error,
            Ok(PublicationStep::Continue) => {}
            Ok(PublicationStep::Idle) => panic!("invalid payload published"),
        }
    }
    panic!("refusal did not converge within bounded fixture work");
}
fn rewrite(path: &Path, mutate: impl FnOnce(&mut DataFrame)) {
    let budget = registration_budget();
    let table =
        crate::engine::AdmittedTable::from_file_budgeted(File::open(path).unwrap(), &budget)
            .unwrap();
    let names: Vec<_> = table.fields().map(|(name, _)| name).collect();
    let mut frame = table.read_metadata_budgeted(&names, None).unwrap();
    mutate(&mut frame);
    drop(table);
    crate::encoder::encode_write(&mut frame, File::create(path).unwrap()).unwrap();
}
fn ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse(
    captured: bool,
    mutation: &str,
) {
    let (fixture, object, budget) = staged(captured, true);
    let path = object.join("pending");
    match mutation {
        "omit" => rewrite(&path.join("cpus.parquet"), |frame| {
            *frame = frame.slice(0, frame.height() - 1);
        }),
        "numeric" => rewrite(&path.join("memory.parquet"), |frame| {
            frame
                .with_column(Column::new("total_bytes".into(), [999u64]))
                .unwrap();
        }),
        "native" => rewrite(&path.join("processes.parquet"), |frame| {
            frame
                .with_column(Column::new(
                    "executable_name".into(),
                    vec![b"forged-\xff".as_slice(); frame.height()],
                ))
                .unwrap();
        }),
        _ => unreachable!(),
    }
    let raw = raw_bytes(&path);
    let occupied = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    assert_eq!(
        failure(&mut owner).kind(),
        io::ErrorKind::InvalidData,
        "{mutation}"
    );
    drop(owner);
    assert_eq!(raw_bytes(&path), raw);
    assert_eq!(fixture.slot(), occupied);
    assert!(!path.join("metadata").exists());
    assert!(ledger.work().raw_read_operations > 0);
}
fn ordinary_equal_content_inode_substitution_after_inventory_refuses(captured: bool) {
    let (fixture, object, budget) = staged(captured, true);
    let raw = raw_bytes(&object.join("pending"));
    let occupied = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    for _ in 0..100 {
        owner.step().unwrap();
        if owner.work().inventory.positions == 4109 {
            break;
        }
    }
    assert_eq!(owner.work().inventory.positions, 4109);
    let replacement = fixture.base.join("replacement");
    fs::copy(object.join("pending/memory.parquet"), &replacement).unwrap();
    fs::rename(replacement, object.join("pending/memory.parquet")).unwrap();
    assert_eq!(failure(&mut owner).kind(), io::ErrorKind::InvalidData);
    drop(owner);
    assert_eq!(fixture.slot(), occupied);
    assert_eq!(raw_bytes(&object.join("pending")), raw);
    assert!(!object.join("pending/metadata").exists());
}
fn ordinary_active_boundary_waits_then_same_owner_completes_and_progresses(captured: bool) {
    let (mut fixture, object, budget) = staged(captured, false);
    let raw = raw_bytes(&object.join("pending"));
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    assert_eq!(failure(&mut owner).kind(), io::ErrorKind::WouldBlock);
    assert!(!object.join("pending/metadata").exists());
    publish_at(&mut fixture.writer, 1);
    finish_recovery(&mut owner);
    drop(owner);
    assert_eq!(raw_bytes(&object.join("ready")), raw);
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
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.losses.predecessor = frontier.predecessor().clone();
    input.losses.publication_start = 3;
    input.losses.publication_inherited_end = 3;
    input.losses.event_start = 3;
    input.losses.event_inherited_end = 3;
    input.losses.prior_window = frontier.prior_window();
    input.operation = AttemptOperation::Ordinary {
        window: 1,
        attempted_origin: 3,
    };
    fixture.writer.finish();
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    let manifest = encode_payload(&attempt, &fixture, 3, false);
    assert_eq!(manifest.losses.publications, 0);
    assert_eq!(manifest.losses.events, 0);
    drop(attempt);
    drop(store);
    let store = Store::open(&fixture.root).unwrap();
    let mut second = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    finish_recovery(&mut second);
    drop(second);
    let frontier = store
        .source_frontier(
            &fixture.run,
            &budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 4);
    assert_eq!(frontier.event_end(), 3);
    assert_eq!(frontier.prior_window(), Some(1));
    assert!(!frontier.complete());
    assert_eq!(raw_bytes(&object.join("ready")), raw);
}
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(
    captured: bool,
    length: usize,
) {
    let (fixture, object, budget) = staged(captured, true);
    {
        let store = Store::open(&fixture.root).unwrap();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let mut owner = store
            .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
            .unwrap();
        for _ in 0..20_000 {
            owner.step().unwrap();
            if object.join("pending/metadata").exists() {
                break;
            }
        }
    }
    let path = object.join("pending/metadata");
    let full = fs::read(&path).unwrap();
    let mut altered = full.clone();
    if length == usize::MAX {
        altered[100] ^= 1;
    } else {
        altered.truncate(length);
    }
    fs::write(&path, &altered).unwrap();
    let raw = raw_bytes(&object.join("pending"));
    let occupied = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    if length == usize::MAX {
        assert_eq!(failure(&mut owner).kind(), io::ErrorKind::InvalidData);
        drop(owner);
        assert_eq!(fs::read(path).unwrap(), altered);
        assert_eq!(fixture.slot(), occupied);
        assert_eq!(raw_bytes(&object.join("pending")), raw);
    } else {
        finish_recovery(&mut owner);
        drop(owner);
        assert_eq!(fs::read(object.join("ready/metadata")).unwrap(), full);
        assert_eq!(raw_bytes(&object.join("ready")), raw);
    }
}
fn ordinary_shared_budget_refusal_and_source_substitution_keep_authority(captured: bool) {
    let (fixture, object, budget) = staged(captured, true);
    let raw = raw_bytes(&object.join("pending"));
    let occupied = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let held = budget
        .reserve(budget.maximum() - budget.used() - 200_000)
        .unwrap();
    let before = ledger.work();
    let error = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    assert!(ledger.work().inventory.open_attempts > before.inventory.open_attempts);
    drop(held);
    let other = Fixture::new_profile(false, captured);
    assert_eq!(
        store
            .resume_ordinary_proof(other.reader(), &budget, &ledger)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fixture.slot(), occupied);
    assert_eq!(raw_bytes(&object.join("pending")), raw);
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    finish_recovery(&mut owner);
}

#[path = "ordinary_native_tests.rs"]
mod ordinary_native_tests;

#[test]
fn ordinary_exhausted_budget_refuses_before_control_reads_numeric() {
    // Goal: refused workspace admission performs no unreserved control read.
    // Method: exhaust the shared allowance before opening numeric reconstruction authority.
    exhausted_budget_refuses_before_control_reads(false);
}

#[test]
fn ordinary_exhausted_budget_refuses_before_control_reads_captured() {
    // Goal: captured reconstruction preserves occupied authority when its allowance is exhausted.
    // Method: refuse construction before control I/O, then release the pressure and recover.
    exhausted_budget_refuses_before_control_reads(true);
}

fn exhausted_budget_refuses_before_control_reads(captured: bool) {
    let (fixture, object, budget) = staged(captured, true);
    let raw = raw_bytes(&object.join("pending"));
    let occupied = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let held = budget.reserve(budget.maximum() - budget.used()).unwrap();
    let before = ledger.work();

    let error = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .err()
        .unwrap();

    assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    assert_eq!(
        ledger.work().control_read_operations,
        before.control_read_operations
    );
    assert_eq!(fixture.slot(), occupied);
    assert_eq!(raw_bytes(&object.join("pending")), raw);

    drop(held);
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    finish_recovery(&mut owner);
    drop(owner);
    assert_eq!(raw_bytes(&object.join("ready")), raw);
}

fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(
    captured: bool,
    mutation: &str,
) {
    let (fixture, object, budget) = staged(captured, true);
    let pending = object.join("pending");
    match mutation {
        "missing" => fs::remove_file(pending.join("memory.parquet")).unwrap(),
        "torn" => fs::write(pending.join("memory.parquet"), b"PAR1torn").unwrap(),
        "extra" => fs::write(pending.join("extra.parquet"), b"unexpected").unwrap(),
        "clock" => rewrite(&pending.join("ticks.parquet"), |frame| {
            frame
                .with_column(Column::new("wall_ns".into(), [999u64]))
                .unwrap();
        }),
        "half-group" => rewrite(&pending.join("ticks.parquet"), |frame| {
            frame.drop_in_place("gpu_celsius").unwrap();
        }),
        _ => unreachable!(),
    }
    let before = fixture.slot();
    let names: Vec<_> = ["memory.parquet", "ticks.parquet", "extra.parquet"]
        .into_iter()
        .map(|name| {
            let bytes = match fs::read(pending.join(name)) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => panic!("snapshot: {error}"),
            };
            (name, bytes)
        })
        .collect();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    let error = failure(&mut owner);
    if matches!(mutation, "missing" | "torn") {
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    } else {
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
    drop(owner);
    assert_eq!(fixture.slot(), before);
    assert!(!pending.join("metadata").exists());
    for (name, expected) in names {
        match expected {
            Some(bytes) => assert_eq!(fs::read(pending.join(name)).unwrap(), bytes),
            None => assert!(!pending.join(name).exists()),
        }
    }
}
fn ordinary_missing_source_before_proof_does_not_become_loss(captured: bool) {
    let (mut fixture, object, budget) = staged(captured, true);
    let raw = raw_bytes(&object.join("pending"));
    let before = fixture.slot();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;
    assert_eq!(
        RingReader::open_generation(fixture.writer.name())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::NotFound
    );
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store.resume_publication_counted(&budget, &ledger).unwrap();
    let mut missing = false;
    for _ in 0..100 {
        match owner.step() {
            Ok(PublicationStep::Continue) => {}
            Ok(PublicationStep::Idle) => panic!("missing metadata published without source"),
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::NotFound);
                missing = true;
                break;
            }
        }
    }
    assert!(missing);
    drop(owner);
    assert_eq!(fixture.slot(), before);
    assert_eq!(raw_bytes(&object.join("pending")), raw);
    assert!(!object.join("pending/metadata").exists());
}

fn ordinary_reconstruction_never_undoes_selected_salvage_census(captured: bool, selected: u32) {
    let (fixture, object, budget) = staged(captured, true);
    let raw = raw_bytes(&object.join("pending"));
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut row = store
        .resume_row_publication(Some(fixture.reader()), &budget, &ledger)
        .unwrap()
        .unwrap();
    let mut reached = false;
    for _ in 0..1000 {
        assert_eq!(row.step().unwrap(), RowPublicationStep::Continue);
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if state.census.kind == selected {
            reached = true;
            break;
        }
    }
    assert!(reached);
    drop(row);
    let before = fixture.slot();
    assert_eq!(
        store
            .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        store
            .resume_publication_counted(&budget, &ledger)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(fixture.slot(), before);
    assert_eq!(raw_bytes(&object.join("pending")), raw);
}

fn ordinary_actual_header_change_refuses_then_same_owner_retries(captured: bool, position: usize) {
    let (fixture, object, budget) = staged(captured, true);
    let mut header = WritableHeader::open(&fixture);
    let raw = raw_bytes(&object.join("pending"));
    let before = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    let previous = header.word(position).fetch_add(1, Ordering::Release);
    assert_eq!(failure(&mut owner).kind(), io::ErrorKind::InvalidData);
    assert_eq!(fixture.slot(), before);
    assert!(!object.join("pending/metadata").exists());
    assert_eq!(raw_bytes(&object.join("pending")), raw);
    header.word(position).store(previous, Ordering::Release);
    finish_recovery(&mut owner);
    drop(owner);
    assert_eq!(raw_bytes(&object.join("ready")), raw);
}

#[path = "ordinary_refusal_cases.rs"]
mod ordinary_refusal_cases;
