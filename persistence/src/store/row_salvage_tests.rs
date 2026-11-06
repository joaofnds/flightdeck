use super::*;
use crate::canonical::{Namespace, ReadWork};
use crate::resources::ByteBudget;

#[test]
fn registered_survivors_require_a_durable_row_bearing_disposition() {
    // Goal: recovery preserves real original/source overlap after ordinary output interruption.
    // Method: omit metadata/index, retain sparse encoded originals, wrap the same producer.
    registered_survivors(false);
}
#[test]
fn captured_registered_survivors_require_a_durable_row_bearing_disposition() {
    // Goal: preserve native captured bytes through the same interrupted Store path.
    // Method: use the shared real producer/encoder fixture with captured identity enabled.
    registered_survivors(true);
}
#[test]
fn a_fragment_of_saved_rows_is_listed_as_an_event_object() {
    // Goal: rows saved apart from a minute are found like a terminal's, and typed so that a
    // fragment is never taken for a minute's completion (P02.1).
    // Method: publish a fragment through the interrupted Store path and list the event objects.
    let fixture = registered_survivors(false);
    let budget = registration_budget();
    let catalog = crate::canonical::catalog::Catalog::open(&fixture.root, &budget).unwrap();

    let terminals = catalog
        .event_objects_page(None, 8, &mut ReadWork::default())
        .unwrap();
    let page = catalog
        .event_objects_page(terminals.next.as_ref(), 8, &mut ReadWork::default())
        .unwrap();

    assert_eq!(terminals.objects.len(), 0);
    assert_eq!(page.objects.len(), 1);
    assert_eq!(
        page.objects[0].kind,
        crate::canonical::catalog::EventObjectKind::Fragment
    );
    assert_eq!(page.objects[0].recovery.rows, 2);
    assert!(
        crate::retention::order_entry_names(&fixture.root)
            .iter()
            .any(|name| name.contains("-fragment-"))
    );
}
fn registered_survivors(captured: bool) -> Fixture {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    if captured {
        publish_captured_pair_at(&mut fixture, 2, 2);
    } else {
        publish_numeric_pair_at(&mut fixture, 2, 2);
    }
    let rows = copy_detached_rows(&fixture);
    let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    let binding = fixture.reader().object_binding().unwrap();
    publish_at(&mut fixture.writer, 1);
    if captured {
        publish_captured_pair_at(&mut fixture, 61, 61_000_000_001);
    } else {
        publish_numeric_pair_at(&mut fixture, 61, 61_000_000_001);
    }
    assert_eq!(fixture.reader().object_binding().unwrap(), binding);
    let mut reader = fixture.reader();
    reader.seek_event(1).unwrap();
    assert!(matches!(
        reader.read_event().unwrap(),
        collector::ring::FoundEvent::Event { number: 1, .. }
    ));
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut publication = attempt
        .recover_rows(fixture.reader(), &budget, &ledger)
        .unwrap();
    let mut idle = false;
    for _ in 0..10000 {
        match publication.step() {
            Ok(RowPublicationStep::Idle) => {
                idle = true;
                break;
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(RowPublicationStep::Unresolved) => panic!("proved ordinary prefix unresolved"),
            Err(error) => panic!("row-bearing Store recovery refused captured={captured}: {error}"),
        }
    }
    assert!(idle);
    assert_eq!(ledger.work().cleanup_removals, 4);
    registered_survivors_oracle(&fixture, &budget, &rows, captured);
    fixture
}
fn registered_survivors_oracle(
    fixture: &Fixture,
    budget: &Arc<ByteBudget>,
    rows: &[crate::event_recovery::batch::Row],
    captured: bool,
) {
    let object = fixture
        .root
        .join("data")
        .read_dir()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(!object.join("pending").exists());
    let mut file = private::open_private_file(&object.join("fragment-0/metadata"), false).unwrap();
    let metadata = crate::canonical::CanonicalMetadata::read(&mut file).unwrap();
    assert_eq!(metadata.loss.events_new, 0);
    assert_eq!(metadata.loss.publications_new, 3);
    let crate::canonical::metadata::Body::Events(manifest) = metadata.body else {
        panic!("fragment");
    };
    let actual = crate::event_recovery::read::read_batch_file(
        private::open_private_file(&object.join("fragment-0/0000.parquet"), false).unwrap(),
        &manifest.batches[0],
        captured,
        true,
        budget,
    )
    .unwrap();
    assert!(actual.equals_missing(&crate::event_recovery::batch::frame(rows, captured).unwrap()));
    let canonical = row_publication_canonical(fixture, budget);
    assert!(
        canonical.equals_missing(&crate::event_recovery::batch::frame(rows, captured).unwrap())
    );
}

#[test]
fn row_merge_sparse_union_profiles_and_association() {
    // Goal: preserve sparse originals, native equality and publication association.
    // Method: encode sparse ordinals and compare all emitted columns in both profiles.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let input = fixture.input(false);
        let attempt = store
            .register_attempt(&input, &mut fixture.reader(), &budget)
            .unwrap();
        if captured {
            publish_captured_events_at(&mut fixture, 2, 2, 4);
        } else {
            publish_numeric_events_at(&mut fixture, 2, 2, 4);
        }
        let mut rows = row_merge_read_four(&fixture, captured);
        rows[1].publication = Some(1);
        let expected = crate::event_recovery::batch::frame(&rows, captured).unwrap();
        for (position, ordinal) in [1023, 4095].into_iter().enumerate() {
            let mut frame = crate::event_recovery::batch::frame(
                &rows[position * 2 + 1..=position * 2 + 1],
                captured,
            )
            .unwrap();
            crate::encoder::encode_write(&mut frame, attempt.create_event_batch(ordinal).unwrap())
                .unwrap();
        }
        let slot_before = fixture.slot();
        let object = fs::read_dir(fixture.root.join("data"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let names_before: Vec<_> = fs::read_dir(&object)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let originals = row_merge_originals(&object);
        let ledger = PublicationLedger::new(&budget).unwrap();
        let mut merge =
            row_merge::RowMerge::new(attempt, fixture.reader(), &budget, &ledger).unwrap();
        let output = row_merge_finish(&mut merge, captured);
        assert_eq!(fixture.slot(), slot_before);
        let names_after: Vec<_> = fs::read_dir(&object)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names_after, names_before);
        for (path, bytes) in originals {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert!(output.equals_missing(&expected));
        assert_eq!(merge.losses, [0, 0]);
        assert_eq!(merge.unresolved, 0);
        assert!(ledger.work().inventory.positions >= 4096);
        assert_eq!(ledger.work().controls, 0);
        assert_eq!(ledger.work().canonical.creates, 0);
        assert_eq!(ledger.work().cleanup_removals, 0);
    }
}

#[test]
fn row_merge_conflicts_and_failed_reopen_do_not_advance() {
    // Goal: duplicate conflicts and changed original files retain committed position.
    // Method: change numeric/native copies or replace a censused file before its admission.
    for mode in 0..3 {
        let captured = mode == 1;
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let input = fixture.input(false);
        let attempt = store
            .register_attempt(&input, &mut fixture.reader(), &budget)
            .unwrap();
        if captured {
            publish_captured_pair_at(&mut fixture, 2, 2);
        } else {
            publish_numeric_pair_at(&mut fixture, 2, 2);
        }
        let mut rows = copy_detached_rows(&fixture);
        if mode == 0 {
            rows[0].event.tick += 1;
        }
        if mode == 1 {
            rows[0].name.as_mut().unwrap()[0] ^= 1;
        }
        let file = attempt.create_event_batch(4095).unwrap();
        let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
        crate::encoder::encode_write(&mut frame, file).unwrap();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let mut merge =
            row_merge::RowMerge::new(attempt, fixture.reader(), &budget, &ledger).unwrap();
        for _ in 0..1000 {
            merge.step().unwrap();
            if merge.range().is_some() {
                break;
            }
        }
        assert!(merge.range().is_some());
        let position = merge.position();
        if mode == 2 {
            // Only this isolated fixture's positively identified original is changed.
            let path = fixture.root.join("data");
            let object = fs::read_dir(path).unwrap().next().unwrap().unwrap().path();
            let batch = object.join("pending/events/4095.parquet");
            assert!(batch.exists(), "{}", batch.display());
            fs::write(batch, b"failed read fixture").unwrap();
        }
        let mut refused = false;
        for _ in 0..5000 {
            if merge.step().is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused);
        assert_eq!(merge.position(), position);
        assert_eq!(merge.losses, [0, 0]);
        assert!(merge.take_batch().is_none());
        assert_eq!(ledger.work().controls, 0);
    }
}

#[test]
fn row_merge_rescue_frozen_append_and_budget() {
    // Goal: reconcile overwritten positions against originals and exclude later appends.
    // Method: retain event1, overwrite the same object, freeze through event4, then append again.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let input = fixture.input(false);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    publish_numeric_pair_at(&mut fixture, 2, 2);
    let rows = copy_detached_rows(&fixture);
    let mut frame = crate::event_recovery::batch::frame(&rows[..1], false).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(1023).unwrap()).unwrap();
    publish_numeric_pair_at(&mut fixture, 3, 3);
    let last = row_merge_rescue_read_last(&fixture);
    let mut frame = crate::event_recovery::batch::frame(&[last], false).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    publish_numeric_pair_at(&mut fixture, 4, 4);
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut merge = row_merge::RowMerge::new(attempt, fixture.reader(), &budget, &ledger).unwrap();
    for _ in 0..1000 {
        merge.step().unwrap();
        if merge.range().is_some() {
            break;
        }
    }
    assert_eq!(merge.range(), Some(1..5));
    assert_eq!(merge.source_range(), Some(3..5));
    publish_numeric_events_at(&mut fixture, 5, 5, 1);
    let mut numbers = Vec::new();
    for _ in 0..10000 {
        match merge.step().unwrap() {
            row_merge::MergeStep::Continue => {}
            row_merge::MergeStep::Batch => {
                let output = merge.take_batch().unwrap();
                numbers.extend(output.rows().iter().map(|row| row.event.number));
            }
            row_merge::MergeStep::Finished => break,
        }
    }
    assert_eq!(numbers, [1, 4]);
    assert_eq!(merge.losses.iter().sum::<u64>(), 2);
    assert_eq!(merge.unresolved, 0);
    drop(merge);
    // A failed construction consumes no additional shared reservation or namespace mutation.
    let tiny = Arc::new(ByteBudget::new(1024));
    let tiny_ledger = PublicationLedger::new(&tiny).unwrap();
    let resumed = store.resume_attempt().unwrap();
    assert!(row_merge::RowMerge::new(resumed, fixture.reader(), &tiny, &tiny_ledger).is_err());
    assert_eq!(tiny.used(), 1024);
}

#[test]
fn row_merge_bound_source_changes_retain_position_and_slot() {
    // Goal: immutable source changes and replacement cannot manufacture absence evidence.
    // Method: mutate an owned source header after freezing, restore it, and try another object.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 4);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        if captured {
            publish_captured_pair_at(&mut fixture, 2, 2);
        } else {
            publish_numeric_pair_at(&mut fixture, 2, 2);
        }
        let mut header = WritableHeader::open(&fixture);
        fixture.binding = fixture.reader().object_binding().unwrap();
        let input = fixture.input(false);
        let attempt = store
            .register_attempt(&input, &mut fixture.reader(), &budget)
            .unwrap();
        let rows = copy_detached_rows(&fixture);
        let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
        crate::encoder::encode_write(&mut frame, attempt.create_event_batch(0).unwrap()).unwrap();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let slot = fixture.slot();
        let mut merge =
            row_merge::RowMerge::new(attempt, fixture.reader(), &budget, &ledger).unwrap();
        for _ in 0..1000 {
            merge.step().unwrap();
            if merge.range().is_some() {
                break;
            }
        }
        let position = merge.position();
        let previous = header.word(6).fetch_add(1, Ordering::Release);
        let result = merge.step();
        header.word(6).store(previous, Ordering::Release);
        assert!(result.is_err());
        assert_eq!(merge.position(), position);
        assert_eq!(fixture.slot(), slot);
        drop(merge);
        let other = proof_fixture(captured, 4);
        let attempt = store.resume_attempt().unwrap();
        assert!(row_merge::RowMerge::new(attempt, other.reader(), &budget, &ledger).is_err());
        assert_eq!(fixture.slot(), slot);
    }
}

