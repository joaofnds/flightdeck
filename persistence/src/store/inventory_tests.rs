use super::*;
use crate::resources::ByteBudget;
use crate::{BatchFact, IndexFact, InventoryStep, OriginalInventory, OriginalPlacement};
use std::os::unix::fs::PermissionsExt;

fn budget() -> Arc<ByteBudget> {
    Arc::new(ByteBudget::new(256 * 1024 * 1024))
}
fn original(fixture: &Fixture, transaction: Identity, terminal: bool) -> PathBuf {
    fixture
        .root
        .join(if terminal { "recovery-data" } else { "data" })
        .join(collector::recording::identity_text(transaction).to_string())
        .join("pending")
}
fn enumerate(census: &mut OriginalInventory<'_, '_>) {
    for _ in 0..160 {
        let before = census.work();
        let step = census.step().unwrap();
        let after = census.work();
        assert!(after.positions - before.positions <= 128);
        assert!(after.directory.entries - before.directory.entries <= 128);
        assert!(
            after.positions - before.positions + after.directory.entries - before.directory.entries
                <= 128
        );
        if step == InventoryStep::Enumerated {
            return;
        }
    }
    panic!("bounded original census did not finish");
}
fn frame(number: u64) -> polars::prelude::DataFrame {
    use polars::prelude::*;
    let mut frame = event_frame();
    frame
        .with_column(Column::new("event".into(), [number]))
        .unwrap();
    frame
}
fn remove_transport(fixture: &mut Fixture) {
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;
}

#[test]
fn census_preserves_sparse_high_batch_after_transport_and_index_disappear() {
    // Goal: a missing ordinal or index never hides a valid later original batch.
    // Method: encode ordinal4095 through Store, drop transport, census and compare bytes.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let path = original(&fixture, attempt.transaction(), false).join("events/4095.parquet");
    let mut expected = frame(9000);
    crate::encoder::encode_write(&mut expected, attempt.create_event_batch(4095).unwrap()).unwrap();
    let bytes = fs::read(&path).unwrap();
    let authority = fixture.slot();
    drop(attempt);
    remove_transport(&mut fixture);
    let mut attempt = store.resume_attempt().unwrap();
    let memory = budget();
    let mut census = attempt.original_inventory(&memory).unwrap();
    assert!(!census.enumerated());
    assert_eq!(
        census.reopen_batch(4095).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    enumerate(&mut census);

    assert_eq!(census.index(), IndexFact::Missing);
    assert_eq!(census.batches()[0], BatchFact::Missing);
    assert!(matches!(
        census.batches()[4095],
        BatchFact::Complete {
            first: 9000,
            end: 9001,
            rows: 1,
            ..
        }
    ));
    let mut retained = Vec::new();
    census
        .reopen_batch(4095)
        .unwrap()
        .read_to_end(&mut retained)
        .unwrap();
    assert_eq!(retained, bytes);
    assert_eq!(census.physical_rows(), 1);
    assert_eq!(census.work().positions, 4109);
    eprintln!(
        "sparse-original work={:?} budget_peak={}",
        census.work(),
        memory.used_peak()
    );
    assert_eq!(fixture.slot(), authority);
    drop(census);
    assert_eq!(
        attempt.create_event_batch(4095).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn valid_zero_row_index_retains_its_attempted_interval_without_inventing_rows() {
    // Goal: finite zero-row coverage survives independently of a final ordinary manifest.
    // Method: write the actual index codec through a registered capability and resume.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let index = crate::event_table::index::Index {
        identity: WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        first: 2,
        end: 7,
        rows: 0,
        identities: false,
        batches: vec![],
    };
    index
        .write_file(&mut attempt.create_event_index().unwrap())
        .unwrap();
    drop(attempt);
    remove_transport(&mut fixture);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);

    assert!(matches!(
        census.index(),
        IndexFact::Valid {
            first: 2,
            end: 7,
            rows: 0,
            batches: 0,
            ..
        }
    ));
    assert_eq!(census.registered_event_start(), 1);
    assert_eq!(census.physical_rows(), 0);
    assert!(
        census
            .batches()
            .iter()
            .all(|fact| *fact == BatchFact::Missing)
    );
}

#[test]
fn torn_index_and_torn_low_batch_do_not_erase_a_complete_later_batch() {
    // Goal: definitively incomplete unpublished files remain distinct from missing rows.
    // Method: retain short framing at index/ordinal0 and an encoded ordinal4000.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    attempt
        .create_event_index()
        .unwrap()
        .write_all(b"FDE")
        .unwrap();
    attempt
        .create_event_batch(0)
        .unwrap()
        .write_all(b"PAR1")
        .unwrap();
    crate::encoder::encode_write(&mut frame(40), attempt.create_event_batch(4000).unwrap())
        .unwrap();
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);

    assert!(matches!(census.index(), IndexFact::Malformed { .. }));
    assert!(matches!(census.batches()[0], BatchFact::Malformed { .. }));
    assert!(matches!(
        census.batches()[4000],
        BatchFact::Complete { first: 40, .. }
    ));
    assert_eq!(census.physical_rows(), 1);
}

