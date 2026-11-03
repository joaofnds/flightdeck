use super::*;

#[test]
fn metadata_census_129_native_bounds_and_failures() {
    // Goal: constructor and every continuation preserve the128-position bound and failed work.
    // Method: captured files, native open failures, Store reacquisition and full rows.
    if let Some(root) = std::env::var_os("FLIGHTDECK_PACED_ROOT") {
        metadata_census_child(Path::new(&root));
        return;
    }
    let mut fixture = proof_fixture(true, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let expected = metadata_census_originals(&mut fixture, &attempt);
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = attempt
        .recover_rows(fixture.reader(), &budget, &ledger)
        .unwrap();
    for _ in 0..10000 {
        owner.step().unwrap();
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if state.row_plan.is_some_and(|plan| plan.rows == [129, 0]) {
            break;
        }
    }
    drop(owner);
    drop(store);
    metadata_census_repack(&object, &expected);
    metadata_census_launch(&fixture, &object);
}
fn metadata_census_originals(
    fixture: &mut Fixture,
    attempt: &RegisteredAttempt<'_>,
) -> Vec<crate::event_recovery::batch::Row> {
    publish_captured_events_at(fixture, 2, 2, 4);
    let template =
        crate::event_recovery::batch::frame(&row_merge_read_four(fixture, true), true).unwrap();
    let mut rows = Vec::new();
    for ordinal in 0..129 {
        let mut row = crate::event_recovery::batch::owned_row(&template, 0, true).unwrap();
        row.event.number = collector::u64_from_usize(ordinal) + 1;
        row.publication = (ordinal == 0).then_some(1);
        rows.push(row);
    }
    let mut frame = crate::event_recovery::batch::frame(&rows, true).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap()).unwrap();
    for _ in 0..34 {
        publish_captured_events_at(fixture, 2, 2, 0);
        publish_captured_events_at(fixture, 2, 2, 4);
    }
    rows
}
fn metadata_census_repack(object: &Path, rows: &[crate::event_recovery::batch::Row]) {
    let mut metadata = row_publication_read_metadata(object, 0);
    let crate::canonical::metadata::Body::Events(manifest) = &mut metadata.body else {
        panic!("events");
    };
    manifest.batches.clear();
    for (ordinal, row) in rows.iter().enumerate() {
        let path = object
            .join("fragment-0")
            .join(crate::event_recovery::batch_name(ordinal));
        let file = if ordinal == 0 {
            let file = private::open_private_file_writable(&path).unwrap();
            file.set_len(0).unwrap();
            file
        } else {
            private::open_private_file(&path, true).unwrap()
        };
        let mut frame =
            crate::event_recovery::batch::frame(std::slice::from_ref(row), true).unwrap();
        crate::encoder::encode_write(&mut frame, file).unwrap();
        manifest.batches.push(crate::event_recovery::Batch {
            first: row.event.number,
            end: row.event.number + 1,
            rows: 1,
        });
    }
    let mut file =
        private::open_private_file_writable(&object.join("fragment-0/metadata")).unwrap();
    file.set_len(0).unwrap();
    metadata.write(&mut file).unwrap();
}
fn metadata_census_launch(fixture: &Fixture, object: &Path) {
    let library = native_registration_build(fixture);
    let root = fixture.root.canonicalize().unwrap();
    for name in ["paced-opens", "paced-observed"] {
        fs::write(fixture.base.join(name), b"").unwrap();
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::row_salvage_tests::",
                "paced::metadata_census_129_native_bounds_and_failures"
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_PACED_ROOT", &root)
        .env("FLIGHTDECK_PACED_LOG", fixture.base.join("paced-opens"))
        .env(
            "FLIGHTDECK_PACED_BATCH_ARM",
            fixture.base.join("paced-batch-arm"),
        )
        .env(
            "FLIGHTDECK_REGISTRATION_ARMED",
            fixture.base.join("paced-directory-arm"),
        )
        .env(
            "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
            fixture.base.join("paced-observed"),
        )
        .env(
            "FLIGHTDECK_MUTATION_TARGET",
            object.canonicalize().unwrap().join("fragment-0"),
        )
        .env("FLIGHTDECK_MUTATION_KIND", "open")
        .env("FLIGHTDECK_OPEN_EVERY_THIRD", "1")
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name());
    let mut child = OwnedChild(command.spawn().unwrap());
    assert!(child.wait().success());
    metadata_census_evidence(fixture);
}
fn metadata_census_child(root: &Path) {
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let ring = std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap();
    let reader = || RingReader::open_generation(&ring).unwrap();
    let arm = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    let batch_arm = PathBuf::from(std::env::var_os("FLIGHTDECK_PACED_BATCH_ARM").unwrap());
    let log = PathBuf::from(std::env::var_os("FLIGHTDECK_PACED_LOG").unwrap());
    metadata_census_constructor_failures(root, &budget, &ledger, &arm, &log, &reader);
    let mut store = Some(Store::open(root).unwrap());
    let before = fs::metadata(&log).unwrap().len();
    let mut owner = store
        .as_ref()
        .unwrap()
        .resume_row_publication(Some(reader()), &budget, &ledger)
        .unwrap()
        .unwrap();
    assert_eq!(fs::metadata(&log).unwrap().len(), before);
    fs::write(&batch_arm, b"owned batch failure").unwrap();
    let mut failures = 0;
    let mut finished = false;
    for step in 0..10000 {
        let before = fs::metadata(&log).unwrap().len();
        let attempted = ledger.work().inventory.open_attempts;
        let result = owner.step();
        let probes = fs::metadata(&log).unwrap().len() - before;
        assert!(probes <= 128, "native probes={probes}");
        eprintln!(
            "paced129 step={step} native_batch_opens={probes} attempted_opens={} result={result:?}",
            ledger.work().inventory.open_attempts - attempted
        );
        match result {
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(5));
                failures += 1;
                if failures == 1 {
                    drop(owner);
                    drop(store.take());
                    store = Some(Store::open(root).unwrap());
                    owner = store
                        .as_ref()
                        .unwrap()
                        .resume_row_publication(Some(reader()), &budget, &ledger)
                        .unwrap()
                        .unwrap();
                } else {
                    assert_eq!(failures, 2);
                    fs::remove_file(&batch_arm).unwrap();
                }
            }
            Ok(RowPublicationStep::Continue) => {}
            Ok(result) => {
                assert_eq!(result, RowPublicationStep::Unresolved);
                finished = true;
                break;
            }
        }
    }
    assert!(finished);
    assert_eq!(failures, 2);
    metadata_census_oracle(root, &reader(), &budget, store.as_ref().unwrap());
    eprintln!(
        "paced129 constructor raw=0 each_step<=128 work={:?}",
        ledger.work()
    );
}

