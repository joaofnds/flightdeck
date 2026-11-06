use super::*;
use crate::event_recovery::batch::Row;

#[test]
fn second_candidate_native_torn_metadata_in_both_profiles() {
    // Goal: empty/prefix/inner metadata faults converge at candidate1 with full fidelity.
    // Method: execute the real M split using generated validated originals and actual source rows.
    // This is logical split coverage, not whole-producer maximum-native recovery evidence.
    if let Some(root) = std::env::var_os("FLIGHTDECK_SECOND_METADATA_ROOT") {
        second_child(Path::new(&root));
        return;
    }
    for captured in [false, true] {
        second_case(captured);
    }
}
fn second_case(captured: bool) {
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
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let expected = fixture.base.join("second-expected.parquet");
    crate::encoder::encode_write(
        &mut crate::event_recovery::batch::frame(&rows, captured).unwrap(),
        private::open_private_file(&expected, true).unwrap(),
    )
    .unwrap();
    second_originals(&attempt, &rows, captured);
    if captured {
        publish_captured_events_at(&mut fixture, 3, 3, 0);
    } else {
        publish_numeric_events_at(&mut fixture, 3, 3, 0);
    }
    drop(attempt);
    drop(store);
    let library = native_registration_build(&fixture);
    let mut child = OwnedChild(
        second_command(&fixture, &object, &library, "stop")
            .spawn()
            .unwrap(),
    );
    assert!(child.wait().success());
    assert_eq!(
        fs::metadata(object.join("fragment-1/metadata"))
            .unwrap()
            .len(),
        649
    );
    let mut child = OwnedChild(
        second_command(&fixture, &object, &library, "finish")
            .spawn()
            .unwrap(),
    );
    assert!(child.wait().success());
    assert!(!object.join("pending/events/0119.parquet").exists());
    assert!(!object.join("original-census").exists());
}
fn second_originals(attempt: &RegisteredAttempt<'_>, templates: &[Row], captured: bool) {
    let size = crate::event_observation::BATCH_ROWS_MAX;
    let maximum = crate::event_observation::ROWS_MAX;
    let template = crate::event_recovery::batch::frame(templates, captured).unwrap();
    let mut rows: Vec<Row> = (0..size)
        .map(|position| {
            crate::event_recovery::batch::owned_row(&template, position % 2, captured).unwrap()
        })
        .collect();
    for ordinal in 0..maximum.div_ceil(size) {
        let count = (maximum - ordinal * size).min(size);
        for (position, row) in rows[..count].iter_mut().enumerate() {
            row.event.number = 3 + collector::u64_from_usize(ordinal * size + position);
            row.publication = Some(1);
        }
        crate::encoder::encode_write(
            &mut crate::event_recovery::batch::frame(&rows[..count], captured).unwrap(),
            attempt
                .create_event_batch(u32::try_from(ordinal).unwrap())
                .unwrap(),
        )
        .unwrap();
    }
}
fn second_command(
    fixture: &Fixture,
    object: &Path,
    library: &Path,
    mode: &str,
) -> std::process::Command {
    let object = object.canonicalize().unwrap();
    let root = fixture.root.canonicalize().unwrap();
    let path = object.join("fragment-1/metadata");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::row_salvage_tests::",
                "metadata_tests::second_candidate_native_torn_metadata_in_both_profiles"
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_SECOND_METADATA_ROOT", &root)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env("FLIGHTDECK_CENSUS_MODE", mode)
        .env(
            "FLIGHTDECK_REGISTRATION_ARMED",
            fixture.base.join("second-arm"),
        )
        .env("FLIGHTDECK_METADATA_WRITE_TARGET", &path)
        .env("FLIGHTDECK_METADATA_WRITE_MODE", "sequence")
        .env("FLIGHTDECK_MUTATION_TARGET", &path)
        .env("FLIGHTDECK_MUTATION_KIND", "create-effect")
        .env(
            "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
            fixture.base.join("second-observed"),
        );
    fs::write(fixture.base.join("second-observed"), b"").unwrap();
    command
}
fn second_child(root: &Path) {
    let mut store = Some(Store::open(root).unwrap());
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let reader = || {
        RingReader::open_generation(&std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap())
            .unwrap()
    };
    let mode = std::env::var("FLIGHTDECK_CENSUS_MODE").unwrap();
    let arm = root.parent().unwrap().join("second-arm");
    let object = fs::read_dir(root.join("data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut owner = store
        .as_ref()
        .unwrap()
        .resume_row_publication(Some(reader()), &budget, &ledger)
        .unwrap()
        .unwrap();
    if mode == "stop" {
        fs::write(&arm, b"c").unwrap();
        second_failure(&mut owner, &object, 0);
        fs::write(&arm, b"p").unwrap();
        second_failure(&mut owner, &object, 512);
        drop(owner);
        drop(store.take());
        store = Some(Store::open(root).unwrap());
        fs::write(&arm, b"i").unwrap();
        owner = store
            .as_ref()
            .unwrap()
            .resume_row_publication(Some(reader()), &budget, &ledger)
            .unwrap()
            .unwrap();
        second_failure(&mut owner, &object, 649);
        fs::remove_file(arm).unwrap();
        std::process::exit(0);
    }
    assert_eq!(second_finish(&mut owner), RowPublicationStep::Idle);
    second_oracle(root, &object, &budget, &reader(), store.as_ref().unwrap());
    eprintln!(
        "candidate1 metadata exact union rows={} reserved_peak={} work={:?}",
        crate::event_observation::ROWS_MAX + 2,
        budget.used_peak(),
        ledger.work()
    );
}
fn second_failure(owner: &mut RowPublication<'_>, object: &Path, length: u64) {
    for _ in 0..second_passes() {
        match owner.step() {
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(5));
                assert_eq!(
                    fs::metadata(object.join("fragment-1/metadata"))
                        .unwrap()
                        .len(),
                    length
                );
                assert!(object.join("pending/events/0119.parquet").exists());
                return;
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(result) => panic!("missing candidate1 fault: {result:?}"),
        }
    }
    panic!("candidate1 finite continuations");
}
fn second_oracle(
    root: &Path,
    object: &Path,
    budget: &Arc<ByteBudget>,
    reader: &RingReader,
    store: &Store,
) {
    let captured = reader.dictionary_capacity().is_some();
    let expected = second_oracle_expected(root, budget, captured);
    let maximum = collector::u64_from_usize(crate::event_observation::ROWS_MAX);
    let mut next = 1;
    for candidate in 0..2 {
        let metadata = row_publication_read_metadata(object, candidate);
        second_oracle_gate(root, object, candidate, &metadata);
        assert_eq!(metadata.loss.events_new, 0);
        assert_eq!(metadata.loss.inherited.events, 0);
        assert_eq!(metadata.loss.publications_new, 0);
        assert_eq!(metadata.lifecycle, crate::canonical::Lifecycle::Active);
        assert!(!metadata.trailing_unknown);
        let crate::canonical::metadata::Body::Events(manifest) = metadata.body else {
            panic!("event candidate");
        };
        for (ordinal, batch) in manifest.batches.iter().enumerate() {
            let actual = crate::event_recovery::read::read_batch_file(
                private::open_private_file(
                    &object
                        .join(format!("fragment-{candidate}"))
                        .join(crate::event_recovery::batch_name(ordinal)),
                    false,
                )
                .unwrap(),
                batch,
                captured,
                true,
                budget,
            )
            .unwrap();
            let expected_rows = second_oracle_rows(&expected, &mut next, actual.height(), captured);
            assert!(actual.equals_missing(&expected_rows));
        }
    }
    assert_eq!(next, maximum + 3);
    let frontier = store
        .source_frontier(
            &reader.identity().unwrap(),
            budget,
            &mut ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.event_end(), maximum + 3);
    assert_eq!(frontier.publication_end(), 1);
    assert!(!frontier.complete());
}

fn second_oracle_rows(
    expected: &polars::prelude::DataFrame,
    next: &mut u64,
    count: usize,
    captured: bool,
) -> polars::prelude::DataFrame {
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let mut row = crate::event_recovery::batch::owned_row(
            expected,
            usize::try_from((*next - 1) % 2).unwrap(),
            captured,
        )
        .unwrap();
        row.event.number = *next;
        if *next >= 3 {
            row.publication = Some(1);
        }
        rows.push(row);
        *next += 1;
    }
    crate::event_recovery::batch::frame(&rows, captured).unwrap()
}