#[test]
fn census_does_not_advance_past_a_failed_private_admission() {
    // Goal: an I/O/private-admission refusal cannot become an absent or lost batch.
    // Method: change file mode after enumeration, restore it, then retry the same position.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    crate::encoder::encode_write(&mut frame(5), attempt.create_event_batch(0).unwrap()).unwrap();
    let path = original(&fixture, attempt.transaction(), false).join("events/0000.parquet");
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    census.step().unwrap();
    census.step().unwrap();
    assert_eq!(census.next_position(), 0);
    // Position13 is ordinal0; earlier deterministic probes remain committed on error.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(census.step().is_err());
    assert_eq!(census.next_position(), 13);
    assert_eq!(census.batches()[0], BatchFact::Unprobed);
    let attempts = census.work().open_attempts;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    enumerate(&mut census);
    assert!(census.work().open_attempts > attempts);
    assert!(matches!(
        census.batches()[0],
        BatchFact::Complete { first: 5, .. }
    ));
}

#[test]
fn census_reserves_before_work_and_dropping_it_allows_a_fresh_census() {
    // Goal: refusal/restart retains original authority and bytes without append permission.
    // Method: refuse a tiny budget and drop a partial census before rescanning.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    crate::encoder::encode_write(&mut frame(7), attempt.create_event_batch(90).unwrap()).unwrap();
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let slot = fixture.slot();
    assert_eq!(
        attempt
            .original_inventory(&Arc::new(ByteBudget::new(1)))
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::OutOfMemory
    );
    let memory = budget();
    let mut first = attempt.original_inventory(&memory).unwrap();
    assert_eq!(first.step().unwrap(), InventoryStep::Continue);
    drop(first);
    assert_eq!(memory.used(), 0);
    let mut second = attempt.original_inventory(&memory).unwrap();
    enumerate(&mut second);
    assert!(matches!(
        second.batches()[90],
        BatchFact::Complete { first: 7, .. }
    ));
    assert_eq!(fixture.slot(), slot);
}

#[test]
fn ready_inventory_is_explicit_without_certifying_ready_adoption() {
    // Goal: registered phase preserves a visible ready original without claiming completeness.
    // Method: rename a sparse original and census through retained Store ownership.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    crate::encoder::encode_write(&mut frame(30), attempt.create_event_batch(3).unwrap()).unwrap();
    let pending = original(&fixture, attempt.transaction(), false);
    drop(attempt);
    fs::rename(&pending, pending.with_file_name("ready")).unwrap();
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);
    assert_eq!(census.placement(), OriginalPlacement::Ready);
    assert!(matches!(
        census.batches()[3],
        BatchFact::Complete { first: 30, .. }
    ));
    assert!(!pending.exists());
}

#[test]
fn conflicting_original_and_candidate_placements_refuse_without_cleanup() {
    // Goal: the original-only census cannot silently reconcile contradictory/candidate authority.
    // Method: add each registered sibling to a pending original and compare retained bytes.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let pending = original(&fixture, attempt.transaction(), false);
    drop(attempt);
    let slot = fixture.slot();
    for child in ["ready", "fragment-0", "fragment-1", "loss"] {
        let path = pending.with_file_name(child);
        private::create_private_dir(&path).unwrap();
        assert!(store.resume_attempt().is_err());
        assert!(path.is_dir());
        assert!(pending.is_dir());
        assert_eq!(fixture.slot(), slot);
        fs::remove_dir(path).unwrap();
    }
}