fn row_merge_read_four(
    fixture: &Fixture,
    captured: bool,
) -> Vec<crate::event_recovery::batch::Row> {
    let mut reader = fixture.reader();
    reader.seek_event(1).unwrap();
    let mut rows = Vec::with_capacity(4);
    for _ in 0..4 {
        let mut identity = collector::ring::IdentityObservation::EMPTY;
        let found = if captured {
            reader.read_event_identity(&mut identity)
        } else {
            reader.read_event()
        }
        .unwrap();
        let collector::ring::FoundEvent::Event {
            number,
            tick,
            event,
        } = found
        else {
            panic!("event");
        };
        rows.push(
            crate::event_recovery::batch::copy_row(
                &reader,
                &crate::NumberedEvent {
                    number,
                    tick,
                    event: &event,
                },
                captured.then_some(&identity),
            )
            .unwrap(),
        );
    }
    rows
}

#[test]
fn row_merge_owned_adapter_retains_measured_numeric_fields() {
    // Goal: the bounded adapter preserves non-null usage as well as nullable columns.
    // Method: round-trip a validated production frame with distinct measured counters.
    let mut fixture = proof_fixture(false, 1);
    publish_numeric_pair_at(&mut fixture, 2, 2);
    let mut rows = copy_detached_rows(&fixture);
    rows[0].event.kind = crate::schema::event_kind::EXIT;
    rows[0].event.availability = crate::schema::availability::MEASURED;
    rows[0].event.noticed = Some(crate::schema::noticed::ON_EXIT);
    rows[0].event.usage = Some(collector::processes::Usage {
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
    });
    rows[0].publication = Some(1);
    let frame = crate::event_recovery::batch::frame(&rows, false).unwrap();
    crate::event_recovery::batch::validate_frame(&frame, false).unwrap();
    let owned: Vec<_> = (0..2)
        .map(|position| crate::event_recovery::batch::owned_row(&frame, position, false).unwrap())
        .collect();
    let copied = crate::event_recovery::batch::frame(&owned, false).unwrap();
    assert!(frame.equals_missing(&copied));
}

fn row_merge_finish(
    merge: &mut row_merge::RowMerge<'_>,
    captured: bool,
) -> polars::prelude::DataFrame {
    let mut output = None;
    let mut finished = false;
    for _ in 0..10000 {
        match merge.step().unwrap() {
            row_merge::MergeStep::Continue => {}
            row_merge::MergeStep::Batch => {
                let batch = merge.take_batch().unwrap();
                assert!(output.is_none());
                output = Some(crate::event_recovery::batch::frame(batch.rows(), captured).unwrap());
            }
            row_merge::MergeStep::Finished => {
                finished = true;
                break;
            }
        }
    }
    assert!(finished);
    output.unwrap()
}