fn second_oracle_expected(
    root: &Path,
    budget: &Arc<ByteBudget>,
    captured: bool,
) -> polars::prelude::DataFrame {
    crate::event_recovery::read::read_batch_file(
        private::open_private_file(
            &root.parent().unwrap().join("second-expected.parquet"),
            false,
        )
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
fn second_oracle_gate(
    root: &Path,
    object: &Path,
    candidate: usize,
    metadata: &crate::canonical::CanonicalMetadata,
) {
    let canonical_root = PrivateDirectory::open(root).unwrap();
    let directory = PrivateDirectory::open(&object.join(format!("fragment-{candidate}"))).unwrap();
    let key = crate::canonical::CanonicalKey::source(
        Namespace::Fragments,
        &metadata.run,
        metadata.loss.inherited.event_start,
    )
    .unwrap();
    crate::canonical::insert::verify(
        &canonical_root,
        &directory,
        Namespace::Fragments,
        &key,
        &mut ReadWork::default(),
    )
    .unwrap();
}
fn second_passes() -> usize {
    (2 * crate::event_observation::ROWS_MAX).div_ceil(128) + 4096
}
fn second_finish(owner: &mut RowPublication<'_>) -> RowPublicationStep {
    for _ in 0..second_passes() {
        let step = owner.step().unwrap();
        if step != RowPublicationStep::Continue {
            return step;
        }
    }
    panic!("logical M continuation bound");
}