#[test]
fn directory_name_validation_yields_before_the_whole_batch_universe() {
    // Goal: name validation itself obeys the pass bound, not only ordinal probing.
    // Method: create300 deterministic torn originals and observe multiple name-only steps.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    for ordinal in 0..300 {
        attempt.create_event_batch(ordinal).unwrap();
    }
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    census.step().unwrap();
    let before = census.work();
    assert_eq!(census.step().unwrap(), InventoryStep::Continue);
    assert_eq!(census.next_position(), 0);
    assert!(census.work().directory.entries - before.directory.entries <= 128);
    enumerate(&mut census);
    assert_eq!(
        census
            .batches()
            .iter()
            .filter(|fact| matches!(fact, BatchFact::Malformed { .. }))
            .count(),
        300
    );
}

#[test]
fn census_refuses_unknown_names_and_private_type_substitutions_without_removal() {
    // Goal: registered filename authority cannot admit arbitrary names/types.
    // Method: leave unknown and symlink/directory substitutions in isolated attempts.
    for (name, kind) in [
        ("alien", 0),
        ("ticks.parquet", 1),
        ("memory.parquet", 2),
        ("events", 1),
    ] {
        let fixture = Fixture::new(false);
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(
                &fixture.input(false),
                &mut fixture.reader(),
                &registration_budget(),
            )
            .unwrap();
        let path = original(&fixture, attempt.transaction(), false).join(name);
        drop(attempt);
        match kind {
            0 => {
                private::open_private_file(&path, true).unwrap();
            }
            1 => std::os::unix::fs::symlink(&fixture.base, &path).unwrap(),
            2 => private::create_private_dir(&path).unwrap(),
            _ => unreachable!(),
        }
        let slot = fixture.slot();
        let mut attempt = store.resume_attempt().unwrap();
        let result = attempt.original_inventory(&budget());
        if let Ok(mut census) = result {
            let mut refused = false;
            for _ in 0..8 {
                if census.step().is_err() {
                    refused = true;
                    break;
                }
            }
            assert!(refused, "{name}");
        }
        assert!(fs::symlink_metadata(&path).is_ok());
        assert_eq!(fixture.slot(), slot);
    }
}

#[test]
fn census_refuses_changed_original_or_reopened_batch_identity() {
    // Goal: retained census facts cannot authorize a substituted source.
    // Method: replace an encoded file after census, then replace the original directory.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    crate::encoder::encode_write(&mut frame(7), attempt.create_event_batch(1).unwrap()).unwrap();
    let pending = original(&fixture, attempt.transaction(), false);
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);
    let batch = pending.join("events/0001.parquet");
    let bytes = fs::read(&batch).unwrap();
    fs::rename(&batch, pending.join("events/0001.saved")).unwrap();
    private::open_private_file(&batch, true)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    assert!(census.reopen_batch(1).is_err());
    fs::rename(&pending, pending.with_file_name("held")).unwrap();
    private::create_private_dir(&pending).unwrap();
    assert!(census.step().is_err());
}

#[test]
fn census_keeps_overlapping_originals_as_distinct_facts() {
    // Goal: duplicate/conflicting intervals cannot be discarded by ordinal preference.
    // Method: encode equal event numbers with differing raw pid values in two ordinals.
    use polars::prelude::*;
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    crate::encoder::encode_write(&mut frame(9), attempt.create_event_batch(0).unwrap()).unwrap();
    let mut conflict = frame(9);
    conflict
        .with_column(Column::new("pid".into(), [99_i32]))
        .unwrap();
    crate::encoder::encode_write(&mut conflict, attempt.create_event_batch(4000).unwrap()).unwrap();
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);
    assert!(census.has_overlaps());
    assert!(census.has_unordered_extents());
    assert_eq!(census.physical_rows(), 2);
    assert!(matches!(
        census.batches()[0],
        BatchFact::Complete { first: 9, .. }
    ));
    assert!(matches!(
        census.batches()[4000],
        BatchFact::Complete { first: 9, .. }
    ));
    let mut first = Vec::new();
    let mut second = Vec::new();
    census
        .reopen_batch(0)
        .unwrap()
        .read_to_end(&mut first)
        .unwrap();
    census
        .reopen_batch(4000)
        .unwrap()
        .read_to_end(&mut second)
        .unwrap();
    assert_ne!(first, second);
}