fn metadata_census_oracle(
    root: &Path,
    reader: &RingReader,
    budget: &Arc<ByteBudget>,
    store: &Store,
) {
    let object = fs::read_dir(root.join("data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let expected = crate::event_recovery::read::read_batch_file(
        private::open_private_file(&object.join("pending/events/4095.parquet"), false).unwrap(),
        &crate::event_recovery::Batch {
            first: 1,
            end: 130,
            rows: 129,
        },
        true,
        true,
        budget,
    )
    .unwrap();
    assert!(
        row_publication_canonical_at(root, &reader.identity().unwrap(), true, budget)
            .equals_missing(&expected)
    );
    metadata_census_frontier(store, reader, budget);
}

fn metadata_census_frontier(store: &Store, reader: &RingReader, budget: &Arc<ByteBudget>) {
    let frontier = store
        .source_frontier(
            &reader.identity().unwrap(),
            budget,
            &mut ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.event_end(), 130);
    assert_eq!(frontier.publication_end(), 1);
    assert!(!frontier.complete());
}
fn metadata_census_constructor_failures(
    root: &Path,
    budget: &Arc<ByteBudget>,
    ledger: &PublicationLedger,
    arm: &Path,
    log: &Path,
    reader: &impl Fn() -> RingReader,
) {
    for _ in 0..2 {
        let store = Store::open(root).unwrap();
        fs::write(arm, b"owned directory failure").unwrap();
        let before = ledger.work().inventory.open_attempts;
        let error = store
            .resume_row_publication(Some(reader()), budget, ledger)
            .err()
            .unwrap();
        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(ledger.work().inventory.open_attempts - before, 6);
        assert_eq!(fs::metadata(log).unwrap().len(), 0);
        fs::remove_file(arm).unwrap();
    }
}

fn metadata_census_evidence(fixture: &Fixture) {
    if let Some(output) = std::env::var_os("FLIGHTDECK_PUBLICATION_EVIDENCE") {
        let output = PathBuf::from(output);
        fs::create_dir_all(&output).unwrap();
        for name in ["paced-opens", "paced-observed"] {
            fs::copy(
                fixture.base.join(name),
                output.join(format!("129-captured-{name}.bin")),
            )
            .unwrap();
        }
    }
}