fn row_merge_originals(object: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    [1023, 4095]
        .iter()
        .map(|ordinal| {
            let path = object
                .join("pending/events")
                .join(crate::event_recovery::batch_name(*ordinal));
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

#[test]
fn row_candidates_durable_prefix_reopens_in_both_profiles() {
    // Goal: real Store candidate authority survives reopen without publishing or deleting.
    // Method: encode sparse originals, prepare candidates, wrap the same source and replay.
    for captured in [false, true] {
        row_candidates_profile(captured);
    }
}
fn row_candidates_profile(captured: bool) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let input = fixture.input(false);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    if captured {
        publish_captured_events_at(&mut fixture, 2, 2, 4);
    } else {
        publish_numeric_events_at(&mut fixture, 2, 2, 4);
    }
    let mut rows = row_merge_read_four(&fixture, captured);
    rows[1].publication = Some(1);
    for (position, ordinal) in [(1, 1023), (3, 4095)] {
        let mut frame =
            crate::event_recovery::batch::frame(&rows[position..=position], captured).unwrap();
        crate::encoder::encode_write(&mut frame, attempt.create_event_batch(ordinal).unwrap())
            .unwrap();
    }
    let object = fs::read_dir(fixture.root.join("data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let originals = row_merge_originals(&object);
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut candidates = attempt
        .row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    row_candidates_finish(&mut candidates);
    assert_eq!(candidates.rows(), [4, 0]);
    let plan = candidates.plan().unwrap();
    let before = fs::read(object.join("fragment-0/0000.parquet")).unwrap();
    assert!(!object.join("fragment-1").exists());
    assert!(!object.join("fragment-0/metadata").exists());
    drop(candidates);
    publish_at(&mut fixture.writer, 3);
    if captured {
        publish_captured_events_at(&mut fixture, 3, 3, 4);
    } else {
        publish_numeric_events_at(&mut fixture, 3, 3, 4);
    }
    let mut replay = store
        .resume_row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    row_candidates_finish(&mut replay);
    assert_eq!(replay.plan(), Some(plan));
    assert_eq!(replay.rows(), [4, 0]);
    assert_eq!(
        fs::read(object.join("fragment-0/0000.parquet")).unwrap(),
        before
    );
    row_candidates_check_union(&object, &rows, captured, &budget);
    for (path, bytes) in originals {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(ledger.work().links, 0);
    assert_eq!(ledger.work().cleanup_removals, 0);
    drop(replay);
    let slot = fixture.slot();
    let loss = store.resume_loss_publication_with_ledger(&budget, &ledger);
    assert!(matches!(loss,Err(error) if error.kind() == io::ErrorKind::Unsupported));
    assert_eq!(fixture.slot(), slot);
}
fn row_candidates_finish(candidates: &mut row_candidates::RowCandidates<'_>) {
    let mut prepared = false;
    for _ in 0..20000 {
        match candidates.step().unwrap() {
            row_candidates::CandidateStep::Unresolved => {
                panic!("ordinary candidate unexpectedly unresolved")
            }
            row_candidates::CandidateStep::Continue => {}
            row_candidates::CandidateStep::Prepared => {
                prepared = true;
                break;
            }
        }
    }
    assert!(prepared);
}

#[test]
fn row_candidates_native_effects_retry_and_restart() {
    // Goal: plan, creation and batch durability effects survive reported EIO.
    // Method: isolated native interposition, followed by same-owner retry and Store reopen.
    if let Some(root) = std::env::var_os("FLIGHTDECK_CANDIDATE_ROOT") {
        row_candidates_native_child(Path::new(&root));
        return;
    }
    let compiler = Fixture::new(false);
    let library = native_registration_build(&compiler);
    for boundary in ["plan", "creation", "file", "directory"] {
        for reopen in [false, true] {
            row_candidates_native_case(&library, boundary, reopen);
        }
    }
}
fn row_candidates_native_case(library: &Path, boundary: &str, reopen: bool) {
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    publish_numeric_pair_at(&mut fixture, 2, 2);
    let rows = copy_detached_rows(&fixture);
    let mut frame = crate::event_recovery::batch::frame(&rows, false).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    let transaction = collector::recording::identity_text(attempt.transaction()).to_string();
    let original = fixture
        .root
        .join("data")
        .join(&transaction)
        .join("pending/events/4095.parquet");
    let bytes = fs::read(&original).unwrap();
    drop(attempt);
    drop(store);
    let root = fixture.root.canonicalize().unwrap();
    let candidate = root.join("data").join(transaction).join("fragment-0");
    let mut command =
        row_candidates_native_command(&fixture, library, boundary, reopen, &root, &candidate);
    let mut child = OwnedChild(command.spawn().unwrap());
    assert!(child.wait().success(), "{boundary} reopen={reopen}");
    assert_eq!(fs::read(original).unwrap(), bytes);
    row_candidates_native_observed(&fixture, boundary, reopen);
    assert!(candidate.join("0000.parquet").exists());
    assert!(!candidate.join("metadata").exists());
}
fn row_candidates_native_child(root: &Path) {
    let store = Store::open(root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let ring = std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap();
    let arm = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    let attempt = store.resume_attempt().unwrap();
    let mut candidates = attempt
        .row_candidates(
            RingReader::open_generation(&ring).unwrap(),
            &budget,
            &ledger,
        )
        .unwrap();
    fs::write(&arm, b"owned fault").unwrap();
    let mut failed = false;
    for _ in 0..20000 {
        match candidates.step() {
            Ok(row_candidates::CandidateStep::Unresolved) => {
                panic!("ordinary candidate unexpectedly unresolved")
            }
            Ok(row_candidates::CandidateStep::Continue) => {}
            Ok(row_candidates::CandidateStep::Prepared) => panic!("missing native failure"),
            Err(error) => {
                assert!(matches!(error, crate::Error::Io(_)));
                failed = true;
                break;
            }
        }
    }
    assert!(failed);
    assert_eq!(candidates.rows(), [0, 0]);
    assert_eq!(fs::read(&arm).unwrap(), b"owned fault");
    fs::remove_file(&arm).unwrap();
    if std::env::var("FLIGHTDECK_CANDIDATE_REOPEN").unwrap() == "1" {
        drop(candidates);
        candidates = store
            .resume_row_candidates(
                RingReader::open_generation(&ring).unwrap(),
                &budget,
                &ledger,
            )
            .unwrap();
    }
    row_candidates_finish(&mut candidates);
    assert_eq!(candidates.rows(), [2, 0]);
    assert_eq!(ledger.work().links, 0);
    assert_eq!(ledger.work().cleanup_removals, 0);
    eprintln!(
        "candidate-native work={:?} reservation_peak={}",
        ledger.work(),
        budget.used_peak()
    );
}

#[test]
fn row_candidates_original_aggregate_refuses_before_plan() {
    // Goal: establish the production original row bound before fixing any candidate intent.
    // Method: encode121full numeric batches through the registered original capability.
    let fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    let slot = fixture.slot();
    let count = crate::event_observation::BATCH_ROWS_MAX;
    let mut rows: Vec<_> = (0..count)
        .map(|position| crate::event_recovery::batch::Row {
            publication: None,
            event: crate::window::EventRecord {
                number: collector::u64_from_usize(position) + 1,
                tick: 1,
                pid: 41,
                kind: crate::schema::event_kind::EXEC,
                coalition: None,
                usage: None,
                availability: crate::schema::availability::NOT_APPLICABLE,
                noticed: None,
            },
            identity: None,
            name: None,
            app: None,
        })
        .collect();
    for ordinal in 0..=crate::event_observation::ROWS_MAX / count {
        for (position, row) in rows.iter_mut().enumerate() {
            row.event.number = collector::u64_from_usize(ordinal * count + position) + 1;
        }
        let mut frame = crate::event_recovery::batch::frame(&rows, false).unwrap();
        crate::encoder::encode_write(
            &mut frame,
            attempt
                .create_event_batch(u32::try_from(ordinal).unwrap())
                .unwrap(),
        )
        .unwrap();
    }
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    let mut refused = false;
    for _ in 0..1000 {
        if let Err(error) = owner.step() {
            assert!(error.to_string().contains("aggregate"));
            refused = true;
            break;
        }
        if owner.plan().is_some() {
            break;
        }
    }
    assert!(refused);
    assert_eq!(fixture.slot(), slot);
}

fn row_candidates_check_union(
    object: &Path,
    rows: &[crate::event_recovery::batch::Row],
    captured: bool,
    budget: &Arc<ByteBudget>,
) {
    let actual = crate::event_recovery::read::read_batch_file(
        private::open_private_file(&object.join("fragment-0/0000.parquet"), false).unwrap(),
        &crate::event_recovery::Batch {
            first: 1,
            end: 5,
            rows: 4,
        },
        captured,
        true,
        budget,
    )
    .unwrap();
    assert!(actual.equals_missing(&crate::event_recovery::batch::frame(rows, captured).unwrap()));
}

fn row_candidates_native_command(
    fixture: &Fixture,
    library: &Path,
    boundary: &str,
    reopen: bool,
    root: &Path,
    candidate: &Path,
) -> std::process::Command {
    let target = match boundary {
        "plan" => root.join("foreground"),
        "file" => candidate.join("0000.parquet"),
        _ => candidate.to_path_buf(),
    };
    fs::write(fixture.base.join("candidate-observed"), b"").unwrap();
    if boundary == "file" && reopen {
        fs::write(fixture.base.join("candidate-fds"), b"").unwrap();
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::row_salvage_tests::",
                "row_candidates_native_effects_retry_and_restart"
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_CANDIDATE_ROOT", root)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env(
            "FLIGHTDECK_REGISTRATION_ARMED",
            fixture.base.join("candidate-arm"),
        )
        .env(
            "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
            fixture.base.join("candidate-observed"),
        )
        .env(
            "FLIGHTDECK_CANDIDATE_REOPEN",
            if reopen { "1" } else { "0" },
        );
    if boundary == "plan" {
        command
            .env("FLIGHTDECK_MUTATION_TARGET", &target)
            .env("FLIGHTDECK_MUTATION_KIND", "rename");
    } else {
        command
            .env("FLIGHTDECK_PRIVATE_SYNC_TARGET", &target)
            .env("FLIGHTDECK_SYNC_EFFECT", "1");
    }
    if boundary == "file" && reopen {
        command
            .env("FLIGHTDECK_FD_ROOT", root)
            .env("FLIGHTDECK_FD_LOG", fixture.base.join("candidate-fds"));
    }
    if boundary == "directory" {
        command.env(
            "FLIGHTDECK_SYNC_AFTER_EXISTS",
            candidate.join("0000.parquet"),
        );
    }
    command
}

#[test]
fn row_candidates_real_logical_split() {
    // Goal: enforce the actual M retained-row split without changing production constants.
    // Method: M validated original rows plus two real ring rows write two immutable candidates.
    if let Some(root) = std::env::var_os("FLIGHTDECK_CANDIDATE_SPLIT_ROOT") {
        row_candidates_split_child(Path::new(&root));
        return;
    }
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    publish_numeric_pair_at(&mut fixture, 2, 2);
    let maximum = crate::event_observation::ROWS_MAX;
    row_candidates_encode_rows(&attempt, maximum, 3);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    drop(attempt);
    drop(store);
    row_candidates_split_launch(&fixture, &object);
    let store = Store::open(&fixture.root).unwrap();
    let state =
        crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
            .unwrap();
    assert_eq!(
        state.row_plan.unwrap().split,
        collector::u64_from_usize(maximum) + 1
    );
    assert!(object.join("fragment-0/0119.parquet").exists());
    assert!(object.join("fragment-1/0000.parquet").exists());
    row_candidates_check_numbered_files(&object, maximum, &budget);
    drop(store);
}
fn row_candidates_encode_rows(attempt: &RegisteredAttempt<'_>, count: usize, first: u64) {
    assert!(count <= crate::event_observation::ROWS_MAX + crate::event_observation::BATCH_ROWS_MAX);
    let size = crate::event_observation::BATCH_ROWS_MAX;
    let mut rows = row_candidates_numeric_rows(first, size);
    for ordinal in 0..count.div_ceil(size) {
        let length = (count - ordinal * size).min(size);
        for (position, row) in rows.iter_mut().enumerate() {
            row.event.number = first + collector::u64_from_usize(ordinal * size + position);
        }
        let mut frame = crate::event_recovery::batch::frame(&rows[..length], false).unwrap();
        crate::encoder::encode_write(
            &mut frame,
            attempt
                .create_event_batch(u32::try_from(ordinal).unwrap())
                .unwrap(),
        )
        .unwrap();
    }
}
fn row_candidates_numeric_rows(first: u64, count: usize) -> Vec<crate::event_recovery::batch::Row> {
    assert!(count <= crate::event_observation::BATCH_ROWS_MAX);
    (0..count)
        .map(|position| crate::event_recovery::batch::Row {
            publication: None,
            event: crate::window::EventRecord {
                number: first + collector::u64_from_usize(position),
                tick: 2,
                pid: 41,
                kind: crate::schema::event_kind::EXEC,
                coalition: None,
                usage: None,
                availability: crate::schema::availability::NOT_APPLICABLE,
                noticed: None,
            },
            identity: None,
            name: None,
            app: None,
        })
        .collect()
}
fn row_candidates_check_numbered_files(object: &Path, maximum: usize, budget: &Arc<ByteBudget>) {
    let size = crate::event_observation::BATCH_ROWS_MAX;
    for ordinal in 0..=maximum / size {
        let (candidate, index, count) = if ordinal == maximum / size {
            (1, 0, 2)
        } else {
            (0, ordinal, size)
        };
        let first = collector::u64_from_usize(ordinal * size) + 1;
        let fact = crate::event_recovery::Batch {
            first,
            end: first + collector::u64_from_usize(count),
            rows: collector::u64_from_usize(count),
        };
        let path = object
            .join(format!("fragment-{candidate}"))
            .join(crate::event_recovery::batch_name(index));
        let actual = crate::event_recovery::read::read_batch_file(
            private::open_private_file(&path, false).unwrap(),
            &fact,
            false,
            true,
            budget,
        )
        .unwrap();
        let expected =
            crate::event_recovery::batch::frame(&row_candidates_numeric_rows(first, count), false)
                .unwrap();
        assert!(actual.equals_missing(&expected));
    }
}

#[test]
fn row_candidates_reject_contradictory_retained_inputs() {
    // Goal: retained candidates cannot contradict originals or conceal sparse/malformed output.
    // Method: alter only isolated prepared candidates, then reopen actual Store authority.
    for mode in 0..6 {
        row_candidates_bad_case(mode);
    }
}
fn row_candidates_bad_case(mode: u8) {
    let captured = mode == 1;
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    if captured {
        publish_captured_pair_at(&mut fixture, 2, 2);
    } else {
        publish_numeric_pair_at(&mut fixture, 2, 2);
    }
    let mut rows = copy_detached_rows(&fixture);
    rows[0].publication = Some(1);
    let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    row_candidates_finish(&mut owner);
    drop(owner);
    let path = object.join("fragment-0/0000.parquet");
    row_candidates_alter(&path, &mut rows, captured, mode);
    let before = fs::read(&path).unwrap();
    let mut replay = store
        .resume_attempt()
        .unwrap()
        .row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    if mode == 5 {
        row_candidates_finish(&mut replay);
        assert_eq!(replay.rows(), [2, 0]);
    } else {
        let mut refused = false;
        for _ in 0..20000 {
            if replay.step().is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused);
        assert_eq!(fs::read(path).unwrap(), before);
    }
    assert!(object.join("pending/events/4095.parquet").exists());
    assert_eq!(ledger.work().links, 0);
    assert_eq!(ledger.work().cleanup_removals, 0);
}
fn row_candidates_alter(
    path: &Path,
    rows: &mut [crate::event_recovery::batch::Row],
    captured: bool,
    mode: u8,
) {
    assert!(path.exists());
    match mode {
        0 => rows[0].event.tick += 1,
        1 => rows[0].name.as_mut().unwrap()[0] ^= 1,
        2 => rows[0].publication = None,
        3 => {
            fs::copy(path, path.with_file_name("0001.parquet")).unwrap();
            return;
        }
        4 => {}
        5 => {
            fs::write(path, b"PAR").unwrap();
            return;
        }
        _ => panic!("mode"),
    }
    let mut frame = crate::event_recovery::batch::frame(rows, captured).unwrap();
    if mode == 4 {
        frame.drop_in_place("kind").unwrap();
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap();
    crate::encoder::encode_write(&mut frame, file).unwrap();
}
#[test]
fn row_candidates_legacy_loss_route_remains_zero_survivor_only() {
    // Goal: preserve the old route's refusal without pretending it writes candidates.
    // Method: actual registered survivors reach Unsupported with unchanged slot and originals.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    publish_numeric_pair_at(&mut fixture, 2, 2);
    let rows = copy_detached_rows(&fixture);
    let mut frame = crate::event_recovery::batch::frame(&rows, false).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    let before = fixture.slot();
    let mut proof = attempt.prove_loss(fixture.reader(), &budget).unwrap();
    let mut refused = false;
    for _ in 0..1000 {
        if proof.step().is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert_eq!(fixture.slot(), before);
}
#[test]
fn row_candidates_budget_and_foreign_source_refuse_without_new_intent() {
    // Goal: admission failures leave occupied control unchanged and release reservations.
    // Method: exhaust the actual shared budget and then admit a different real ring object.
    let fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    let before = fixture.slot();
    let held = budget.reserve(budget.maximum() - budget.used()).unwrap();
    assert!(
        attempt
            .row_candidates(fixture.reader(), &budget, &ledger)
            .is_err()
    );
    assert_eq!(fixture.slot(), before);
    drop(held);
    let other = proof_fixture(false, 1);
    assert!(
        store
            .resume_attempt()
            .unwrap()
            .row_candidates(other.reader(), &budget, &ledger)
            .is_err()
    );
    assert_eq!(fixture.slot(), before);
    assert_eq!(budget.used(), 1024);
}

fn row_candidates_native_observed(fixture: &Fixture, boundary: &str, reopen: bool) {
    let observed = fs::read(fixture.base.join("candidate-observed")).unwrap();
    assert!(!observed.is_empty());
    if let Some(evidence) = std::env::var_os("FLIGHTDECK_CANDIDATE_EVIDENCE") {
        let evidence = PathBuf::from(evidence);
        fs::create_dir_all(&evidence).unwrap();
        fs::write(
            evidence.join(format!("{boundary}-{reopen}-fault.bin")),
            observed,
        )
        .unwrap();
    }
    if boundary == "file" && reopen {
        let bytes = fs::read(fixture.base.join("candidate-fds")).unwrap();
        assert!(!bytes.is_empty());
        assert!(bytes.len().is_multiple_of(4));
        let namespace_peak = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
            .max()
            .unwrap();
        let raw_peak = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|value| u16::from_le_bytes([value[2], value[3]]))
            .max()
            .unwrap();
        assert!(namespace_peak <= 8);
        eprintln!(
            "candidate scope=file-reopen namespace_peak={namespace_peak} raw_peak={raw_peak}"
        );
        if let Some(evidence) = std::env::var_os("FLIGHTDECK_CANDIDATE_EVIDENCE") {
            fs::write(PathBuf::from(evidence).join("file-reopen-fds.bin"), bytes).unwrap();
        }
    }
}

#[test]
fn row_candidates_plan_freezes_actual_held_range() {
    // Goal: durable R uses actual held oldest, including an empty intersection with the prefix.
    // Method: keep encoded originals1/2, wrap the same source through6, then inspect fixed wire.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    publish_numeric_pair_at(&mut fixture, 2, 2);
    let rows = copy_detached_rows(&fixture);
    let mut frame = crate::event_recovery::batch::frame(&rows, false).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    for tick in [3, 5] {
        publish_at(&mut fixture.writer, tick);
        publish_numeric_pair_at(&mut fixture, tick + 1, tick + 1);
    }
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    for _ in 0..1000 {
        owner.step().unwrap();
        if owner.plan().is_some() {
            break;
        }
    }
    let plan = owner.plan().unwrap();
    assert_eq!((plan.source_first, plan.source_end), (3, 3));
    let durable =
        crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
            .unwrap();
    assert_eq!(durable.row_plan, Some(plan));
    publish_at(&mut fixture.writer, 7);
    publish_numeric_pair_at(&mut fixture, 8, 8);
    drop(owner);
    let mut reopened = store
        .resume_row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    row_candidates_finish(&mut reopened);
    assert_eq!(reopened.plan(), Some(plan));
    assert_eq!(reopened.rows(), [2, 0]);
}

fn row_merge_rescue_read_last(fixture: &Fixture) -> crate::event_recovery::batch::Row {
    let mut reader = fixture.reader();
    reader.seek_event(4).unwrap();
    let collector::ring::FoundEvent::Event {
        number,
        tick,
        event,
    } = reader.read_event().unwrap()
    else {
        panic!("event4");
    };
    crate::event_recovery::batch::copy_row(
        &reader,
        &crate::NumberedEvent {
            number,
            tick,
            event: &event,
        },
        None,
    )
    .unwrap()
}

fn row_candidates_split_launch(fixture: &Fixture, object: &Path) {
    let library = native_registration_build(fixture);
    let root = fixture.root.canonicalize().unwrap();
    let observed = fixture.base.join("split-observed");
    fs::write(&observed, b"").unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::row_salvage_tests::",
                "row_candidates_real_logical_split"
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_CANDIDATE_SPLIT_ROOT", &root)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env("FLIGHTDECK_SYNC_ARMED", fixture.base.join("split-sync-arm"))
        .env(
            "FLIGHTDECK_REGISTRATION_ARMED",
            fixture.base.join("split-control-arm"),
        )
        .env(
            "FLIGHTDECK_PRIVATE_SYNC_TARGET",
            object
                .canonicalize()
                .unwrap()
                .join("fragment-0/0119.parquet"),
        )
        .env("FLIGHTDECK_SYNC_EFFECT", "1")
        .env("FLIGHTDECK_PRIVATE_SYNC_OBSERVED", observed)
        .env("FLIGHTDECK_MUTATION_TARGET", root.join("foreground"))
        .env("FLIGHTDECK_MUTATION_KIND", "rename");
    let mut child = OwnedChild(command.spawn().unwrap());
    assert!(child.wait().success());
}
fn row_candidates_split_child(root: &Path) {
    let store = Store::open(root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let ring = std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap();
    let mut owner = store
        .resume_row_candidates(
            RingReader::open_generation(&ring).unwrap(),
            &budget,
            &ledger,
        )
        .unwrap();
    let sync_arm = PathBuf::from(std::env::var_os("FLIGHTDECK_SYNC_ARMED").unwrap());
    let control_arm = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    let maximum = collector::u64_from_usize(crate::event_observation::ROWS_MAX);
    let mut faults = 0;
    let mut sync_armed = false;
    let mut control_armed = false;
    let mut prepared = false;
    for _ in 0..50000 {
        if !sync_armed
            && owner.rows()[0]
                == maximum - collector::u64_from_usize(crate::event_observation::BATCH_ROWS_MAX)
        {
            fs::write(&sync_arm, b"owned").unwrap();
            sync_armed = true;
        }
        if !control_armed && owner.rows()[0] == maximum {
            fs::write(&control_arm, b"owned").unwrap();
            control_armed = true;
        }
        match owner.step() {
            Ok(row_candidates::CandidateStep::Unresolved) => {
                panic!("ordinary candidate unexpectedly unresolved")
            }
            Ok(row_candidates::CandidateStep::Continue) => {}
            Ok(row_candidates::CandidateStep::Prepared) => {
                prepared = true;
                break;
            }
            Err(error) => {
                assert!(matches!(error, crate::Error::Io(_)));
                row_candidates_split_fault(
                    root,
                    &owner,
                    faults,
                    &sync_arm,
                    &control_arm,
                    store.recording,
                );
                faults += 1;
            }
        }
    }
    assert!(prepared);
    assert_eq!(faults, 2);
    assert_eq!(owner.rows(), [maximum, 2]);
    assert_eq!(ledger.work().links, 0);
    assert_eq!(ledger.work().cleanup_removals, 0);
    eprintln!(
        "real-M split work={:?} reservation_peak={}",
        ledger.work(),
        budget.used_peak()
    );
}
fn row_candidates_split_fault(
    root: &Path,
    owner: &row_candidates::RowCandidates<'_>,
    faults: u32,
    sync_arm: &Path,
    control_arm: &Path,
    recording: Identity,
) {
    let bytes = fs::read(root.join("foreground")).unwrap();
    let state = crate::transaction::normal::NormalState::decode(&bytes, recording).unwrap();
    let object = root
        .join("data")
        .join(collector::recording::identity_text(state.transaction).to_string());
    assert!(!object.join("fragment-1").exists());
    assert_eq!(owner.plan().unwrap().split, 0);
    match faults {
        0 => {
            assert_eq!(state.row_plan.unwrap().split, 0);
            assert_eq!(fs::read(sync_arm).unwrap(), b"owned");
            fs::remove_file(sync_arm).unwrap();
        }
        1 => {
            assert_ne!(state.row_plan.unwrap().split, 0);
            assert_eq!(fs::read(control_arm).unwrap(), b"owned");
            fs::remove_file(control_arm).unwrap();
        }
        _ => panic!("unexpected split fault"),
    }
}

#[test]
fn row_candidates_rescue_inside_a_later_missed_interval() {
    // Goal: candidate rows rescue overwritten positions inside R without becoming exact loss.
    // Method: freeze R3..7, overwrite3/4 before output, then overwrite candidate5 before replay.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    for tick in [1, 3, 5] {
        publish_at(&mut fixture.writer, tick);
        publish_numeric_pair_at(&mut fixture, tick + 1, tick + 1);
    }
    let mut reader = fixture.reader();
    reader.seek_event(6).unwrap();
    let collector::ring::FoundEvent::Event {
        number,
        tick,
        event,
    } = reader.read_event().unwrap()
    else {
        panic!("six");
    };
    let row = crate::event_recovery::batch::copy_row(
        &reader,
        &crate::NumberedEvent {
            number,
            tick,
            event: &event,
        },
        None,
    )
    .unwrap();
    let mut frame = crate::event_recovery::batch::frame(&[row], false).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    for _ in 0..1000 {
        owner.step().unwrap();
        if owner.plan().is_some() {
            break;
        }
    }
    let plan = owner.plan().unwrap();
    assert_eq!((plan.source_first, plan.source_end), (3, 7));
    publish_at(&mut fixture.writer, 7);
    publish_numeric_pair_at(&mut fixture, 8, 8);
    row_candidates_finish(&mut owner);
    assert_eq!(owner.rows(), [2, 0]);
    assert_eq!(owner.losses(), ([0, 4], 0));
    drop(owner);
    publish_at(&mut fixture.writer, 9);
    publish_numeric_pair_at(&mut fixture, 10, 10);
    let mut replay = store
        .resume_row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    row_candidates_finish(&mut replay);
    assert_eq!(replay.rows(), [2, 0]);
    assert_eq!(replay.losses(), ([0, 4], 0));
}

fn row_publication_canonical(
    fixture: &Fixture,
    budget: &Arc<ByteBudget>,
) -> polars::prelude::DataFrame {
    row_publication_canonical_at(
        &fixture.root,
        &fixture.run,
        fixture.input(false).captured,
        budget,
    )
}
fn row_publication_canonical_at(
    root_path: &Path,
    run: &RunIdentity,
    captured: bool,
    budget: &Arc<ByteBudget>,
) -> polars::prelude::DataFrame {
    let root = PrivateDirectory::open(root_path).unwrap();
    let mut state = [0; 256];
    let mut cursor = crate::canonical::CanonicalCursor::begin(
        &root,
        run.recording,
        Namespace::Fragments,
        crate::canonical::SeekRange::source_tail(Namespace::Fragments, run).unwrap(),
        &mut state,
        budget,
    )
    .unwrap();
    for _ in 0..1000 {
        match cursor.step().unwrap() {
            crate::canonical::ReadStep::Continue => {}
            crate::canonical::ReadStep::End => panic!("missing canonical fragment"),
            crate::canonical::ReadStep::Found(key) => {
                let directory = crate::canonical::insert::open_locator(
                    &root,
                    &cursor.current_locator(),
                    &mut ReadWork::default(),
                )
                .unwrap();
                let admitted = crate::canonical::rows::admit(
                    &root,
                    directory.open_file(&key.file_name()).unwrap(),
                    Namespace::Fragments,
                    &key,
                    run,
                    budget,
                    &mut ReadWork::default(),
                )
                .unwrap();
                let metadata = admitted.metadata;
                assert_eq!(metadata.lifecycle, crate::canonical::Lifecycle::Active);
                let crate::canonical::metadata::Body::Events(manifest) = metadata.body else {
                    panic!("fragment");
                };
                let raw = root_path
                    .join("data")
                    .join(collector::recording::identity_text(metadata.transaction).to_string())
                    .join(metadata.child.name());
                return row_publication_canonical_batches(&raw, &manifest, captured, budget);
            }
        }
    }
    panic!("canonical cursor budget");
}

fn row_publication_canonical_batches(
    raw: &Path,
    manifest: &crate::event_recovery::Manifest,
    captured: bool,
    budget: &Arc<ByteBudget>,
) -> polars::prelude::DataFrame {
    let mut result: Option<polars::prelude::DataFrame> = None;
    for (ordinal, batch) in manifest.batches.iter().enumerate() {
        let frame = crate::event_recovery::read::read_batch_file(
            private::open_private_file(
                &raw.join(crate::event_recovery::batch_name(ordinal)),
                false,
            )
            .unwrap(),
            batch,
            captured,
            true,
            budget,
        )
        .unwrap();
        if let Some(result) = &mut result {
            result.vstack_mut(&frame).unwrap();
        } else {
            result = Some(frame);
        }
    }
    result.unwrap()
}

fn row_publication_seed<'store>(
    fixture: &mut Fixture,
    store: &'store Store,
    budget: &Arc<ByteBudget>,
) -> RegisteredAttempt<'store> {
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), budget)
        .unwrap();
    let captured = input.captured;
    if captured {
        publish_captured_pair_at(fixture, 2, 2);
    } else {
        publish_numeric_pair_at(fixture, 2, 2);
    }
    let mut rows = copy_detached_rows(fixture);
    rows[0].publication = Some(1);
    let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    publish_at(&mut fixture.writer, 1);
    if captured {
        publish_captured_pair_at(fixture, 61, 61_000_000_001);
    } else {
        publish_numeric_pair_at(fixture, 61, 61_000_000_001);
    }
    attempt
}
fn row_publication_copy_derivatives(attempt: &RegisteredAttempt<'_>, fixture: &Fixture) {
    for name in crate::schema::DERIVED_TABLES {
        let bytes = fs::read(
            fixture
                .base
                .join("row-derivatives")
                .join(format!("{name}.parquet")),
        )
        .unwrap();
        attempt
            .create_file(&format!("{name}.parquet"))
            .unwrap()
            .write_all(&bytes)
            .unwrap();
    }
}
fn row_publication_make_derivatives(fixture: &Fixture) {
    use crate::worker::Derive;
    let tick = encode_payload_tick(&mut fixture.reader(), 1);
    let mut window = Window::new(
        WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        Limits::from_shape(&fixture.reader().shape()).unwrap(),
    );
    window
        .push_tick(&original_observation(1, &tick), &tick, &[])
        .unwrap();
    let directory = fixture.base.join("row-derivatives");
    private::create_private_dir(&directory).unwrap();
    let tables = crate::sealed_apps::SealedApps::default()
        .derive(&crate::worker::Derivation {
            window: &window,
            pending: &PrivateDirectory::open(&directory).unwrap(),
        })
        .unwrap();
    assert_eq!(tables.len(), crate::schema::DERIVED_TABLES.len());
}
fn row_publication_finish(owner: &mut RowPublication<'_>) -> RowPublicationStep {
    for _ in 0..20000 {
        let result = owner.step().unwrap();
        if result != RowPublicationStep::Continue {
            return result;
        }
    }
    panic!("row publication budget");
}
#[test]
fn row_publication_source_free_proof_keeps_rows_and_clears_the_slot() {
    // Goal: durable rows survive source loss and leave the slot to the next attempt.
    // Method: omit real derivatives before census; resume before gates or during cleanup.
    for captured in [false, true] {
        for committed in [false, true] {
            row_publication_source_free_case(captured, committed);
        }
    }
}
fn row_publication_source_free_case(captured: bool, committed: bool) {
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
    let expected = crate::event_recovery::read::read_batch_file(
        private::open_private_file(&object.join("pending/events/4095.parquet"), false).unwrap(),
        &crate::event_recovery::Batch {
            first: 1,
            end: 3,
            rows: 2,
        },
        captured,
        true,
        &budget,
    )
    .unwrap();
    // App outputs are derivatives; durable native candidate rows remain the preservation proof.
    for name in crate::schema::DERIVED_TABLES {
        fs::remove_file(object.join(format!("pending/{name}.parquet"))).unwrap();
    }
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut candidates = attempt
        .row_candidates(fixture.reader(), &budget, &ledger)
        .unwrap();
    row_candidates_finish(&mut candidates);
    drop(candidates);
    let mut owner = store
        .resume_row_publication(Some(fixture.reader()), &budget, &ledger)
        .unwrap()
        .unwrap();
    for _ in 0..10000 {
        owner.step().unwrap();
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if state.row_plan.is_some_and(|plan| plan.rows == [2, 0])
            && (!committed
                || (state.phase == bootstrap::codec::Phase::SalvageCommitted
                    && state.cleanup.ordinal == 128))
        {
            break;
        }
    }
    drop(owner);
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;
    let mut resumed = store
        .resume_row_publication(None, &budget, &ledger)
        .unwrap()
        .unwrap();
    assert_eq!(
        row_publication_finish(&mut resumed),
        RowPublicationStep::Idle
    );
    assert!(store.interrupted().unwrap().is_none());
    assert!(row_publication_canonical(&fixture, &budget).equals_missing(&expected));
    assert!(!object.join("pending/events/4095.parquet").exists());
}

#[test]
fn row_publication_valid_ticks_remain_unpublished_and_undeleted() {
    // Goal: an event fragment cannot authorize deletion of a real original tick.
    // Method: production-encode a nonempty tick table alongside the original events.
    let mut fixture = proof_fixture(false, 4);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = row_publication_seed(&mut fixture, &store, &budget);
    let mut reader = fixture.reader();
    let tick = encode_payload_tick(&mut reader, 1);
    let mut window = Window::new(
        WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        Limits::from_shape(&reader.shape()).unwrap(),
    );
    window
        .push_tick(&original_observation(1, &tick), &tick, &[])
        .unwrap();
    let mut frame = crate::frame(crate::schema::Table::Ticks, &window).unwrap();
    assert_eq!(frame.height(), 1);
    crate::encoder::encode_write(&mut frame, attempt.create_file("ticks.parquet").unwrap())
        .unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .recover_rows(fixture.reader(), &budget, &ledger)
        .unwrap();
    let mut refused = false;
    for _ in 0..10000 {
        if let Err(error) = owner.step() {
            assert!(error.to_string().contains("metrics"));
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert_eq!(ledger.work().links, 0);
    assert_eq!(ledger.work().cleanup_removals, 0);
    let object = fixture
        .root
        .join("data")
        .read_dir()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(object.join("pending/ticks.parquet").exists());
    assert!(object.join("pending/events/4095.parquet").exists());
}

#[test]
fn row_publication_native_effects_retry_and_restart() {
    // Goal: durable effects never authorize early deletion or duplicate ownership on retry.
    // Method: native sync/link/rename/unlink effect errors, with same-owner and cold reopen.
    if let Some(root) = std::env::var_os("FLIGHTDECK_ROW_PUBLICATION_ROOT") {
        row_publication_native_child(Path::new(&root));
        return;
    }
    for boundary in [
        "commitment",
        "commitment-pending",
        "plan-pending",
        "witness-read",
        "torn-unlink",
        "index-unlink",
        "first-unlink",
        "witness-create",
        "witness-prefix",
        "witness-body",
        "witness-sync",
        "witness-parent",
        "plan",
        "retirement",
        "witness-unlink",
        "create",
        "create-effect",
        "partial-prefix",
        "prefix-effect",
        "partial-inner",
        "metadata",
        "metadata-dir",
        "link",
        "structure",
        "adoption",
        "quiet",
        "unlink",
        "cursor",
        "idle",
    ] {
        for captured in [false, true] {
            for mode in 0..3 {
                row_publication_native_case(boundary, captured, mode);
            }
        }
    }
}
fn row_publication_native_case(boundary: &str, captured: bool, mode: u8) {
    let reopen = mode != 0;
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = row_publication_seed(&mut fixture, &store, &budget);
    let transaction = attempt.transaction();
    row_publication_native_original(&attempt, &fixture, boundary);
    drop(attempt);
    drop(store);
    let library = native_registration_build(&fixture);
    let mut command =
        row_publication_native_command(&fixture, transaction, &library, boundary, reopen);
    if mode == 2 {
        command.env("FLIGHTDECK_STORE_REACQUIRE", "1");
    }
    let mut child = OwnedChild(command.spawn().unwrap());
    assert!(child.wait().success(), "{boundary} mode={mode}");
    assert!(
        !fs::read(fixture.base.join("publication-observed"))
            .unwrap()
            .is_empty()
    );
    row_publication_native_evidence(&fixture, boundary, mode);
}

fn row_publication_native_command(
    fixture: &Fixture,
    transaction: Identity,
    library: &Path,
    boundary: &str,
    reopen: bool,
) -> std::process::Command {
    let root = fixture.root.canonicalize().unwrap();
    let identity = collector::recording::identity_text(transaction).to_string();
    let object = root.join("data").join(&identity);
    let key = crate::canonical::CanonicalKey::source(Namespace::Fragments, &fixture.run, 1)
        .unwrap()
        .file_name();
    let (target, mutation) = row_publication_fault_target(&root, &object, &identity, key, boundary);
    fs::write(fixture.base.join("publication-observed"), b"").unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::",
                "row_salvage_tests::row_publication_native_effects_retry_and_restart"
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_ROW_PUBLICATION_ROOT", &root)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env("FLIGHTDECK_PUBLICATION_BOUNDARY", boundary)
        .env(
            "FLIGHTDECK_CANDIDATE_REOPEN",
            if reopen { "1" } else { "0" },
        )
        .env(
            "FLIGHTDECK_REGISTRATION_ARMED",
            fixture.base.join("publication-arm"),
        )
        .env(
            "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
            fixture.base.join("publication-observed"),
        );
    if mutation.is_empty() {
        command
            .env("FLIGHTDECK_PRIVATE_SYNC_TARGET", target)
            .env("FLIGHTDECK_SYNC_EFFECT", "1");
    } else {
        command
            .env("FLIGHTDECK_MUTATION_TARGET", target)
            .env("FLIGHTDECK_MUTATION_KIND", mutation);
    }
    row_publication_native_metadata(&mut command, &object, boundary);
    if boundary == "metadata" && reopen {
        fs::write(fixture.base.join("publication-fds"), b"").unwrap();
        command
            .env("FLIGHTDECK_FD_ROOT", &root)
            .env("FLIGHTDECK_FD_LOG", fixture.base.join("publication-fds"));
    }
    if boundary == "metadata-dir" {
        command.env(
            "FLIGHTDECK_SYNC_AFTER_EXISTS",
            object.join("fragment-0/metadata"),
        );
    }
    command
}
fn row_publication_native_metadata(
    command: &mut std::process::Command,
    object: &Path,
    boundary: &str,
) {
    if boundary == "witness-read" {
        command.env(
            "FLIGHTDECK_INVENTORY_READ_TARGET",
            object.join("original-census"),
        );
    }
    if boundary == "torn-unlink" {
        command.env(
            "FLIGHTDECK_EXPECTED_ORIGINAL",
            "pending/events/1023.parquet",
        );
    }
    if boundary == "first-unlink" {
        command.env(
            "FLIGHTDECK_EXPECTED_ORIGINAL",
            "pending/events/0000.parquet",
        );
    }
    if matches!(
        boundary,
        "partial-prefix" | "partial-inner" | "prefix-effect" | "witness-prefix" | "witness-body"
    ) {
        command
            .env(
                "FLIGHTDECK_METADATA_WRITE_TARGET",
                object.join(if boundary.starts_with("witness-") {
                    "original-census"
                } else {
                    "fragment-0/metadata"
                }),
            )
            .env(
                "FLIGHTDECK_METADATA_WRITE_MODE",
                if boundary == "partial-inner" {
                    "inner"
                } else if boundary == "prefix-effect" {
                    "complete"
                } else {
                    "prefix"
                },
            );
    }
}
fn row_publication_fault_target(
    root: &Path,
    object: &Path,
    identity: &str,
    key: String,
    boundary: &str,
) -> (PathBuf, &'static str) {
    match boundary {
        "commitment-pending" | "plan-pending" => (root.join("foreground.pending"), ""),
        "witness-read" => (object.join("original-census"), "read"),
        "first-unlink" => (object.join("pending/events/0000.parquet"), "unlink"),

        "index-unlink" => (object.join("pending/events/index"), "unlink"),
        "witness-create" => (object.join("original-census"), "create-effect"),
        "witness-prefix" | "witness-body" => (object.join("original-census"), "write"),
        "witness-sync" => (object.join("original-census"), ""),
        "witness-parent" => (object.to_path_buf(), ""),
        "witness-unlink" => (object.join("original-census"), "unlink"),
        "create" => (object.join("fragment-0/metadata"), "create"),
        "create-effect" => (object.join("fragment-0/metadata"), "create-effect"),
        "partial-prefix" | "partial-inner" | "prefix-effect" => {
            (object.join("fragment-0/metadata"), "write")
        }
        "metadata" => (object.join("fragment-0/metadata"), ""),
        "metadata-dir" => (object.join("fragment-0"), ""),
        "link" => (
            root.join("event-fragments")
                .join(format!("txn-{identity}-build"))
                .join(key),
            "link",
        ),
        "structure" => (root.join("event-fragments/0"), "rename"),
        "adoption" => (root.join("event-fragments/0"), ""),
        "quiet" => (root.join("namespace-state"), "rename"),
        "unlink" | "torn-unlink" => (object.join("pending/events/4095.parquet"), "unlink"),
        _ => (root.join("foreground"), "rename"),
    }
}
fn row_publication_native_evidence(fixture: &Fixture, boundary: &str, mode: u8) {
    let reopen = mode != 0;
    let profile = if fixture.reader().dictionary_capacity().is_some() {
        "captured"
    } else {
        "numeric"
    };
    let output = std::env::var_os("FLIGHTDECK_PUBLICATION_EVIDENCE").map(PathBuf::from);
    if let Some(output) = &output {
        fs::create_dir_all(output).unwrap();
        fs::write(
            output.join(format!("{profile}-{boundary}-mode-{mode}.bin")),
            fs::read(fixture.base.join("publication-observed")).unwrap(),
        )
        .unwrap();
    }
    if boundary == "metadata" && reopen {
        let bytes = fs::read(fixture.base.join("publication-fds")).unwrap();
        assert!(!bytes.is_empty());
        assert!(bytes.len().is_multiple_of(4));
        let chunks = bytes.as_chunks::<4>().0;
        let namespace = chunks
            .iter()
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
            .max()
            .unwrap();
        let raw = chunks
            .iter()
            .map(|value| u16::from_le_bytes([value[2], value[3]]))
            .max()
            .unwrap();
        assert!(namespace <= 8, "namespace descriptors {namespace}");
        eprintln!(
            concat!(
                "publication {profile} mode={mode} metadata-reo",
                "pen owned-root namespace={namespace} raw={raw}"
            ),
            profile = profile,
            mode = mode,
            namespace = namespace,
            raw = raw
        );
        if let Some(output) = output {
            fs::write(
                output.join(format!("{profile}-mode-{mode}-metadata-reopen-fds.bin")),
                bytes,
            )
            .unwrap();
        }
    }
}
fn row_publication_native_child(root: &Path) {
    let mut store = Some(Store::open(root).unwrap());
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let ring = std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap();
    let reader = || RingReader::open_generation(&ring).unwrap();
    let captured = reader().dictionary_capacity().is_some();
    let expected = row_publication_native_expected(root, &budget, captured);
    let arm = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    let boundary = std::env::var("FLIGHTDECK_PUBLICATION_BOUNDARY").unwrap();
    let mut owner = row_publication_native_initial(
        store.as_ref().unwrap(),
        root,
        reader(),
        &budget,
        &ledger,
        &boundary,
    );
    let slot_before = fs::read(root.join("foreground")).unwrap();
    let error = row_publication_native_fault(
        root,
        store.as_ref().unwrap().recording,
        &arm,
        &boundary,
        &mut owner,
    );
    assert_eq!(error.raw_os_error(), Some(5));
    if boundary == "witness-read" {
        assert_eq!(fs::read(root.join("foreground")).unwrap(), slot_before);
    }
    row_publication_native_survives(root, &boundary);
    let before = ledger.work();
    fs::remove_file(&arm).unwrap();
    if std::env::var("FLIGHTDECK_CANDIDATE_REOPEN").unwrap() == "1" {
        drop(owner);
        if std::env::var("FLIGHTDECK_STORE_REACQUIRE").is_ok() {
            drop(store.take());
            store = Some(Store::open(root).unwrap());
        }
        let resumed = store
            .as_ref()
            .unwrap()
            .resume_row_publication(Some(reader()), &budget, &ledger)
            .unwrap();
        let Some(resumed) = resumed else {
            assert_eq!(boundary, "idle");
            row_publication_native_oracle(
                root,
                store.as_ref().unwrap(),
                &reader(),
                &budget,
                &expected,
            );
            return;
        };
        owner = resumed;
    }
    assert_eq!(row_publication_finish(&mut owner), RowPublicationStep::Idle);
    assert!(ledger.work().full_syncs > before.full_syncs);
    row_publication_native_oracle(root, store.as_ref().unwrap(), &reader(), &budget, &expected);
    row_publication_native_work(&boundary, &ledger, &budget);
}
fn row_publication_native_initial<'a>(
    store: &'a Store,
    root: &Path,
    reader: RingReader,
    budget: &Arc<ByteBudget>,
    ledger: &PublicationLedger,
    boundary: &str,
) -> RowPublication<'a> {
    let mut owner = store
        .resume_attempt()
        .unwrap()
        .recover_rows(reader, budget, ledger)
        .unwrap();
    if boundary == "witness-read" {
        row_publication_native_prepare_read(&mut owner, root, store.recording);
        drop(owner);
        owner = store
            .resume_row_publication(
                Some(
                    RingReader::open_generation(
                        &std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap(),
                    )
                    .unwrap(),
                ),
                budget,
                ledger,
            )
            .unwrap()
            .unwrap();
    }
    owner
}

fn row_publication_native_work(
    boundary: &str,
    ledger: &PublicationLedger,
    budget: &Arc<ByteBudget>,
) {
    eprintln!(
        "publication-native {boundary} work={:?} reserved_peak={}",
        ledger.work(),
        budget.used_peak()
    );
}
fn row_publication_native_survives(root: &Path, boundary: &str) {
    if !matches!(
        boundary,
        "unlink"
            | "cursor"
            | "idle"
            | "retirement"
            | "witness-unlink"
            | "torn-unlink"
            | "index-unlink"
            | "first-unlink"
    ) {
        let object = fs::read_dir(root.join("data"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(object.join("pending/events/4095.parquet").exists());
    }
}
fn row_publication_native_fault(
    root: &Path,
    recording: Identity,
    arm: &Path,
    boundary: &str,
    owner: &mut RowPublication<'_>,
) -> io::Error {
    for _ in 0..20000 {
        row_publication_arm(root, recording, arm, boundary);
        match owner.step() {
            Err(error) => return error,
            Ok(RowPublicationStep::Continue) => {}
            Ok(result) => panic!("missing native failure {boundary}: {result:?}"),
        }
    }
    panic!("native fault finite pass budget");
}
fn row_publication_native_expected(
    root: &Path,
    budget: &Arc<ByteBudget>,
    captured: bool,
) -> polars::prelude::DataFrame {
    let object = fs::read_dir(root.join("data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let name = std::env::var("FLIGHTDECK_EXPECTED_ORIGINAL")
        .unwrap_or_else(|_| "pending/events/4095.parquet".into());
    let file = private::open_private_file(&object.join(name), false).unwrap();
    crate::event_recovery::read::read_batch_file(
        file,
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
fn row_publication_native_oracle(
    root: &Path,
    store: &Store,
    reader: &RingReader,
    budget: &Arc<ByteBudget>,
    expected: &polars::prelude::DataFrame,
) {
    let run = reader.identity().unwrap();
    let actual =
        row_publication_canonical_at(root, &run, reader.dictionary_capacity().is_some(), budget);
    assert!(actual.equals_missing(expected));
    let object = fs::read_dir(root.join("data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let metadata = row_publication_read_metadata(&object, 0);
    assert_eq!(metadata.loss.events_new, 0);
    assert_eq!(metadata.loss.publications_new, 3);
    assert_eq!(metadata.loss.inherited.events, 0);
    assert_eq!(metadata.loss.inherited.publications, 0);
    let frontier = store
        .source_frontier(&run, budget, &mut ReadWork::default())
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 4);
    assert_eq!(frontier.event_end(), 3);
    assert!(!frontier.complete());
}
fn row_publication_arm(root: &Path, recording: Identity, arm: &Path, boundary: &str) {
    let bytes: [u8; 4096] = fs::read(root.join("foreground"))
        .unwrap()
        .try_into()
        .unwrap();
    let state = crate::transaction::normal::NormalState::decode(&bytes, recording).unwrap();
    let enabled = match boundary {
        "quiet" => state.phase == bootstrap::codec::Phase::Published,
        "cursor" => {
            state.phase == bootstrap::codec::Phase::SalvageCommitted
                && state.cleanup.ordinal == 4095
        }
        "commitment" | "commitment-pending" => {
            state.phase == bootstrap::codec::Phase::Registered && state.census.kind == 0
        }
        "plan" | "plan-pending" => {
            state.phase == bootstrap::codec::Phase::Registered && state.census.kind == 1
        }
        "witness-body" => row_publication_witness_len(root) == Some(12544),
        "witness-parent" => {
            state.phase == bootstrap::codec::Phase::Registered
                && state.census.kind == 1
                && row_publication_witness_len(root) == Some(394_720)
        }
        "retirement" => {
            state.phase == bootstrap::codec::Phase::SalvageCommitted && state.cleanup.directory == 1
        }
        "idle" => {
            state.phase == bootstrap::codec::Phase::SalvageCommitted && state.cleanup.region == 8
        }

        _ => true,
    };
    if enabled && !arm.exists() {
        fs::write(arm, b"owned publication fault").unwrap();
    }
}

#[test]
fn row_publication_later_ordinary_does_not_recharge_losses() {
    // Goal: a fragment owns old losses while the admitted live source supports later work.
    // Method: publish the failed window, read its frontier, persist publication4 normally.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = row_publication_seed(&mut fixture, &store, &budget);
        let ledger = PublicationLedger::new(&budget).unwrap();
        let mut owner = attempt
            .recover_rows(fixture.reader(), &budget, &ledger)
            .unwrap();
        assert_eq!(row_publication_finish(&mut owner), RowPublicationStep::Idle);
        drop(owner);
        let frontier = store
            .source_frontier(&fixture.run, &budget, &mut ReadWork::default())
            .unwrap()
            .unwrap();
        assert_eq!((frontier.publication_end(), frontier.event_end()), (4, 3));
        assert!(!frontier.complete());
        let mut input = fixture.input(false);
        input.operation = AttemptOperation::Ordinary {
            window: 1,
            attempted_origin: 4,
        };
        input.losses = LossTransfer {
            predecessor: frontier.predecessor().clone(),
            publication_start: 4,
            publication_inherited_end: 4,
            event_start: 3,
            event_inherited_end: 3,
            publications: 0,
            events: 0,
            unknown: None,
            prior_window: frontier.prior_window(),
        };
        let attempt = store
            .register_attempt(&input, &mut fixture.reader(), &budget)
            .unwrap();
        let manifest = encode_payload(&attempt, &fixture, 4, false);
        if captured {
            publish_captured_pair_at(&mut fixture, 121, 121_000_000_001);
        } else {
            publish_numeric_pair_at(&mut fixture, 121, 121_000_000_001);
        }
        let mut publication = issue(attempt, &manifest, fixture.reader(), &budget).unwrap();
        finish_publication(&mut publication);
        assert_eq!(manifest.losses.publications, 0);
        assert_eq!(manifest.losses.events, 0);
        let frontier = store
            .source_frontier(&fixture.run, &budget, &mut ReadWork::default())
            .unwrap()
            .unwrap();
        assert_eq!((frontier.publication_end(), frontier.event_end()), (5, 5));
    }
}
#[test]
fn row_publication_changed_originals_are_not_deleted() {
    // Goal: a completed input plan never grants blanket original deletion.
    // Method: replace a valid original with different valid rows after proof or during cleanup.
    for during_cleanup in [false, true] {
        let mut fixture = proof_fixture(false, 1);
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
        for _ in 0..10000 {
            owner.step().unwrap();
            let state = crate::transaction::normal::NormalState::decode(
                &fixture.slot(),
                fixture.run.recording,
            )
            .unwrap();
            if state.row_plan.is_some_and(|plan| plan.rows == [2, 0])
                && (!during_cleanup
                    || (state.phase == bootstrap::codec::Phase::SalvageCommitted
                        && state.cleanup.ordinal == 4095))
            {
                break;
            }
        }
        let path = object.join("pending/events/4095.parquet");
        let mut rows = row_candidates_numeric_rows(1, 2);
        rows[0].event.pid = 999;
        let replacement = fixture.base.join("owned-replacement.parquet");
        let mut frame = crate::event_recovery::batch::frame(&rows, false).unwrap();
        crate::encoder::encode_write(
            &mut frame,
            private::open_private_file(&replacement, true).unwrap(),
        )
        .unwrap();
        fs::rename(&replacement, &path).unwrap();
        let expected = fs::read(&path).unwrap();
        let mut refused = false;
        for _ in 0..10000 {
            match owner.step() {
                Err(error) => {
                    assert!(error.to_string().contains("conflicting"), "{error:?}");
                    refused = true;
                    break;
                }
                Ok(RowPublicationStep::Continue) => {}
                Ok(result) => panic!("unpreserved material completed: {result:?}"),
            }
        }
        assert!(refused);
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(row_publication_canonical(&fixture, &budget).height(), 2);
    }
}

#[test]
fn row_publication_real_split_uses_one_busy_interval() {
    if let Some(root) = std::env::var_os("FLIGHTDECK_PUBLICATION_SPLIT_ROOT") {
        row_publication_split_child(Path::new(&root));
        return;
    }

    // Goal: two real candidates acquire durable gates before Quiet or any original removal.
    // Method: generated validated originals plus actual numbered ring publications; logical scope.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    let maximum = crate::event_observation::ROWS_MAX;
    row_candidates_encode_rows(&attempt, maximum, 1);
    row_publication_split_gap(&fixture, &attempt, maximum);
    for _ in 0..maximum / 4 {
        publish_numeric_events_at(&mut fixture, 2, 2, 4);
        publish_numeric_events_at(&mut fixture, 2, 2, 0);
    }
    publish_numeric_pair_at(&mut fixture, 2, 2);
    publish_numeric_events_at(&mut fixture, 61, 61_000_000_001, 0);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    drop(attempt);
    drop(store);
    row_publication_split_launch(&fixture);
    let store = Store::open(&fixture.root).unwrap();
    assert!(!object.join("pending").exists());
    row_candidates_check_numbered_files(&object, maximum, &budget);
    row_publication_split_oracle(&fixture, &store, &budget, &object, maximum);
}
fn row_publication_split_launch(fixture: &Fixture) {
    let library = native_registration_build(fixture);
    let root = fixture.root.canonicalize().unwrap();
    let end = collector::u64_from_usize(crate::event_observation::ROWS_MAX) + 1;
    let key =
        crate::canonical::CanonicalKey::source(Namespace::Fragments, &fixture.run, end).unwrap();
    let observed = fixture.base.join("two-gate-observed");
    fs::write(&observed, b"").unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::",
                "row_salvage_tests::row_publication_real_split_uses_one_busy_interval"
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_PUBLICATION_SPLIT_ROOT", &root)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env(
            "FLIGHTDECK_REGISTRATION_ARMED",
            fixture.base.join("two-gate-arm"),
        )
        .env("FLIGHTDECK_PRIVATE_SYNC_OBSERVED", &observed)
        .env(
            "FLIGHTDECK_MUTATION_TARGET",
            root.join("event-fragments/0").join(key.file_name()),
        )
        .env("FLIGHTDECK_MUTATION_KIND", "link");
    let mut child = OwnedChild(command.spawn().unwrap());
    assert!(child.wait().success());
    let bytes = fs::read(observed).unwrap();
    assert!(!bytes.is_empty());
    if let Some(evidence) = std::env::var_os("FLIGHTDECK_PUBLICATION_EVIDENCE") {
        fs::write(
            PathBuf::from(evidence).join("second-gate-effect.bin"),
            bytes,
        )
        .unwrap();
    }
}
fn row_publication_split_child(root: &Path) {
    let store = Store::open(root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let ring = std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap();
    let reader = || RingReader::open_generation(&ring).unwrap();
    let run = reader().identity().unwrap();
    let attempt = store.resume_attempt().unwrap();
    let object = root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let mut owner = attempt.recover_rows(reader(), &budget, &ledger).unwrap();
    let arm = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    fs::write(&arm, b"owned second gate fault").unwrap();
    let mut sequence = None;
    let mut idle = false;
    let mut failed = false;
    for _ in 0..100_000 {
        let result = match owner.step() {
            Ok(result) => result,
            Err(error) => {
                assert!(!failed);
                assert_eq!(error.raw_os_error(), Some(5));
                assert!(object.join("pending/events/0000.parquet").exists());
                failed = true;
                fs::remove_file(&arm).unwrap();
                drop(owner);
                owner = store
                    .resume_row_publication(Some(reader()), &budget, &ledger)
                    .unwrap()
                    .unwrap();
                continue;
            }
        };
        if result == RowPublicationStep::Idle {
            idle = true;
            break;
        }
        assert_eq!(result, RowPublicationStep::Continue);
        row_publication_split_state(root, run.recording, &object, &mut sequence);
    }
    assert!(idle);
    assert!(failed);
    assert!(sequence.is_some());
    eprintln!(
        "logical split publication peak={} work={:?}",
        budget.used_peak(),
        ledger.work()
    );
}
fn row_publication_split_state(
    root: &Path,
    recording: Identity,
    object: &Path,
    sequence: &mut Option<u64>,
) {
    let bytes = fs::read(root.join("foreground")).unwrap();
    let state = crate::transaction::normal::NormalState::decode(&bytes, recording).unwrap();
    let namespace =
        bootstrap::codec::decode_state(&fs::read(root.join("namespace-state")).unwrap(), recording)
            .unwrap();
    if let bootstrap::codec::NamespaceState::Busy {
        sequence: current, ..
    } = namespace
    {
        if let Some(before) = sequence {
            assert_eq!(current, *before);
        }
        *sequence = Some(current);
        assert!(object.join("pending/events/0000.parquet").exists());
    }
    if state.phase == bootstrap::codec::Phase::SalvageCommitted {
        assert_eq!(state.row_plan.unwrap().committed, 3);
        assert!(matches!(
            namespace,
            bootstrap::codec::NamespaceState::Quiet { .. }
        ));
    }
}

fn row_publication_split_oracle(
    fixture: &Fixture,
    store: &Store,
    budget: &Arc<ByteBudget>,
    object: &Path,
    maximum: usize,
) {
    let first = row_publication_read_metadata(object, 0);
    let second = row_publication_read_metadata(object, 1);
    assert_eq!(second.loss.inherited.predecessor, first.reference());
    assert_eq!(first.loss.events_new + second.loss.events_new, 0);
    assert_eq!(second.loss.inherited.publications, 0);
    assert_eq!(second.loss.publications_new, 0);
    assert_eq!(first.loss.event_end, collector::u64_from_usize(maximum) + 1);
    assert_eq!(
        second.loss.event_end,
        collector::u64_from_usize(maximum) + 3
    );
    let frontier = store
        .source_frontier(&fixture.run, budget, &mut ReadWork::default())
        .unwrap()
        .unwrap();
    assert_eq!(frontier.event_end(), collector::u64_from_usize(maximum) + 3);
}

fn row_publication_split_gap(fixture: &Fixture, attempt: &RegisteredAttempt<'_>, maximum: usize) {
    let size = crate::event_observation::BATCH_ROWS_MAX;
    let mut rows = row_candidates_numeric_rows(collector::u64_from_usize(maximum - size) + 1, size);
    rows[size - 2].event.number += 2;
    rows[size - 1].event.number += 2;
    let replacement = fixture.base.join("split-original.parquet");
    let mut frame = crate::event_recovery::batch::frame(&rows, false).unwrap();
    crate::encoder::encode_write(
        &mut frame,
        private::open_private_file(&replacement, true).unwrap(),
    )
    .unwrap();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    fs::rename(replacement, object.join("pending/events/0119.parquet")).unwrap();
}
fn row_publication_read_metadata(
    object: &Path,
    candidate: usize,
) -> crate::canonical::CanonicalMetadata {
    let mut file = private::open_private_file(
        &object.join(format!("fragment-{candidate}/metadata")),
        false,
    )
    .unwrap();
    crate::canonical::CanonicalMetadata::read(&mut file).unwrap()
}

#[test]
fn row_publication_missing_window_boundary_keeps_rows_and_clears_the_slot() {
    // Goal: finite original rows do not manufacture an ordinary publication boundary, and they
    // leave the slot to the next attempt.
    // Method: preserve sparse original rows while only same-window timing remains in the ring.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    publish_numeric_pair_at(&mut fixture, 2, 2);
    let rows = copy_detached_rows(&fixture);
    let mut frame = crate::event_recovery::batch::frame(&rows, false).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    publish_numeric_events_at(&mut fixture, 3, 3, 0);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .recover_rows(fixture.reader(), &budget, &ledger)
        .unwrap();
    assert_eq!(row_publication_finish(&mut owner), RowPublicationStep::Idle);
    let canonical = row_publication_canonical(&fixture, &budget);
    assert!(canonical.equals_missing(&crate::event_recovery::batch::frame(&rows, false).unwrap()));
    assert!(store.interrupted().unwrap().is_none());
    let metadata = row_publication_read_metadata(&object, 0);
    assert_eq!(metadata.original_end, 0);
    assert_eq!(metadata.loss.publications_new, 0);
}

#[test]
fn row_publication_rescues_overwritten_rows_and_partitions_inherited_loss() {
    // Goal: independently settled old/new domains count rescued originals exactly once.
    // Method: register a real inherited prefix, retain sparse originals, overwrite the same ring.
    let mut fixture = proof_fixture(false, 1);
    publish_numeric_pair_at(&mut fixture, 2, 2);
    let mut rows = copy_detached_rows(&fixture);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.losses.publication_inherited_end = 2;
    input.losses.publications = 1;
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 2,
    };
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    rows.remove(0); // Event1 is absent from both admitted inputs and remains a new exact loss.
    let mut frame = crate::event_recovery::batch::frame(&rows, false).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(1023).unwrap()).unwrap();
    publish_numeric_events_at(&mut fixture, 3, 3, 0);
    publish_numeric_pair_at(&mut fixture, 4, 4);
    let later = row_merge_read_last_pair(&fixture, 3);
    let mut frame = crate::event_recovery::batch::frame(&later, false).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    publish_numeric_events_at(&mut fixture, 5, 5, 0);
    publish_numeric_pair_at(&mut fixture, 61, 61_000_000_001);
    let mut reader = fixture.reader();
    reader.seek_event(1).unwrap();
    assert!(matches!(
        reader.read_event().unwrap(),
        collector::ring::FoundEvent::Missed { .. }
    ));
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .recover_rows(fixture.reader(), &budget, &ledger)
        .unwrap();
    assert_eq!(row_publication_finish(&mut owner), RowPublicationStep::Idle);
    rows.extend(later);
    assert!(
        row_publication_canonical(&fixture, &budget)
            .equals_missing(&crate::event_recovery::batch::frame(&rows, false).unwrap())
    );
    let metadata = row_publication_read_metadata(&object, 0);
    assert_eq!(metadata.loss.inherited.events, 0);
    assert_eq!(metadata.loss.events_new, 1);
    assert_eq!(metadata.loss.inherited.publications, 1);
    assert_eq!(metadata.loss.publications_new, 4);
}
fn row_merge_read_last_pair(
    fixture: &Fixture,
    first: u64,
) -> Vec<crate::event_recovery::batch::Row> {
    let mut reader = fixture.reader();
    reader.seek_event(first).unwrap();
    let mut rows = Vec::new();
    for _ in 0..2 {
        let collector::ring::FoundEvent::Event {
            number,
            tick,
            event,
        } = reader.read_event().unwrap()
        else {
            panic!("retained pair");
        };
        rows.push(
            crate::event_recovery::batch::copy_row(
                &reader,
                &crate::NumberedEvent {
                    number,
                    tick,
                    event: &event,
                },
                None,
            )
            .unwrap(),
        );
    }
    rows
}

#[test]
fn row_publication_inherited_event_registration_boundary_stays_explicit() {
    // Goal: retain evidence of the registration boundary rather than fabricate a validated owner.
    // Method: real selected timing and publication loss, with a nonempty inherited event domain.
    let mut fixture = proof_fixture(false, 1);
    publish_numeric_pair_at(&mut fixture, 2, 2);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let mut input = fixture.input(false);
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 2,
    };
    input.losses.unknown = None;
    input.losses.publication_inherited_end = 2;
    input.losses.publications = 1;
    input.losses.event_inherited_end = 3;
    input.losses.events = 1;
    let before = fixture.slot();
    let error = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("inherited event rows"));
    assert_eq!(fixture.slot(), before);
    assert!(
        fs::read_dir(fixture.root.join("data"))
            .unwrap()
            .next()
            .is_none()
    );
}
#[test]
fn row_publication_new_original_behind_cursor_is_retained() {
    // Goal: a cleanup checkpoint never hides newly populated earlier ordinals.
    // Method: add valid rows behind its cursor and require final bounded census refusal.
    let mut fixture = proof_fixture(false, 1);
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
    for _ in 0..10000 {
        owner.step().unwrap();
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if state.cleanup.ordinal == 4095 {
            break;
        }
    }
    let path = object.join("pending/events/0000.parquet");
    let mut frame =
        crate::event_recovery::batch::frame(&row_candidates_numeric_rows(99, 1), false).unwrap();
    crate::encoder::encode_write(&mut frame, private::open_private_file(&path, true).unwrap())
        .unwrap();
    let expected = fs::read(&path).unwrap();
    let mut refused = false;
    for _ in 0..10000 {
        match owner.step() {
            Err(error) => {
                assert!(error.to_string().contains("behind cleanup cursor"));
                refused = true;
                break;
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(result) => panic!("unpreserved ordinal completed: {result:?}"),
        }
    }
    assert!(refused);
    assert_eq!(fs::read(&path).unwrap(), expected);
}

#[test]
fn row_publication_reopen_defers_batch_identity_errors_to_steps() {
    // Goal: constructor admits metadata without probing physical batch identities.
    // Method: remove one prepared file and require a counted, retryable continuation error.
    let mut fixture = proof_fixture(false, 1);
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
    let path = object.join("fragment-0/0000.parquet");
    let bytes = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let store = Store::open(&fixture.root).unwrap();
    let mut owner = store
        .resume_row_publication(Some(fixture.reader()), &budget, &ledger)
        .expect("metadata constructor must not open batches")
        .unwrap();
    let before = ledger.work();
    let mut refused = false;
    for _ in 0..100 {
        if let Err(error) = owner.step() {
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert!(ledger.work().inventory.open_attempts > before.inventory.open_attempts);
    let after = ledger.work();
    assert!(owner.step().is_err());
    assert!(ledger.work().inventory.open_attempts > after.inventory.open_attempts);
    let mut file = private::open_private_file(&path, true).unwrap();
    file.write_all(&bytes).unwrap();
    assert_eq!(row_publication_finish(&mut owner), RowPublicationStep::Idle);
}

#[test]
fn row_publication_torn_metadata_matches_prefix_after_store_reopen() {
    // Goal: interrupted unpublished metadata converges without overwriting contradictory output.
    // Method: truncate real proved metadata, drop Store, then replay and compare canonical rows.
    for captured in [false, true] {
        for length in [0, 137, 512, 641] {
            row_publication_torn_metadata_case(captured, length);
        }
    }
}

fn row_publication_torn_metadata_case(captured: bool, length: u64) {
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
    let expected = row_publication_read_metadata(&object, 0);
    let original = fs::read(object.join("pending/events/4095.parquet")).unwrap();
    let path = object.join("fragment-0/metadata");
    private::open_private_file_writable(&path)
        .unwrap()
        .set_len(length)
        .unwrap();
    let store = Store::open(&fixture.root).unwrap();
    let mut owner = store
        .resume_row_publication(Some(fixture.reader()), &budget, &ledger)
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::read(object.join("pending/events/4095.parquet")).unwrap(),
        original
    );
    assert_eq!(row_publication_finish(&mut owner), RowPublicationStep::Idle);
    assert_eq!(row_publication_read_metadata(&object, 0), expected);
    let actual = row_publication_canonical(&fixture, &budget);
    let batch = crate::event_recovery::Batch {
        first: 1,
        end: 3,
        rows: 2,
    };
    let source = fixture.base.join("expected-original.parquet");
    private::open_private_file(&source, true)
        .unwrap()
        .write_all(&original)
        .unwrap();
    let expected = crate::event_recovery::read::read_batch_file(
        private::open_private_file(&source, false).unwrap(),
        &batch,
        captured,
        true,
        &budget,
    )
    .unwrap();
    assert!(actual.equals_missing(&expected));
}

#[test]
fn row_publication_census_identity_refuses_equal_replacement_and_new_ordinal() {
    // Goal: identical represented values cannot authorize a substituted or newly populated inode.
    // Method: alter original names after census, retaining the exact valid bytes, before cleanup.
    for new_ordinal in [false, true] {
        let mut fixture = proof_fixture(false, 1);
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
        for _ in 0..10000 {
            owner.step().unwrap();
            let state = crate::transaction::normal::NormalState::decode(
                &fixture.slot(),
                fixture.run.recording,
            )
            .unwrap();
            if state.row_plan.is_some_and(|plan| plan.rows == [2, 0]) {
                break;
            }
        }
        let original = object.join("pending/events/4095.parquet");
        let bytes = fs::read(&original).unwrap();
        let replacement = fixture.base.join("equal-copy.parquet");
        private::open_private_file(&replacement, true)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let target = if new_ordinal {
            object.join("pending/events/0000.parquet")
        } else {
            original
        };
        fs::rename(replacement, &target).unwrap();
        let mut refused = false;
        for _ in 0..10000 {
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
        assert_eq!(fs::read(target).unwrap(), bytes);
    }
}
#[test]
fn row_publication_admitted_torn_originals_converge() {
    // Goal: exact admitted torn scratch does not block preserving valid sparse survivors.
    // Method: independent truncated-index and unframed-final-batch cases under real registration.
    for captured in [false, true] {
        for index in [false, true] {
            let mut fixture = proof_fixture(captured, 1);
            let store = Store::open(&fixture.root).unwrap();
            let budget = registration_budget();
            let attempt = row_publication_seed(&mut fixture, &store, &budget);
            let expected = row_publication_native_expected(&fixture.root, &budget, captured);
            let object = fixture
                .root
                .join("data")
                .join(collector::recording::identity_text(attempt.transaction()).to_string());
            if index {
                attempt
                    .create_event_index()
                    .unwrap()
                    .write_all(b"FD")
                    .unwrap();
            } else {
                fs::rename(
                    object.join("pending/events/4095.parquet"),
                    object.join("pending/events/1023.parquet"),
                )
                .unwrap();
                attempt
                    .create_event_batch(4095)
                    .unwrap()
                    .write_all(b"PAR1")
                    .unwrap();
            }
            let ledger = PublicationLedger::new(&budget).unwrap();
            let mut owner = attempt
                .recover_rows(fixture.reader(), &budget, &ledger)
                .unwrap();
            assert_eq!(row_publication_finish(&mut owner), RowPublicationStep::Idle);
            row_publication_native_oracle(
                &fixture.root,
                &store,
                &fixture.reader(),
                &budget,
                &expected,
            );
            assert!(!object.join("pending").exists());
            assert!(!object.join("original-census").exists());
        }
    }
}

#[test]
fn row_merge_captured_physical_packing_ends_at_logical_remainder() {
    // Goal: an M boundary cannot strand a one-row physical prefix in candidate1.
    // Method: execute production packing with one batch's rows left before M and an original
    // batch of one row more, which fits because one of its rows is short.
    let mut fixture = proof_fixture(true, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&fixture.input(false), &mut fixture.reader(), &budget)
        .unwrap();
    publish_captured_events_at(&mut fixture, 2, 2, 4);
    let templates = row_merge_read_four(&fixture, true);
    let expected = row_merge_packing_expected(&templates);
    let capacity = crate::event_recovery::batch::BATCH_ROWS_MIN;
    for (ordinal, rows) in [
        (1023, &expected[..=capacity]),
        (4095, &expected[capacity + 1..]),
    ] {
        let mut frame = crate::event_recovery::batch::frame(rows, true).unwrap();
        crate::encoder::encode_write(&mut frame, attempt.create_event_batch(ordinal).unwrap())
            .unwrap();
    }
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    for ordinal in [1023, 4095] {
        let file = private::open_private_file(
            &object
                .join("pending/events")
                .join(crate::event_recovery::batch_name(ordinal)),
            false,
        )
        .unwrap();
        crate::event_recovery::read::read_original_batch_file(file, true, true, &budget, &mut 0)
            .unwrap();
    }
    for _ in 0..=expected.len().div_ceil(4) {
        publish_captured_events_at(&mut fixture, 2, 2, 0);
        publish_captured_events_at(&mut fixture, 2, 2, 4);
    }
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut merge = row_merge::RowMerge::new(attempt, fixture.reader(), &budget, &ledger).unwrap();
    let maximum = collector::u64_from_usize(crate::event_observation::ROWS_MAX);
    let retained = maximum - collector::u64_from_usize(capacity);
    merge.output_rows_limit(maximum - retained).unwrap();
    let lengths = row_merge_captured_physical_packing_lengths(&mut merge, &expected, maximum);
    assert_eq!(lengths, [capacity, capacity, 1]);
}

fn row_merge_captured_physical_packing_lengths(
    merge: &mut row_merge::RowMerge<'_>,
    expected: &[crate::event_recovery::batch::Row],
    maximum: u64,
) -> Vec<usize> {
    let mut lengths = Vec::new();
    let mut offset = 0;
    for _ in 0..10000 {
        match merge.step().unwrap() {
            row_merge::MergeStep::Continue => {}
            row_merge::MergeStep::Batch => {
                let batch = merge.take_batch().unwrap();
                let count = batch.rows().len();
                assert!(
                    crate::event_recovery::batch::frame(batch.rows(), true)
                        .unwrap()
                        .equals_missing(
                            &crate::event_recovery::batch::frame(
                                &expected[offset..offset + count],
                                true
                            )
                            .unwrap()
                        )
                );
                offset += count;
                lengths.push(count);
                merge.output_rows_limit(maximum).unwrap();
            }
            row_merge::MergeStep::Finished => break,
        }
    }
    assert_eq!(offset, expected.len());
    lengths
}

#[path = "row_salvage_paced_tests.rs"]
mod paced;

#[path = "row_salvage_census_tests.rs"]
mod census_tests;

fn row_publication_witness_len(root: &Path) -> Option<u64> {
    let object = fs::read_dir(root.join("data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    match fs::metadata(object.join("original-census")) {
        Ok(file) => Some(file.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => panic!("witness metadata: {error}"),
    }
}

fn row_merge_packing_expected(
    templates: &[crate::event_recovery::batch::Row],
) -> Vec<crate::event_recovery::batch::Row> {
    let template = crate::event_recovery::batch::frame(templates, true).unwrap();
    let capacity = crate::event_recovery::batch::BATCH_ROWS_MIN;
    let mut expected = Vec::new();
    for position in 0..=2 * capacity {
        let mut row =
            crate::event_recovery::batch::owned_row(&template, position % 4, true).unwrap();
        row.event.number = collector::u64_from_usize(position) + 1;
        row.name = Some(vec![b'x'; collector::ring::PROGRAM_PATH_BYTES_MAX]);
        let mut app = vec![b'y'; collector::ring::PROGRAM_PATH_BYTES_MAX];
        app[0] = b'/';
        let suffix = app.len() - 4;
        app[suffix..].copy_from_slice(b".app");
        row.app = Some(app);
        if position == capacity - 1 {
            row.name = Some(b"short".to_vec());
            row.app = Some(b"/s.app".to_vec());
        }
        expected.push(row);
    }
    expected
}

#[path = "row_salvage_metadata_tests.rs"]
mod metadata_tests;

fn row_publication_native_original(
    attempt: &RegisteredAttempt<'_>,
    fixture: &Fixture,
    boundary: &str,
) {
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    match boundary {
        "first-unlink" => fs::rename(
            object.join("pending/events/4095.parquet"),
            object.join("pending/events/0000.parquet"),
        )
        .unwrap(),
        "torn-unlink" => {
            fs::rename(
                object.join("pending/events/4095.parquet"),
                object.join("pending/events/1023.parquet"),
            )
            .unwrap();
            attempt
                .create_event_batch(4095)
                .unwrap()
                .write_all(b"PAR1")
                .unwrap();
        }
        "index-unlink" => attempt
            .create_event_index()
            .unwrap()
            .write_all(b"FD")
            .unwrap(),
        _ => {}
    }
}
fn row_publication_native_prepare_read(
    owner: &mut RowPublication<'_>,
    root: &Path,
    recording: Identity,
) {
    for _ in 0..10000 {
        owner.step().unwrap();
        let state = crate::transaction::normal::NormalState::decode(
            &fs::read(root.join("foreground")).unwrap(),
            recording,
        )
        .unwrap();
        if state.row_plan.is_some_and(|plan| plan.rows == [2, 0]) {
            return;
        }
    }
    panic!("metadata preparation finite passes");
}