#[test]
fn terminal_census_preserves_sparse_detached_rows_without_transport() {
    // Goal: terminal inventory is bound to actual acquired completion and direct batch names.
    // Method: publish a real event, register end2, encode ordinal99 and remove transport.
    let mut fixture = Fixture::new(false);
    let memory = collector::machine::Memory::default();
    fixture.writer.publish(&collector::ring::Readings {
        run_tick: 2,
        cpus: &[],
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[collector::tracker::Event::Exec { pid: 41 }],
        dropped: collector::ring::Dropped::default(),
    });
    fixture.writer.finish();
    let mut input = fixture.input(true);
    input.operation = AttemptOperation::Terminal { event_end: 2 };
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    crate::encoder::encode_write(&mut frame(1), attempt.create_event_batch(99).unwrap()).unwrap();
    drop(attempt);
    remove_transport(&mut fixture);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);
    assert!(matches!(
        census.batches()[99],
        BatchFact::Complete {
            first: 1,
            end: 2,
            rows: 1,
            ..
        }
    ));
    assert_eq!(census.index(), IndexFact::Missing);
    assert_eq!(census.registered_event_start(), 1);
}

#[test]
fn malformed_row_schema_profile_and_oversized_batches_remain_retained() {
    // Goal: bounded footer acceptance is followed by actual schema/profile/row validation.
    // Method: encode several invalid frames and ensure none is treated as a complete batch.
    use polars::prelude::*;
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let mut schema = frame(1);
    schema
        .with_column(Column::new("event".into(), ["wrong"]))
        .unwrap();
    crate::encoder::encode_write(&mut schema, attempt.create_event_batch(0).unwrap()).unwrap();
    let mut profile = crate::event_recovery::batch::frame(
        &[crate::event_recovery::batch::Row {
            publication: None,
            event: crate::window::EventRecord {
                number: 1,
                tick: 1,
                pid: 41,
                kind: 1,
                coalition: None,
                usage: None,
                availability: crate::schema::availability::NOT_APPLICABLE,
                noticed: None,
            },
            identity: None,
            name: None,
            app: None,
        }],
        true,
    )
    .unwrap();
    crate::encoder::encode_write(&mut profile, attempt.create_event_batch(1).unwrap()).unwrap();
    let mut oversized = ascending_frame(16_385);
    ParquetWriter::new(attempt.create_event_batch(2).unwrap())
        .with_row_group_size(Some(16_385))
        .finish(&mut oversized)
        .unwrap();
    let mut row = frame(1);
    row.with_column(Column::new("publication".into(), [Some(7_u64)]))
        .unwrap();
    crate::encoder::encode_write(&mut row, attempt.create_event_batch(3).unwrap()).unwrap();
    let pending = original(&fixture, attempt.transaction(), false);
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);
    for ordinal in 0..4 {
        assert!(matches!(
            census.batches()[ordinal],
            BatchFact::Malformed { .. }
        ));
        assert!(
            pending
                .join("events")
                .join(crate::event_recovery::batch_name(ordinal))
                .is_file()
        );
    }
    assert_eq!(census.physical_rows(), 0);
}

#[test]
fn captured_original_keeps_binary_programs_nulls_and_publication_association() {
    // Goal: census validation/reopen preserves full captured rows without association rewriting.
    // Method: encode invalid-UTF8 program bytes and compare both exact file and decoded frames.
    let fixture = Fixture::new_profile(false, true);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let row = captured_row();
    let mut expected = crate::event_recovery::batch::frame(&[row], true).unwrap();
    crate::event_recovery::batch::validate_frame(&expected, true).unwrap();
    crate::encoder::encode_write(&mut expected, attempt.create_event_batch(4001).unwrap()).unwrap();
    let bytes =
        fs::read(original(&fixture, attempt.transaction(), false).join("events/4001.parquet"))
            .unwrap();
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let memory = budget();
    let mut census = attempt.original_inventory(&memory).unwrap();
    enumerate(&mut census);
    assert!(matches!(
        census.batches()[4001],
        BatchFact::Complete {
            first: 8,
            end: 9,
            rows: 1,
            ..
        }
    ));
    let mut retained = Vec::new();
    census
        .reopen_batch(4001)
        .unwrap()
        .read_to_end(&mut retained)
        .unwrap();
    assert_eq!(retained, bytes);
    let actual = crate::event_recovery::read::read_batch_file(
        census.reopen_batch(4001).unwrap(),
        &crate::event_recovery::Batch {
            first: 8,
            end: 9,
            rows: 1,
        },
        true,
        true,
        &memory,
    )
    .unwrap();
    assert!(actual.equals_missing(&expected));
}

#[test]
fn native_payload_read_error_keeps_the_current_position_until_explicit_retry() {
    // Goal: an actual payload EIO cannot advance a deterministic ordinal or prove absence.
    // Method: arm one exact file in an isolated bounded interposed child, then disarm/retry.
    if let Some(root) = std::env::var_os("FLIGHTDECK_INVENTORY_ROOT") {
        native_inventory_child(Path::new(&root));
        return;
    }
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    crate::encoder::encode_write(&mut frame(9), attempt.create_event_batch(0).unwrap()).unwrap();
    let path = original(&fixture, attempt.transaction(), false).join("events/0000.parquet");
    let bytes = fs::read(&path).unwrap();
    let slot = fixture.slot();
    drop(attempt);
    drop(store);
    let library = build_interposer(&fixture);
    let observed = fixture.base.join("read-observed");
    fs::write(&observed, b"").unwrap();
    let mut child = OwnedChild(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                concat!(
                    "store::registration_tests::inventory_tests::",
                    "native_payload_read_error_keeps_the_current_position_until_explicit_retry"
                ),
                "--nocapture",
            ])
            .env("DYLD_INSERT_LIBRARIES", library)
            .env("FLIGHTDECK_INVENTORY_ROOT", &fixture.root)
            .env(
                "FLIGHTDECK_INVENTORY_READ_TARGET",
                path.canonicalize().unwrap(),
            )
            .env(
                "FLIGHTDECK_REGISTRATION_ARMED",
                fixture.base.join("read-armed"),
            )
            .env("FLIGHTDECK_PRIVATE_SYNC_OBSERVED", &observed)
            .spawn()
            .unwrap(),
    );
    assert!(child.wait().success());
    assert_eq!(fs::read(observed).unwrap(), b"x");
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(fixture.slot(), slot);
}

fn build_interposer(fixture: &Fixture) -> PathBuf {
    let library = fixture.base.join("inventory-read.dylib");
    let log_path = fixture.base.join("inventory-native-build.log");
    let log = File::create(&log_path).unwrap();
    let mut child = OwnedChild(
        std::process::Command::new("rustc")
            .args(["--edition=2024", "--crate-type=cdylib", "--deny=warnings"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store/registration_sync_fault.rs"))
            .arg("-o")
            .arg(&library)
            .stdout(std::process::Stdio::from(log.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(log))
            .spawn()
            .unwrap(),
    );
    assert!(
        child.wait().success(),
        "{}",
        fs::read_to_string(log_path).unwrap()
    );
    library
}
fn native_inventory_child(root: &Path) {
    let store = Store::open(root).unwrap();
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    census.step().unwrap();
    census.step().unwrap();
    let armed = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    fs::write(&armed, b"armed").unwrap();
    let before = census.work();
    let error = census.step().unwrap_err();
    assert!(matches!(error, crate::Error::Io(error) if error.raw_os_error() == Some(5)));
    assert_eq!(census.next_position(), 13);
    assert_eq!(census.batches()[0], BatchFact::Unprobed);
    assert!(census.work().open_attempts > before.open_attempts);
    fs::remove_file(armed).unwrap();
    enumerate(&mut census);
    assert!(matches!(
        census.batches()[0],
        BatchFact::Complete {
            first: 9,
            end: 10,
            rows: 1,
            ..
        }
    ));
}

#[test]
fn decoder_budget_refusal_never_classifies_the_current_batch_as_malformed() {
    // Goal: memory refusal is retriable evidence failure, not a statement about original bytes.
    // Method: consume remaining shared budget after enumeration, then release it and retry.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    crate::encoder::encode_write(&mut frame(3), attempt.create_event_batch(0).unwrap()).unwrap();
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let memory = budget();
    let mut census = attempt.original_inventory(&memory).unwrap();
    census.step().unwrap();
    census.step().unwrap();
    let blocker = memory
        .reserve(memory.maximum() - memory.used() - 1)
        .unwrap();
    assert!(matches!(census.step(), Err(crate::Error::Io(error))
        if error.kind() == io::ErrorKind::OutOfMemory));
    assert_eq!(census.next_position(), 13);
    assert_eq!(census.batches()[0], BatchFact::Unprobed);
    drop(blocker);
    enumerate(&mut census);
    assert!(matches!(
        census.batches()[0],
        BatchFact::Complete { first: 3, .. }
    ));
}

#[test]
fn index_disagreement_is_retained_without_erasing_or_relabeling_batches() {
    // Goal: index commitments remain separate from independent sparse-file evidence.
    // Method: write a finite zero-row index and a valid later original beyond its end.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    crate::event_table::index::Index {
        identity: WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        first: 2,
        end: 7,
        rows: 0,
        identities: false,
        batches: vec![],
    }
    .write_file(&mut attempt.create_event_index().unwrap())
    .unwrap();
    crate::encoder::encode_write(&mut frame(90), attempt.create_event_batch(4095).unwrap())
        .unwrap();
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);
    assert!(census.index_conflicts());
    assert!(matches!(
        census.index(),
        IndexFact::Valid {
            first: 2,
            end: 7,
            ..
        }
    ));
    assert_eq!(census.registered_event_start(), 1);
    assert!(matches!(
        census.batches()[4095],
        BatchFact::Complete {
            first: 90,
            end: 91,
            rows: 1,
            ..
        }
    ));
}

#[test]
fn typed_metadata_reopen_refuses_an_in_place_rewrite() {
    // Goal: unchanged directory and inode alone cannot authorize changed original bytes.
    // Method: census opaque metadata, reopen exactly, then rewrite that inode with a new length.
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    attempt
        .create_file("metadata")
        .unwrap()
        .write_all(b"opaque")
        .unwrap();
    let path = original(&fixture, attempt.transaction(), false).join("metadata");
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);
    let mut retained = Vec::new();
    census
        .reopen_file(OriginalFile::Metadata)
        .unwrap()
        .read_to_end(&mut retained)
        .unwrap();
    assert_eq!(retained, b"opaque");
    let inode = fs::metadata(&path).unwrap().ino();
    fs::write(&path, b"changed original").unwrap();
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(
        census
            .reopen_file(OriginalFile::Metadata)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

fn captured_row() -> crate::event_recovery::batch::Row {
    crate::event_recovery::batch::Row {
        publication: Some(1),
        event: crate::window::EventRecord {
            number: 8,
            tick: 1,
            pid: 41,
            kind: crate::schema::event_kind::EXEC,
            coalition: None,
            usage: None,
            availability: crate::schema::availability::NOT_APPLICABLE,
            noticed: None,
        },
        identity: Some(crate::owned_identity::OwnedIdentity {
            pid: 41,
            creation: Some(collector::ring::ProcessCreation {
                unique_id: 991,
                pid_version: 7,
            }),
            availability: crate::schema::identity_availability::MEASURED,
            usage: crate::schema::availability::NOT_APPLICABLE,
            program: None,
        }),
        name: Some(vec![0xff, 0x80]),
        app: None,
    }
}

#[test]
fn unknown_numeric_kind_never_becomes_complete() {
    // Goal: typed but unsupported numeric rows cannot become original authority.
    // Method: change only kind in a valid encoded registered batch and resume census.
    use polars::prelude::*;
    let fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let mut invalid = frame(1);
    invalid
        .with_column(Column::new("kind".into(), [255_u8]))
        .unwrap();
    crate::encoder::encode_write(&mut invalid, attempt.create_event_batch(0).unwrap()).unwrap();
    drop(attempt);
    let mut attempt = store.resume_attempt().unwrap();
    let mut census = attempt.original_inventory(&budget()).unwrap();
    enumerate(&mut census);
    assert!(matches!(census.batches()[0], BatchFact::Malformed { .. }));
}

#[test]
fn canonical_decoder_and_census_share_the_exact_owned_payload_boundary() {
    // Goal: a valid encoded batch at8MiB passes both admission paths; one extra byte fails both.
    // Method: retain identical registered files for canonical's File decoder and census.
    for extra in [0, 1] {
        let fixture = Fixture::new_profile(false, true);
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(
                &fixture.input(false),
                &mut fixture.reader(),
                &registration_budget(),
            )
            .unwrap();
        let mut frame = crate::event_recovery::read::physical_captured_frame(extra);
        crate::event_recovery::batch::validate_frame(&frame, true).unwrap();
        crate::encoder::encode_write(&mut frame, attempt.create_event_batch(0).unwrap()).unwrap();
        let file = PrivateDirectory::open(&original(&fixture, attempt.transaction(), false))
            .unwrap()
            .open_directory("events")
            .unwrap()
            .open_file("0000.parquet")
            .unwrap();
        let memory = budget();
        let decoded = crate::event_recovery::read::read_batch_file(
            file,
            &crate::event_recovery::Batch {
                first: 1,
                end: 16_385,
                rows: 16_384,
            },
            true,
            false,
            &memory,
        );
        assert_eq!(decoded.is_ok(), extra == 0);
        if extra == 1 {
            assert!(matches!(
                decoded,
                Err(crate::Error::Bound("original payload"))
            ));
        }
        drop(attempt);
        let mut attempt = store.resume_attempt().unwrap();
        let mut census = attempt.original_inventory(&memory).unwrap();
        enumerate(&mut census);
        assert_eq!(
            matches!(census.batches()[0], BatchFact::Complete { .. }),
            extra == 0
        );
        if extra == 1 {
            assert!(matches!(census.batches()[0], BatchFact::Malformed { .. }));
        }
    }
}

#[test]
fn native_prepared_handoff_and_inventory_fit_eight_namespace_descriptors() {
    // Goal: query ownership release enforces the descriptor bound with a separate root still held.
    // Method: native fstat interposition samples actual open descriptors, including refusal paths.
    if let Some(root) = std::env::var_os("FLIGHTDECK_FD_ROOT") {
        native_prepared_child(Path::new(&root));
        return;
    }
    let fixture = Fixture::new(false);
    let library = build_interposer(&fixture);
    let log = fixture.base.join("fd-counts");
    fs::write(&log, b"").unwrap();
    let test = concat!(
        "store::registration_tests::inventory_tests::",
        "native_prepared_handoff_and_inventory_fit_eight_namespace_descriptors"
    );
    let mut child = OwnedChild(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("DYLD_INSERT_LIBRARIES", library)
            .env("FLIGHTDECK_FD_ROOT", fixture.root.canonicalize().unwrap())
            .env("FLIGHTDECK_FD_LOG", &log)
            .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
            .spawn()
            .unwrap(),
    );
    assert!(child.wait().success());
    let bytes = fs::read(log).unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(bytes.len() % 4, 0);
    assert!(bytes.len() <= 128 * 1024);
    eprintln!("native descriptor raw samples={bytes:?}");
    let namespace_peak = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .max()
        .unwrap();
    let raw_peak = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes([bytes[2], bytes[3]]))
        .max()
        .unwrap();
    assert!(namespace_peak <= 8, "namespace peak {namespace_peak}");
    assert!(raw_peak > 0);
    eprintln!(
        "native descriptors namespace_peak={namespace_peak} raw_peak={raw_peak} samples={}",
        bytes.len() / 4
    );
}
fn native_prepared_child(root: &Path) {
    let store = Store::open(root).unwrap();
    let directory = PrivateDirectory::open(root).unwrap();
    let mut reader =
        RingReader::open_generation(&std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap())
            .unwrap();
    let run = reader.identity().unwrap();
    let input = AttemptInput {
        run,
        operation: AttemptOperation::Ordinary {
            window: 0,
            attempted_origin: 1,
        },
        captured: false,
        losses: LossTransfer {
            predecessor: Predecessor::Genesis,
            publication_start: 1,
            publication_inherited_end: 1,
            event_start: 1,
            event_inherited_end: 1,
            publications: 0,
            events: 0,
            unknown: None,
            prior_window: None,
        },
    };
    let memory = budget();
    let mut state = [0; 256];
    let mut query =
        crate::canonical::SourceFrontierRead::begin(&directory, run, &mut state, &memory).unwrap();
    for _ in 0..3 {
        query.step().unwrap();
    }
    let mut attempt = store
        .register_attempt_from_frontier(&input, &mut reader, query)
        .unwrap();
    crate::encoder::encode_write(&mut frame(1), attempt.create_event_batch(0).unwrap()).unwrap();
    let batch = root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string())
        .join("pending/events/0000.parquet");
    let mut census = attempt.original_inventory(&memory).unwrap();
    census.step().unwrap();
    census.step().unwrap();
    fs::set_permissions(&batch, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(census.step().is_err());
    fs::set_permissions(&batch, fs::Permissions::from_mode(0o600)).unwrap();
    enumerate(&mut census);
    drop(census.reopen_batch(0).unwrap());
}

fn ascending_frame(rows: usize) -> polars::prelude::DataFrame {
    use polars::prelude::*;
    let mut result = frame(1)
        .take(&IdxCa::from_vec("".into(), vec![0; rows]))
        .unwrap();
    let numbers: Vec<_> = (1..=rows).map(collector::u64_from_usize).collect();
    result
        .with_column(Column::new("event".into(), numbers))
        .unwrap();
    result
}

#[test]
fn physical_row_count_group_count_and_order_have_independent_controls() {
    // Goal: a single failing constraint cannot be confused with another physical property.
    // Method: compare one valid boundary with ascending overflow, extra group and bad order.
    use polars::prelude::*;
    for case in 0..4 {
        let fixture = Fixture::new(false);
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(
                &fixture.input(false),
                &mut fixture.reader(),
                &registration_budget(),
            )
            .unwrap();
        let count = if case == 1 { 16_385 } else { 16_384 };
        let mut frame = ascending_frame(count);
        if case == 3 {
            let mut numbers: Vec<_> = (1..=count).map(collector::u64_from_usize).collect();
            numbers[count - 1] = 1;
            frame
                .with_column(Column::new("event".into(), numbers))
                .unwrap();
        }
        let group = if case == 2 { 8192 } else { count };
        ParquetWriter::new(attempt.create_event_batch(0).unwrap())
            .with_row_group_size(Some(group))
            .finish(&mut frame)
            .unwrap();
        drop(attempt);
        let mut attempt = store.resume_attempt().unwrap();
        let mut census = attempt.original_inventory(&budget()).unwrap();
        enumerate(&mut census);
        assert_eq!(
            matches!(
                census.batches()[0],
                BatchFact::Complete { rows: 16_384, .. }
            ),
            case == 0,
            "case {case}"
        );
        if case != 0 {
            assert!(matches!(census.batches()[0], BatchFact::Malformed { .. }));
        }
    }
}

#[test]
fn both_profiles_validate_producer_numeric_relationships_before_adoption() {
    // Goal: captured identity cannot bypass numeric semantics, and valid producer variants survive.
    // Method: construct real producer rows, then vary one numeric relationship per negative case.
    let events = producer_events();
    for captured in [false, true] {
        for case in 0..15 {
            let fixture = Fixture::new_profile(false, captured);
            let store = Store::open(&fixture.root).unwrap();
            let attempt = store
                .register_attempt(
                    &fixture.input(false),
                    &mut fixture.reader(),
                    &registration_budget(),
                )
                .unwrap();
            let source = &events[if case < 5 {
                case
            } else if case < 9 {
                1
            } else {
                2
            }];
            let mut event = crate::window::EventRecord::copy(&crate::NumberedEvent {
                number: 1,
                tick: 1,
                event: source,
            });
            match case {
                5 => event.kind = 255,
                6 => event.availability = 0,
                7 => event.noticed = Some(0),
                8 | 9 => event.coalition = Some(9),
                10 => event.noticed = None,
                11 => event.noticed = Some(255),
                12 => event.availability = 3,
                13 => event.usage = None,
                14 => event.availability = 1,
                _ => {}
            }
            let identity = captured.then_some(crate::owned_identity::OwnedIdentity {
                pid: 41,
                creation: None,
                availability: crate::schema::identity_availability::UNSUPPORTED,
                usage: event.availability,
                program: None,
            });
            let row = crate::event_recovery::batch::Row {
                publication: None,
                event,
                identity,
                name: None,
                app: None,
            };
            let mut frame = crate::event_recovery::batch::frame(&[row], captured).unwrap();
            crate::encoder::encode_write(&mut frame, attempt.create_event_batch(0).unwrap())
                .unwrap();
            drop(attempt);
            let mut attempt = store.resume_attempt().unwrap();
            let mut census = attempt.original_inventory(&budget()).unwrap();
            enumerate(&mut census);
            assert_eq!(
                matches!(census.batches()[0], BatchFact::Complete { .. }),
                case < 5,
                "profile={captured} case={case}"
            );
        }
    }
}

fn producer_events() -> [collector::tracker::Event; 5] {
    let usage = collector::processes::Usage {
        awake_ns_at_start: 1,
        user_ns: 2,
        system_ns: 3,
        energy_nj: 4,
        instructions: 5,
        super_instructions: 6,
        cycles: 7,
        idle_wakeups: 8,
        interrupt_wakeups: 9,
        pageins: 10,
        resident_bytes: 11,
        footprint_bytes: 12,
        disk_read_bytes: 13,
        disk_written_bytes: 14,
    };
    [
        collector::tracker::Event::Start {
            pid: 41,
            coalition: Some(9),
        },
        collector::tracker::Event::Exec { pid: 41 },
        collector::tracker::Event::Exit {
            pid: 41,
            last: Ok(usage),
            noticed: collector::tracker::Noticed::OnExit,
        },
        collector::tracker::Event::Exit {
            pid: 41,
            last: Err(collector::processes::Unreadable::Refused),
            noticed: collector::tracker::Noticed::AlreadyExited,
        },
        collector::tracker::Event::Exit {
            pid: 41,
            last: Err(collector::processes::Unreadable::Gone),
            noticed: collector::tracker::Noticed::OnExit,
        },
    ]
}
