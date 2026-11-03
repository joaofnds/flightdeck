use super::*;
use std::os::unix::fs::MetadataExt;

#[test]
fn review_successor_refuses_readable_numeric_generation() {
    // Goal: successor absence cannot dispose of retained original numeric events.
    // Method: keep the registered generation readable while proving through its successor.
    review_successor_refusal(false);
}

#[test]
fn review_successor_refuses_readable_captured_generation() {
    // Goal: successor absence cannot dispose of retained original captured events.
    // Method: keep the registered generation readable while proving through its successor.
    review_successor_refusal(true);
}

fn review_successor_refusal(captured: bool) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
        .unwrap();
    review_publish_pair(&mut fixture, 2);
    review_zero_index(&attempt, &fixture);
    let expected = copy_detached_rows(&fixture);
    let old_name = live_successor(&mut fixture);
    fixture.writer.finish();
    let mut original = RingReader::open_generation(&old_name).unwrap();
    let generation = original.generation();
    assert_ne!(generation, fixture.reader().generation());
    review_retained_source(&mut original, &expected);
    let transaction = attempt.transaction();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    let slot = fixture.slot();
    let index = fs::read(object.join("pending/events/index")).unwrap();
    let refused = match attempt.prove_loss(fixture.reader(), &budget) {
        Err(_) => true,
        Ok(mut publication) => review_reaches_refusal(&mut publication),
    };
    review_retained_source(&mut original, &expected);
    collector::ring::unlink_generation(&old_name).unwrap();
    eprintln!(
        "review successor captured={captured} refused={refused} phase={}",
        fixture.slot()[51]
    );
    assert!(
        refused,
        "successor certified loss of retained original events"
    );
    assert_eq!(fixture.slot(), slot);
    assert_eq!(
        fs::read(object.join("pending/events/index")).unwrap(),
        index
    );
    assert!(!object.join("loss").exists());
}

fn review_retained_source(reader: &mut RingReader, expected: &[crate::event_recovery::batch::Row]) {
    reader.seek_publication(2).unwrap();
    assert!(matches!(
        reader
            .read_timing_into(&mut collector::ring::PublicationTiming::default())
            .unwrap(),
        collector::ring::Found::Publication { publication: 2, .. }
    ));
    reader.seek_event(1).unwrap();
    for row in expected {
        let mut identity = collector::ring::IdentityObservation::EMPTY;
        let found = if reader.dictionary_capacity().is_some() {
            reader.read_event_identity(&mut identity).unwrap()
        } else {
            reader.read_event().unwrap()
        };
        let collector::ring::FoundEvent::Event {
            number,
            tick,
            event,
        } = found
        else {
            panic!("registered generation lost a retained event");
        };
        let actual = crate::event_recovery::batch::copy_row(
            reader,
            &crate::NumberedEvent {
                number,
                tick,
                event: &event,
            },
            reader.dictionary_capacity().is_some().then_some(&identity),
        )
        .unwrap();
        let captured = reader.dictionary_capacity().is_some();
        let actual =
            crate::event_recovery::batch::frame(std::slice::from_ref(&actual), captured).unwrap();
        let expected =
            crate::event_recovery::batch::frame(std::slice::from_ref(row), captured).unwrap();
        assert!(actual.equals_missing(&expected));
    }
}

fn review_reaches_refusal(publication: &mut LossPublication<'_>) -> bool {
    for _ in 0..10000 {
        match publication.step() {
            Err(_) => return true,
            Ok(PublicationStep::Idle) => return false,
            Ok(PublicationStep::Continue) => {}
        }
    }
    panic!("review proof did not settle within the existing continuation bound");
}

fn review_publish_pair(fixture: &mut Fixture, tick: u64) {
    if fixture.input(false).captured {
        publish_captured_pair_at(fixture, tick, tick);
    } else {
        publish_numeric_pair_at(fixture, tick, tick);
    }
}

fn review_zero_index(attempt: &RegisteredAttempt<'_>, fixture: &Fixture) {
    crate::event_table::index::Index {
        identity: WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        first: 1,
        end: 3,
        rows: 0,
        identities: attempt.input().captured,
        batches: Vec::new(),
    }
    .write_file(&mut attempt.create_event_index().unwrap())
    .unwrap();
}

#[test]
fn review_cleanup_refuses_torn_numeric_batch_replacement() {
    // Goal: a durable count0 plan cannot authorize deletion of a substituted valid batch.
    // Method: overwrite source rows in place, plan, replace torn4095, then resume without source.
    review_cleanup_substitution(false, true);
}

#[test]
fn review_cleanup_refuses_torn_captured_batch_replacement() {
    // Goal: a durable count0 plan cannot authorize deletion of captured survivors.
    // Method: overwrite source rows in place, plan, replace torn4095, then resume without source.
    review_cleanup_substitution(true, true);
}

#[test]
fn review_cleanup_refuses_new_numeric_batch_after_plan() {
    // Goal: an ordinal absent during proof cannot later be removed without admission.
    // Method: plan after genuine overwrite, populate4095 with copied rows and resume.
    review_cleanup_substitution(false, false);
}

#[test]
fn review_cleanup_refuses_new_captured_batch_after_plan() {
    // Goal: an ordinal absent during proof cannot hide subsequently retained captured rows.
    // Method: plan after genuine overwrite, populate4095 with copied rows and resume.
    review_cleanup_substitution(true, false);
}

fn review_cleanup_substitution(captured: bool, torn: bool) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
        .unwrap();
    review_publish_pair(&mut fixture, 2);
    let rows = copy_detached_rows(&fixture);
    review_zero_index(&attempt, &fixture);
    if torn {
        attempt
            .create_event_batch(4095)
            .unwrap()
            .write_all(b"PAR1")
            .unwrap();
    }
    let transaction = attempt.transaction();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    let generation = fixture.reader().generation();
    review_publish_pair(&mut fixture, 3);
    review_publish_pair(&mut fixture, 4);
    assert_eq!(fixture.reader().generation(), generation);
    let mut source = fixture.reader();
    source.seek_event(1).unwrap();
    assert!(matches!(
        source.read_event().unwrap(),
        collector::ring::FoundEvent::Missed { first: 1, end: 3 }
    ));
    let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
    review_await_plan(&mut publication, &fixture);
    drop(publication);
    drop(store);
    let path = object.join("pending/events/4095.parquet");
    if torn {
        assert_eq!(fs::read(&path).unwrap(), b"PAR1");
        fs::remove_file(&path).unwrap();
    }
    let events = PrivateDirectory::open(&object.join("pending/events")).unwrap();
    let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
    crate::encoder::encode_write(&mut frame, events.create_file("4095.parquet").unwrap()).unwrap();
    drop(events);
    let bytes = fs::read(&path).unwrap();
    let inode = fs::metadata(&path).unwrap().ino();
    assert_eq!(fs::metadata(&path).unwrap().nlink(), 1);
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;
    let store = Store::open(&fixture.root).unwrap();
    let refused = match store.resume_loss_publication(&budget) {
        Err(_) => true,
        Ok(mut publication) => review_reaches_refusal(&mut publication),
    };
    eprintln!(
        "review cleanup captured={captured} torn={torn} refused={refused} exists={} phase={}",
        path.exists(),
        fixture.slot()[51]
    );
    assert!(
        refused,
        "post-plan survivor was deleted instead of refusing cleanup"
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_ne!(fixture.slot()[51], 0);
}

fn review_await_plan(publication: &mut LossPublication<'_>, fixture: &Fixture) {
    for _ in 0..10000 {
        publication.step().unwrap();
        if fixture.slot()[51] == 6 {
            return;
        }
    }
    panic!("review plan did not become durable within the continuation bound");
}

#[test]
fn registered_zero_survivor_joint_loss_can_clear_and_advance() {
    // Goal: a real failed attempt can settle exact loss without inventing an empty minute.
    // Method: write a finite zero-row index, roll actual source counters, then dispose it.
    for captured in [false, true] {
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
        crate::event_table::index::Index {
            identity: WindowIdentity {
                run: fixture.run,
                window: 0,
            },
            first: 1,
            end: 3,
            rows: 0,
            identities: captured,
            batches: Vec::new(),
        }
        .write_file(&mut attempt.create_event_index().unwrap())
        .unwrap();
        overwrite_registered_source(&mut fixture);
        let mut reader = fixture.reader();
        reader.seek_publication(1).unwrap();
        assert!(matches!(
            reader
                .read_timing_into(&mut collector::ring::PublicationTiming::default())
                .unwrap(),
            collector::ring::Found::Missed { first: 1, end: 4 }
        ));
        reader.seek_event(1).unwrap();
        assert!(matches!(
            reader.read_event().unwrap(),
            collector::ring::FoundEvent::Missed { first: 1, end: 3 }
        ));
        let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
        for _ in 0..10000 {
            if publication.step().unwrap() == PublicationStep::Idle {
                break;
            }
        }
        assert!(
            store
                .source_frontier(
                    &fixture.run,
                    &budget,
                    &mut crate::canonical::ReadWork::default()
                )
                .unwrap()
                .is_some()
        );
    }
}

fn live_successor(fixture: &mut Fixture) -> String {
    let old_name = fixture.writer.name().to_owned();
    fixture.writer.finish();
    let temporary = collector::ring::Place {
        lock_path: fixture.base.join("loss-next.lock"),
        name_prefix: format!(
            "/fdl{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let temporary_writer = make_writer(fixture, &temporary);
    let previous = std::mem::replace(&mut fixture.writer, temporary_writer);
    drop(previous);
    let next = make_writer(fixture, &fixture.place);
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = next;
    fixture.binding = fixture.reader().object_binding().unwrap();
    old_name
}
fn complete_loss(publication: &mut LossPublication<'_>) {
    for _ in 0..10000 {
        let before = publication.work();
        let step = publication.step().unwrap();
        assert!(publication.work().proof_operations - before.proof_operations <= 16384);
        assert!(publication.work().cleanup_positions - before.cleanup_positions <= 128);
        assert!(publication.work().cleanup_file_attempts - before.cleanup_file_attempts <= 128);
        if step == PublicationStep::Idle {
            return;
        }
    }
    panic!("loss continuation bound");
}
fn exact_input(fixture: &Fixture) -> AttemptInput {
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input
}
fn joint_attempt<'store>(
    fixture: &mut Fixture,
    store: &'store Store,
    budget: &Arc<crate::resources::ByteBudget>,
) -> RegisteredAttempt<'store> {
    let input = exact_input(fixture);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), budget)
        .unwrap();
    if input.captured {
        publish_captured_pair_at(fixture, 2, 2);
    } else {
        publish_numeric_pair_at(fixture, 2, 2);
    }
    crate::event_table::index::Index {
        identity: WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        first: 1,
        end: 3,
        rows: 0,
        identities: input.captured,
        batches: Vec::new(),
    }
    .write_file(&mut attempt.create_event_index().unwrap())
    .unwrap();
    overwrite_registered_source(fixture);
    attempt
}
#[test]
fn loss_plan_restart_without_transport_retains_fact_and_refuses_unproved_trailing_gap() {
    // Goal: durable plan/output/gates replace source authority and exclude old losses later.
    // Method: interrupt before output and after output, unlink transport, reopen and append.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = joint_attempt(&mut fixture, &store, &budget);
        let transaction = attempt.transaction();
        let original_binding = attempt.input().binding;
        let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
        for _ in 0..10000 {
            publication.step().unwrap();
            if fixture.slot()[51] == 6 {
                break;
            }
        }
        assert_eq!(fixture.slot()[51], 6);
        let object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(transaction).to_string());
        assert!(!object.join("loss").exists());
        drop(publication);
        drop(store);
        let old_name = live_successor(&mut fixture);
        collector::ring::unlink_generation(&old_name).unwrap();
        assert!(RingReader::open_generation(&old_name).is_err());
        let store = Store::open(&fixture.root).unwrap();
        let mut publication = store.resume_loss_publication(&budget).unwrap();
        for _ in 0..10000 {
            publication.step().unwrap();
            if object.join("loss/metadata").exists() {
                break;
            }
        }
        assert!(object.join("loss/metadata").exists());
        drop(publication);
        drop(store);
        let store = Store::open(&fixture.root).unwrap();
        complete_loss(&mut store.resume_loss_publication(&budget).unwrap());
        assert!(!object.join("pending").exists());
        let bytes = fs::read(object.join("loss/metadata")).unwrap();
        assert_eq!(bytes.len(), 512);
        let decoded = crate::canonical::CanonicalMetadata::read(
            &mut File::open(object.join("loss/metadata")).unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.binding, original_binding);
        assert_ne!(decoded.binding, fixture.binding);
        assert_unproved_next_window(&fixture, &store, &budget);
    }
}
fn assert_unproved_next_window(
    fixture: &Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let frontier = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 4);
    assert_eq!(frontier.event_end(), 3);
    assert_eq!(frontier.prior_window(), None);
    let before = fixture.slot();
    let mut next = exact_input(fixture);
    next.losses.predecessor = frontier.predecessor().clone();
    next.losses.publication_start = 4;
    next.losses.publication_inherited_end = 4;
    next.losses.event_start = 3;
    next.losses.event_inherited_end = 3;
    next.operation = AttemptOperation::Ordinary {
        window: 1,
        attempted_origin: 4,
    };
    assert!(
        store
            .register_attempt(&next, &mut fixture.reader(), budget)
            .is_err()
    );
    assert_eq!(fixture.slot(), before);
    assert!(!frontier.complete());
}

fn append_after_loss(
    fixture: &mut Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
    _publish: bool,
) {
    let frontier = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    let publication = frontier.publication_end();
    let event_start = frontier.event_end();
    let mut input = exact_input(fixture);
    input.operation = AttemptOperation::Ordinary {
        window: 1,
        attempted_origin: publication,
    };
    input.losses.predecessor = frontier.predecessor().clone();
    input.losses.publication_start = publication;
    input.losses.publication_inherited_end = publication;
    input.losses.event_start = event_start;
    input.losses.event_inherited_end = event_start;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), budget)
        .unwrap();
    let manifest = encode_payload(&attempt, fixture, publication, false);
    assert_eq!(manifest.losses.publications, 0);
    assert_eq!(manifest.losses.events, 0);
    publish_at(&mut fixture.writer, 2);
    finish_publication(&mut issue(attempt, &manifest, fixture.reader(), budget).unwrap());
    let next = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(next.publication_end(), publication + 1);
    assert_eq!(next.event_end(), manifest.events_end);
    assert_eq!(next.prior_window(), Some(1));
}

#[test]
fn native_loss_publication_faults_restore_durability_and_bounded_descriptors() {
    // Goal: each loss-only durability/ownership boundary survives native effect-before-error.
    // Method: isolated bounded children fault once, drop the owner, then reopen real Store.
    if let Some(base) = std::env::var_os("FLIGHTDECK_LOSS_BASE") {
        native_loss_child(Path::new(&base));
        return;
    }
    let compiler = Fixture::new(false);
    let library = native_registration_build(&compiler);
    for boundary in [
        "read",
        "plan",
        "directory",
        "metadata",
        "alias",
        "final",
        "quiet",
        "cleanup",
        "idle",
        "plan-adopt",
        "plan-root-adopt",
        "idle-adopt",
        "plan-live",
        "visible-alias",
        "final-rename",
        "accounting",
    ] {
        native_loss_case(boundary, &library);
    }
}
fn native_loss_case(boundary: &str, library: &Path) {
    let mut fixture = proof_fixture(true, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = native_loss_case_attempt(&mut fixture, &store, &budget, boundary);
    let transaction = attempt.transaction();
    drop(attempt);
    drop(store);
    let root = fixture.root.canonicalize().unwrap();
    let target = native_loss_target(&root, &fixture.run, transaction, boundary);
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::loss_only_tests::",
                "native_loss_publication_faults_restore_durability_and_bounded_descriptors"
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_LOSS_BASE", &fixture.base)
        .env("FLIGHTDECK_LOSS_BOUNDARY", boundary)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env("FLIGHTDECK_REGISTRATION_ARMED", fixture.base.join("armed"))
        .env(
            "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
            fixture.base.join("observed"),
        )
        .env("FLIGHTDECK_FD_ROOT", &root)
        .env("FLIGHTDECK_FD_LOG", fixture.base.join("fds"));
    fs::write(fixture.base.join("observed"), b"").unwrap();
    fs::write(fixture.base.join("fds"), b"").unwrap();
    native_loss_case_fault(&mut command, boundary, &target, &root);
    if boundary == "plan-live" {
        native_loss_case_live(&mut command, &mut fixture, &root);
    } else {
        assert!(
            OwnedChild(command.spawn().unwrap()).wait().success(),
            "{boundary}"
        );
    }
    native_loss_counts(&fixture, boundary);
}
fn native_loss_counts(fixture: &Fixture, boundary: &str) {
    assert!(!fs::read(fixture.base.join("observed")).unwrap().is_empty());
    let bytes = fs::read(fixture.base.join("fds")).unwrap();
    let records = bytes.as_chunks::<4>().0;
    let namespace = records
        .iter()
        .map(|r| u16::from_le_bytes([r[0], r[1]]))
        .max()
        .unwrap();
    let raw = records
        .iter()
        .map(|r| u16::from_le_bytes([r[2], r[3]]))
        .max()
        .unwrap();
    assert!(namespace <= 8, "{boundary} namespace={namespace}");
    println!(
        "loss-native {boundary} namespace={namespace} raw={raw} samples={}",
        records.len()
    );
    if let Some(path) = std::env::var_os("FLIGHTDECK_LOSS_EVIDENCE") {
        let path = PathBuf::from(path);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join(format!("{boundary}-fds.bin")), &bytes).unwrap();
        fs::copy(
            fixture.base.join("observed"),
            path.join(format!("{boundary}-fault.bin")),
        )
        .unwrap();
    }
}

fn native_loss_target(
    root: &Path,
    run: &RunIdentity,
    transaction: Identity,
    boundary: &str,
) -> PathBuf {
    let object = root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    match boundary {
        "plan-adopt" | "plan-root-adopt" | "idle-adopt" | "accounting" => root.join("foreground"),
        "final-rename" => {
            let key = crate::canonical::CanonicalKey::source(
                crate::canonical::Namespace::Ordinary,
                run,
                1,
            )
            .unwrap();
            root.join("ordinary")
                .join(format!("{:x}", key.bytes[0] >> 4))
        }
        "visible-alias" => {
            let key = crate::canonical::CanonicalKey::source(
                crate::canonical::Namespace::Fragments,
                run,
                3,
            )
            .unwrap();
            root.join(crate::canonical::Namespace::Fragments.name())
                .join(format!("{:x}", key.bytes[0] >> 4))
                .join(key.file_name())
        }
        "read" => object.join("pending/events/index"),
        "directory" => object.join("loss"),
        "metadata" => object.join("loss/metadata"),
        "quiet" => root.join("namespace-state.pending"),
        "alias" | "final" => {
            let namespace = if boundary == "alias" {
                crate::canonical::Namespace::Fragments
            } else {
                crate::canonical::Namespace::Ordinary
            };
            root.join(namespace.name())
                .join(format!(
                    "txn-{}-build",
                    collector::recording::identity_text(transaction)
                ))
                .join(
                    crate::canonical::CanonicalKey::source(namespace, run, 1)
                        .unwrap()
                        .file_name(),
                )
        }
        _ => root.join("foreground.pending"),
    }
}
fn native_loss_child(base: &Path) {
    let root_path = base.join("recording");
    let root = PrivateDirectory::open(&root_path).unwrap();
    let store = Store::open(&root_path).unwrap();
    let budget = registration_budget();
    let ring = std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap();
    let boundary = std::env::var("FLIGHTDECK_LOSS_BOUNDARY").unwrap();
    if boundary == "accounting" {
        native_loss_accounting_child(base, &root_path, &store, &budget, &ring);
        return;
    }
    let mut publication = store
        .resume_loss_proof(RingReader::open_generation(&ring).unwrap(), &budget)
        .unwrap();
    native_loss_child_failure(base, &root_path, &store, &boundary, &mut publication);
    println!(
        "loss-native-failure {boundary} work={:?} reservation_peak={}",
        publication.work(),
        budget.used_peak()
    );
    if matches!(
        boundary.as_str(),
        "plan-adopt" | "plan-root-adopt" | "idle-adopt"
    ) {
        native_loss_adoption(base, &root_path, &boundary, publication, &store, &budget);
        return;
    }
    if boundary == "visible-alias" {
        native_loss_visible_retry(base, &root_path, &store, publication, &budget);
        return;
    }
    fs::remove_file(base.join("armed")).unwrap();
    if boundary == "plan-live" {
        native_loss_child_retry(base, &root_path, &mut publication, &store, &budget);
        return;
    }
    drop(publication);
    drop(store);
    let store = Store::open(&root_path).unwrap();
    let bytes = fs::read(root_path.join("foreground")).unwrap();
    if bytes[51] == 1 {
        complete_loss(
            &mut store
                .resume_loss_proof(RingReader::open_generation(&ring).unwrap(), &budget)
                .unwrap(),
        );
    } else if bytes[51] != 0 {
        complete_loss(&mut store.resume_loss_publication(&budget).unwrap());
    }
    assert_eq!(fs::read(root_path.join("foreground")).unwrap()[51], 0);
    assert_eq!(budget.used(), 0);
    drop(root);
}

fn native_loss_adoption(
    base: &Path,
    root_path: &Path,
    boundary: &str,
    mut publication: LossPublication<'_>,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    if boundary == "idle-adopt" {
        assert_eq!(publication.step().unwrap_err().raw_os_error(), Some(5));
        fs::remove_file(base.join("armed")).unwrap();
        complete_loss(&mut publication);
        return;
    }
    drop(publication);
    let state = crate::transaction::normal::NormalState::decode(
        &fs::read(root_path.join("foreground")).unwrap(),
        store.recording(),
    )
    .unwrap();
    let object = root_path
        .join("data")
        .join(collector::recording::identity_text(state.transaction).to_string());
    assert!(!object.join("loss").exists());
    let mut publication = store.resume_loss_publication(budget).unwrap();
    assert_eq!(publication.step().unwrap_err().raw_os_error(), Some(5));
    assert!(
        !object.join("loss").exists(),
        "adoption error allowed output creation"
    );
    assert_eq!(fs::read(root_path.join("foreground")).unwrap()[51], 6);
    fs::remove_file(base.join("armed")).unwrap();
    complete_loss(&mut publication);
}

#[test]
fn terminal_zero_survivor_claim_refuses_an_unowned_final_publication_in_both_profiles() {
    // Goal: no-row terminal publication retains completion and inherited/new loss partitions.
    // Method: save ordinary1, lose later ticks/events through actual generation rollover.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = store
            .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
            .unwrap();
        let manifest = encode_payload(&attempt, &fixture, 1, false);
        publish_at(&mut fixture.writer, 1);
        finish_publication(&mut issue(attempt, &manifest, fixture.reader(), &budget).unwrap());
        if captured {
            publish_captured_pair_at(&mut fixture, 61, 61_000_000_001);
        } else {
            publish_numeric_pair_at(&mut fixture, 61, 61_000_000_001);
        }
        overwrite_registered_source(&mut fixture);
        let mut input = rollover_input(&fixture, &store, &budget);
        input.losses.unknown = None;
        fixture.writer.finish();
        let before = fixture.slot();
        assert!(
            store
                .register_attempt(&input, &mut fixture.reader(), &budget)
                .is_err()
        );
        assert_eq!(fixture.slot(), before);
    }
}

#[test]
fn publication_only_then_event_only_loss_use_distinct_final_gates() {
    // Goal: independent source frontiers advance without inventing rows or a saved window.
    // Method: settle publication loss with empty event extent, then its lost event suffix.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let frontier = publication_loss_prefix(&mut fixture, &store, &budget);
    let mut input = exact_input(&fixture);
    input.operation = AttemptOperation::Ordinary {
        window: 1,
        attempted_origin: 4,
    };
    input.losses.predecessor = frontier.predecessor().clone();
    input.losses.publication_start = 4;
    input.losses.publication_inherited_end = 4;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    let transaction = attempt.transaction();
    crate::event_table::index::Index {
        identity: WindowIdentity {
            run: fixture.run,
            window: 1,
        },
        first: 1,
        end: 3,
        rows: 0,
        identities: false,
        batches: Vec::new(),
    }
    .write_file(&mut attempt.create_event_index().unwrap())
    .unwrap();
    complete_loss(&mut attempt.prove_loss(fixture.reader(), &budget).unwrap());
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
    assert_eq!(frontier.prior_window(), None);
    let directory = PrivateDirectory::open(
        &fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(transaction).to_string())
            .join("loss"),
    )
    .unwrap();
    let metadata =
        crate::canonical::CanonicalMetadata::read(&mut directory.open_file("metadata").unwrap())
            .unwrap();
    assert_eq!(metadata.loss.publications_new, 0);
    assert_eq!(metadata.loss.events_new, 2);
    assert_eq!(metadata.original_end, 0);
    append_after_loss(&mut fixture, &store, &budget, false);
}

fn publication_loss_prefix(
    fixture: &mut Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
) -> crate::canonical::SourceFrontier {
    let attempt = store
        .register_attempt(&exact_input(fixture), &mut fixture.reader(), budget)
        .unwrap();
    publish_numeric_pair_at(fixture, 2, 2);
    crate::event_table::index::Index {
        identity: WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        first: 1,
        end: 1,
        rows: 0,
        identities: false,
        batches: Vec::new(),
    }
    .write_file(&mut attempt.create_event_index().unwrap())
    .unwrap();
    overwrite_registered_source(fixture);
    complete_loss(&mut attempt.prove_loss(fixture.reader(), budget).unwrap());
    let frontier = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 4);
    assert_eq!(frontier.event_end(), 1);
    frontier
}

#[test]
fn high_ordinal_survivor_and_unknown_endpoint_preserve_all_registered_authority() {
    // Goal: no absent low ordinal or missing endpoint can erase surviving raw data.
    // Method: put real copied events only at4095, then separately omit the finite index.
    for high in [false, true] {
        let mut fixture = proof_fixture(true, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = store
            .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
            .unwrap();
        publish_captured_pair_at(&mut fixture, 2, 2);
        if high {
            let rows = copy_detached_rows(&fixture);
            let mut frame = crate::event_recovery::batch::frame(&rows, true).unwrap();
            crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap())
                .unwrap();
        }
        let object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        let before = fixture.slot();
        overwrite_registered_source(&mut fixture);
        let mut proof = attempt.prove_loss(fixture.reader(), &budget).unwrap();
        let mut refused = false;
        for _ in 0..10000 {
            if proof.step().is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused);
        assert_eq!(fixture.slot(), before);
        assert!(!object.join("loss").exists());
        if high {
            assert!(object.join("pending/events/4095.parquet").exists());
        }
    }
}

#[test]
fn readable_source_events_refuse_loss_and_generation_change_never_joins_proofs() {
    // Goal: neither omitted readable events nor a changed generation can authorize count0.
    // Method: retain actual events, then separately mutate only generation in a negative fixture.
    for changed in [false, true] {
        let mut fixture = proof_fixture(false, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = if changed {
            joint_attempt(&mut fixture, &store, &budget)
        } else {
            let attempt = store
                .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
                .unwrap();
            publish_numeric_pair_at(&mut fixture, 2, 2);
            crate::event_table::index::Index {
                identity: WindowIdentity {
                    run: fixture.run,
                    window: 0,
                },
                first: 1,
                end: 3,
                rows: 0,
                identities: false,
                batches: Vec::new(),
            }
            .write_file(&mut attempt.create_event_index().unwrap())
            .unwrap();
            attempt
        };
        let before = fixture.slot();
        let mut header = if changed {
            Some(WritableHeader::open(&fixture))
        } else {
            None
        };
        let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
        let previous = header
            .as_mut()
            .map(|header| header.word(2).fetch_add(1, Ordering::Release));
        let mut refused = false;
        for _ in 0..10000 {
            if publication.step().is_err() {
                refused = true;
                break;
            }
        }
        if let Some(header) = &mut header {
            header.word(2).store(previous.unwrap(), Ordering::Release);
        }
        assert!(refused);
        assert_eq!(fixture.slot(), before);
        assert_eq!(publication.work().proof_event_next, 1);
    }
}
#[test]
fn foreign_loss_names_and_metadata_are_preserved_before_any_canonical_link() {
    // Goal: registered output names and immutable bytes cannot silently adopt contradictions.
    // Method: interrupt at plan, inject one owned foreign fact, observe preserved refusal.
    for foreign_name in [false, true] {
        let mut fixture = proof_fixture(false, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = joint_attempt(&mut fixture, &store, &budget);
        let transaction = attempt.transaction();
        let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
        for _ in 0..10000 {
            publication.step().unwrap();
            if fixture.slot()[51] == 6 {
                break;
            }
        }
        drop(publication);
        let path = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(transaction).to_string());
        let object = PrivateDirectory::open(&path).unwrap();
        let loss = object.create_directory("loss").unwrap();
        let name = if foreign_name { "foreign" } else { "metadata" };
        loss.create_file(name)
            .unwrap()
            .write_all(b"foreign bytes")
            .unwrap();
        let before = fixture.slot();
        let mut publication = store.resume_loss_publication(&budget).unwrap();
        let mut refused = false;
        for _ in 0..10000 {
            if publication.step().is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused);
        assert_eq!(fixture.slot(), before);
        assert_eq!(
            fs::read(path.join("loss").join(name)).unwrap(),
            b"foreign bytes"
        );
        assert!(path.join("pending/events/index").exists());
    }
}

#[test]
fn matching_torn_loss_metadata_retries_from_durable_plan_without_transport() {
    // Goal: a torn unlinked output cannot permanently block the exact registered plan.
    // Method: retain an exact short prefix after phase6, unlink transport, then adopt.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = joint_attempt(&mut fixture, &store, &budget);
    let transaction = attempt.transaction();
    let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
    for _ in 0..10000 {
        publication.step().unwrap();
        if fixture.slot()[51] == 6 {
            break;
        }
    }
    drop(publication);
    let state = crate::transaction::normal::NormalState::decode(&fixture.slot(), store.recording())
        .unwrap();
    let mut expected = Vec::new();
    state.loss_metadata().unwrap().write(&mut expected).unwrap();
    let path = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    let object = PrivateDirectory::open(&path).unwrap();
    let loss = object.create_directory("loss").unwrap();
    loss.create_file("metadata")
        .unwrap()
        .write_all(&expected[..255])
        .unwrap();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;
    complete_loss(&mut store.resume_loss_publication(&budget).unwrap());
    assert_eq!(fs::read(path.join("loss/metadata")).unwrap(), expected);
}
#[test]
fn substituted_raw_inode_after_alias_refuses_final_gate_and_cleanup() {
    // Goal: matching bytes cannot substitute another inode for a published alias.
    // Method: interrupt after alias, replace raw metadata inode, preserve both authorities.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = joint_attempt(&mut fixture, &store, &budget);
    let transaction = attempt.transaction();
    let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
    for _ in 0..10000 {
        publication.step().unwrap();
        if publication.work().links == 1 && fixture.slot()[51] == 3 {
            let state =
                crate::transaction::normal::NormalState::decode(&fixture.slot(), store.recording())
                    .unwrap();
            if state.structure.is_none() {
                break;
            }
        }
    }
    drop(publication);
    let path = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    let loss = PrivateDirectory::open(&path.join("loss")).unwrap();
    let bytes = fs::read(path.join("loss/metadata")).unwrap();
    let held = loss.open_file("metadata").unwrap();
    loss.remove_file("metadata").unwrap();
    loss.create_file("metadata")
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    assert_ne!(
        held.metadata().unwrap().ino(),
        loss.open_file("metadata")
            .unwrap()
            .metadata()
            .unwrap()
            .ino()
    );
    let before = fixture.slot();
    let mut publication = store.resume_loss_publication(&budget).unwrap();
    let mut refused = false;
    for _ in 0..10000 {
        if publication.step().is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert_eq!(fixture.slot(), before);
    assert!(path.join("pending/events/index").exists());
    assert_eq!(held.metadata().unwrap().nlink(), 1);
}

#[test]
fn completed_source_with_retained_suffix_refuses_torn_original_endpoint() {
    // Goal: independently acquired finite source endpoints can settle an unframed original.
    // Method: retain only torn raw prefixes, lose real rows by rollover, verify exact loss.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = store
            .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
            .unwrap();
        attempt
            .create_file("ticks.parquet")
            .unwrap()
            .write_all(b"PAR1")
            .unwrap();
        attempt
            .create_event_index()
            .unwrap()
            .write_all(b"FDE")
            .unwrap();
        if captured {
            publish_captured_pair_at(&mut fixture, 2, 2);
        } else {
            publish_numeric_pair_at(&mut fixture, 2, 2);
        }
        overwrite_registered_source(&mut fixture);
        fixture.writer.finish();
        let before = fixture.slot();
        let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
        assert!(review_reaches_refusal(&mut publication));
        assert_eq!(fixture.slot(), before);
    }
}

#[test]
fn alias_precedes_final_gate_and_cleanup_waits_for_both_durable_owners() {
    // Goal: the alias/final gate protocol cannot expose Quiet or delete originals early.
    // Method: inspect actual link names/inodes after every real Store publisher step.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = joint_attempt(&mut fixture, &store, &budget);
    let transaction = attempt.transaction();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    let ordinary = crate::canonical::Namespace::Ordinary;
    let fragments = crate::canonical::Namespace::Fragments;
    let gate = fixture.root.join(ordinary.name()).join("0").join(
        crate::canonical::CanonicalKey::source(ordinary, &fixture.run, 1)
            .unwrap()
            .file_name(),
    );
    let alias = fixture.root.join(fragments.name()).join("0").join(
        crate::canonical::CanonicalKey::source(fragments, &fixture.run, 1)
            .unwrap()
            .file_name(),
    );
    let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
    for _ in 0..10000 {
        let before = publication.work();
        let step = publication.step().unwrap();
        assert!(publication.work().cleanup_positions - before.cleanup_positions <= 128);
        if gate.exists() {
            assert!(alias.exists(), "final gate became visible before its alias");
            assert_eq!(
                fs::metadata(&gate).unwrap().ino(),
                fs::metadata(&alias).unwrap().ino()
            );
        }
        if !object.join("pending").exists() {
            assert!(gate.exists());
            assert!(alias.exists());
        }
        if step == PublicationStep::Idle {
            break;
        }
    }
    assert_eq!(fixture.slot()[51], 0);
    let controls = publication.work().controls;
    assert_eq!(publication.step().unwrap(), PublicationStep::Idle);
    assert_eq!(publication.work().controls, controls);
    let bytes = fs::read(object.join("loss/metadata")).unwrap();
    assert_eq!(bytes.len(), 512);
    assert_eq!(fs::read(&gate).unwrap(), bytes);
    assert_eq!(fs::read(&alias).unwrap(), bytes);
}
#[test]
fn missing_final_gate_after_committed_hint_never_authorizes_original_cleanup() {
    // Goal: a phase7 hint cannot replace canonical inode verification after restart.
    // Method: remove a final gate in an isolated fixture and inspect refusal before removal.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = joint_attempt(&mut fixture, &store, &budget);
    let transaction = attempt.transaction();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
    for _ in 0..10000 {
        publication.step().unwrap();
        if fixture.slot()[51] == 7 {
            break;
        }
    }
    drop(publication);
    let ordinary = crate::canonical::Namespace::Ordinary;
    let gate = fixture.root.join(ordinary.name()).join("0").join(
        crate::canonical::CanonicalKey::source(ordinary, &fixture.run, 1)
            .unwrap()
            .file_name(),
    );
    assert!(gate.exists());
    fs::remove_file(&gate).unwrap();
    let before = fs::read(object.join("pending/events/index")).unwrap();
    let mut publication = store.resume_loss_publication(&budget).unwrap();
    let mut refused = false;
    for _ in 0..10000 {
        if publication.step().is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert_eq!(
        fs::read(object.join("pending/events/index")).unwrap(),
        before
    );
    assert_eq!(fixture.slot()[51], 7);
}

#[test]
fn completed_empty_generation_settles_an_original_with_no_event_directory() {
    // Goal: absence of the whole registered event directory does not create a false path.
    // Method: prove actual lost source events with no original artifacts and clean all slots.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
        .unwrap();
    publish_at(&mut fixture.writer, 1);
    fixture.writer.finish();
    let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
    complete_loss(&mut publication);
    assert!(publication.work().cleanup_positions >= 4096);
    let frontier = store
        .source_frontier(
            &fixture.run,
            &budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 2);
    assert_eq!(frontier.event_end(), 1);
}

#[test]
fn admitted_zero_row_tick_table_is_not_a_saved_reading_or_a_survivor() {
    // Goal: a valid empty physical tick table does not force a fabricated reading or park loss.
    // Method: encode an empty real Window, prove lost source coverage and retain only kind4.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
        .unwrap();
    let window = Window::new(
        WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        Limits::from_shape(&fixture.reader().shape()).unwrap(),
    );
    let mut frame = crate::frame(crate::schema::Table::Ticks, &window).unwrap();
    crate::encoder::encode_write(&mut frame, attempt.create_file("ticks.parquet").unwrap())
        .unwrap();
    publish_numeric_pair_at(&mut fixture, 2, 2);
    review_zero_index(&attempt, &fixture);
    overwrite_registered_source(&mut fixture);
    complete_loss(&mut attempt.prove_loss(fixture.reader(), &budget).unwrap());
    let frontier = store
        .source_frontier(
            &fixture.run,
            &budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.prior_window(), None);
    assert_eq!(frontier.publication_end(), 4);
    assert_eq!(frontier.event_end(), 3);
}

#[test]
fn original_metric_rows_are_preserved_and_empty_original_tables_can_settle() {
    // Goal: exact loss cannot discard a surviving non-tick metric row.
    // Method: encode complete real tables with either one reading or no reading, omit ticks.
    for survivor in [false, true] {
        let mut fixture = proof_fixture(false, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = store
            .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
            .unwrap();
        let mut window = Window::new(
            WindowIdentity {
                run: fixture.run,
                window: 0,
            },
            Limits::from_shape(&fixture.reader().shape()).unwrap(),
        );
        if survivor {
            let mut reader = fixture.reader();
            let tick = encode_payload_tick(&mut reader, 1);
            window
                .push_tick(&original_observation(1, &tick), &tick, &[])
                .unwrap();
        }
        for table in crate::schema::TABLES {
            if matches!(
                table,
                crate::schema::Table::Ticks | crate::schema::Table::Events
            ) {
                continue;
            }
            let mut frame = crate::frame(table, &window).unwrap();
            crate::encoder::encode_write(
                &mut frame,
                attempt
                    .create_file(&format!("{}.parquet", table.name()))
                    .unwrap(),
            )
            .unwrap();
        }
        publish_numeric_pair_at(&mut fixture, 2, 2);
        review_zero_index(&attempt, &fixture);
        overwrite_registered_source(&mut fixture);
        let before = fixture.slot();
        let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
        if survivor {
            let mut refused = false;
            for _ in 0..10000 {
                if publication.step().is_err() {
                    refused = true;
                    break;
                }
            }
            assert!(refused);
            assert_eq!(fixture.slot(), before);
        } else {
            complete_loss(&mut publication);
        }
    }
}

#[test]
fn loss_proof_reserves_the_shared_ledger_before_mutation_and_releases_it() {
    // Goal: the consuming route cannot issue loss authority from an independent budget.
    // Method: exhaust the caller ledger before entry, preserve bytes, release and resume.
    let mut fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = joint_attempt(&mut fixture, &store, &budget);
    let before = fixture.slot();
    let held = budget.reserve(budget.maximum() - budget.used()).unwrap();
    let Err(error) = attempt.prove_loss(fixture.reader(), &budget) else {
        panic!("exhausted ledger accepted proof");
    };
    assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    assert_eq!(fixture.slot(), before);
    drop(held);
    complete_loss(&mut store.resume_loss_proof(fixture.reader(), &budget).unwrap());
    assert_eq!(budget.used(), 0);
}

#[test]
fn loss_disposition_uses_admitted_descriptors_under_a_1023_byte_root() {
    // Goal: loss publication and raw lookup do not reopen joined absolute object paths.
    // Method: execute the actual Store route with the existing long-root private fixture.
    let mut fixture = proof_fixture_in(Fixture::new_profile(true, true), 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = joint_attempt(&mut fixture, &store, &budget);
    complete_loss(&mut attempt.prove_loss(fixture.reader(), &budget).unwrap());
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
    assert_eq!(frontier.prior_window(), None);
}

#[test]
fn framed_invalid_batch_preserves_potential_rows_but_torn_scratch_can_settle() {
    // Goal: semantic failure cannot erase another valid retained row as exact loss.
    // Method: encode two real rows, invalidate only the second, then roll the source.
    use polars::prelude::*;
    for captured in [false, true] {
        for framed in [false, true] {
            let mut fixture = proof_fixture(captured, 1);
            let store = Store::open(&fixture.root).unwrap();
            let budget = registration_budget();
            let attempt = store
                .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
                .unwrap();
            if captured {
                publish_captured_pair_at(&mut fixture, 2, 2);
            } else {
                publish_numeric_pair_at(&mut fixture, 2, 2);
            }
            let mut output = attempt.create_event_batch(4095).unwrap();
            if framed {
                let rows = copy_detached_rows(&fixture);
                let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
                frame
                    .with_column(Column::new("kind".into(), [rows[0].event.kind, 255]))
                    .unwrap();
                crate::encoder::encode_write(&mut frame, output).unwrap();
            } else {
                use std::io::Write;
                output.write_all(b"PAR1").unwrap();
                drop(output);
            }
            let transaction = attempt.transaction();
            let object = fixture
                .root
                .join("data")
                .join(collector::recording::identity_text(transaction).to_string());
            let path = object.join("pending/events/4095.parquet");
            let bytes = fs::read(&path).unwrap();
            let slot = fixture.slot();
            review_zero_index(&attempt, &fixture);
            overwrite_registered_source(&mut fixture);
            let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
            if framed {
                let error = (0..10000)
                    .find_map(|_| publication.step().err())
                    .expect("framed survivor must refuse");
                assert_eq!(error.kind(), io::ErrorKind::Unsupported);
                assert_eq!(fs::read(path).unwrap(), bytes);
                assert_eq!(fixture.slot(), slot);
                assert!(!object.join("loss").exists());
            } else {
                complete_loss(&mut publication);
                assert!(!path.exists());
            }
        }
    }
}

fn native_loss_live_pause(output: &mut std::process::ChildStdout) {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct PollDescriptor {
        descriptor: i32,
        events: i16,
        returned: i16,
    }
    unsafe extern "C" {
        fn poll(descriptors: *mut PollDescriptor, count: u32, timeout: i32) -> i32;
    }
    let started = std::time::Instant::now();
    let mut bytes = Vec::with_capacity(8192);
    for _ in 0..8192 {
        let remaining = std::time::Duration::from_secs(30)
            .checked_sub(started.elapsed())
            .expect("live pause deadline");
        let mut descriptor = PollDescriptor {
            descriptor: output.as_raw_fd(),
            events: 1,
            returned: 0,
        };
        assert_eq!(
            unsafe {
                poll(
                    &raw mut descriptor,
                    1,
                    i32::try_from(remaining.as_millis().max(1)).unwrap(),
                )
            },
            1
        );
        let mut byte = [0];
        output.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"FD-PAUSED\n") {
            return;
        }
    }
    panic!("native live proof pause bound");
}

fn native_loss_case_attempt<'a>(
    fixture: &mut Fixture,
    store: &'a Store,
    budget: &Arc<crate::resources::ByteBudget>,
    boundary: &str,
) -> RegisteredAttempt<'a> {
    if boundary == "visible-alias" {
        return native_loss_visible_attempt(fixture, store, budget);
    }
    if boundary == "plan-live" {
        let attempt = store
            .register_attempt(&exact_input(fixture), &mut fixture.reader(), budget)
            .unwrap();
        review_publish_pair(fixture, 2);
        review_zero_index(&attempt, fixture);
        review_publish_pair(fixture, 3);
        review_publish_pair(fixture, 4);
        attempt
    } else {
        joint_attempt(fixture, store, budget)
    }
}

fn native_loss_child_retry(
    base: &Path,
    root_path: &Path,
    publication: &mut LossPublication<'_>,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    use std::io::Read;
    println!("FD-PAUSED");
    io::stdout().flush().unwrap();
    io::stdin().read_exact(&mut [0; 1]).unwrap();
    for _ in 0..10000 {
        publication.step().unwrap();
        if fs::read(root_path.join("foreground")).unwrap()[51] == 6 {
            fs::copy(root_path.join("foreground"), base.join("adopted-plan")).unwrap();
            break;
        }
    }
    complete_loss(publication);
    let state = crate::transaction::normal::NormalState::decode(
        &fs::read(base.join("adopted-plan")).unwrap(),
        store.recording(),
    )
    .unwrap();
    let frontier = store
        .source_frontier(
            &state.input.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), state.loss.publication_end);
    assert_eq!(frontier.event_end(), state.loss.event_end);
}

fn native_loss_case_live(command: &mut std::process::Command, fixture: &mut Fixture, root: &Path) {
    command
        .stdout(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped());
    let mut child = OwnedChild(command.spawn().unwrap());
    native_loss_live_pause(child.0.stdout.as_mut().unwrap());
    let pending = fs::read(root.join("foreground.pending")).unwrap();
    review_publish_pair(fixture, 5);
    child.0.stdin.as_mut().unwrap().write_all(b"a").unwrap();
    assert!(child.wait().success());
    assert_eq!(
        pending,
        fs::read(fixture.base.join("adopted-plan")).unwrap()
    );
}

fn native_loss_visible_attempt<'a>(
    fixture: &mut Fixture,
    store: &'a Store,
    budget: &Arc<crate::resources::ByteBudget>,
) -> RegisteredAttempt<'a> {
    let attempt = store
        .register_attempt(&exact_input(fixture), &mut fixture.reader(), budget)
        .unwrap();
    review_publish_pair(fixture, 2);
    review_zero_index(&attempt, fixture);
    review_publish_pair(fixture, 3);
    review_publish_pair(fixture, 4);
    complete_loss(&mut attempt.prove_loss(fixture.reader(), budget).unwrap());
    let frontier = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    let mut input = exact_input(fixture);
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 4,
    };
    input.losses.predecessor = frontier.predecessor().clone();
    input.losses.publication_start = 4;
    input.losses.publication_inherited_end = 4;
    input.losses.event_start = 3;
    input.losses.event_inherited_end = 3;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), budget)
        .unwrap();
    crate::event_table::index::Index {
        identity: WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        first: 3,
        end: 7,
        rows: 0,
        identities: true,
        batches: Vec::new(),
    }
    .write_file(&mut attempt.create_event_index().unwrap())
    .unwrap();
    review_publish_pair(fixture, 5);
    review_publish_pair(fixture, 6);
    attempt
}
fn native_loss_visible_retry(
    base: &Path,
    root: &Path,
    store: &Store,
    publication: LossPublication<'_>,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let state = crate::transaction::normal::NormalState::decode(
        &fs::read(root.join("foreground")).unwrap(),
        store.recording(),
    )
    .unwrap();
    let gate = crate::canonical::CanonicalKey::source(
        crate::canonical::Namespace::Ordinary,
        &state.input.run,
        4,
    )
    .unwrap();
    let final_path = root
        .join("ordinary")
        .join(format!("{:x}", gate.bytes[0] >> 4))
        .join(gate.file_name());
    let original = root
        .join("data")
        .join(collector::recording::identity_text(state.transaction).to_string())
        .join("pending/events/index");
    assert!(original.exists());
    assert!(!final_path.exists());
    drop(publication);
    let mut retry = store.resume_loss_publication(budget).unwrap();
    let error = (0..10000)
        .find_map(|_| retry.step().err())
        .expect("visible alias adoption must sync");
    assert_eq!(error.raw_os_error(), Some(5));
    assert!(original.exists());
    assert!(!final_path.exists());
    fs::remove_file(base.join("armed")).unwrap();
    complete_loss(&mut retry);
    assert!(!original.exists());
    assert!(final_path.exists());
}

fn native_loss_case_fault(
    command: &mut std::process::Command,
    boundary: &str,
    target: &Path,
    root: &Path,
) {
    if boundary == "accounting" {
        command
            .env("FLIGHTDECK_MUTATION_TARGET", target)
            .env("FLIGHTDECK_MUTATION_KIND", "open")
            .env(
                "FLIGHTDECK_CONTROL_TRACE",
                root.parent().unwrap().join("control-trace"),
            )
            .env(
                "FLIGHTDECK_CONTROL_ARMED",
                root.parent().unwrap().join("control-armed"),
            );
        command
            .env(
                "FLIGHTDECK_CONTROL_OPEN_ARMED",
                root.parent().unwrap().join("open-armed"),
            )
            .env("FLIGHTDECK_PRIVATE_SYNC_TARGET", target)
            .env("FLIGHTDECK_SYNC_EFFECT", "1");
        return;
    }
    if matches!(boundary, "plan-adopt" | "plan-root-adopt" | "idle-adopt") {
        command
            .env("FLIGHTDECK_MUTATION_TARGET", target)
            .env("FLIGHTDECK_MUTATION_KIND", "rename")
            .env(
                "FLIGHTDECK_PRIVATE_SYNC_TARGET",
                if boundary == "plan-root-adopt" {
                    root.to_path_buf()
                } else {
                    root.join("foreground")
                },
            )
            .env("FLIGHTDECK_SYNC_EFFECT", "1");
    } else if boundary == "final-rename" {
        command
            .env("FLIGHTDECK_MUTATION_TARGET", target)
            .env("FLIGHTDECK_MUTATION_KIND", "rename");
    } else if boundary == "visible-alias" {
        command
            .env("FLIGHTDECK_MUTATION_TARGET", target)
            .env("FLIGHTDECK_MUTATION_KIND", "link")
            .env("FLIGHTDECK_PRIVATE_SYNC_TARGET", target.parent().unwrap())
            .env("FLIGHTDECK_SYNC_EFFECT", "1");
    } else if boundary == "read" {
        command.env("FLIGHTDECK_INVENTORY_READ_TARGET", target);
    } else if matches!(boundary, "alias" | "final") {
        command
            .env("FLIGHTDECK_MUTATION_TARGET", target)
            .env("FLIGHTDECK_MUTATION_KIND", "link");
    } else {
        command
            .env("FLIGHTDECK_PRIVATE_SYNC_TARGET", target)
            .env("FLIGHTDECK_SYNC_EFFECT", "1");
    }
}

fn native_loss_child_armed(
    boundary: &str,
    state: &crate::transaction::normal::NormalState,
) -> bool {
    match boundary {
        "quiet" => state.phase == bootstrap::codec::Phase::Published,
        "cleanup" => state.cleanup.region == 1 && state.cleanup.ordinal == 0,
        "idle" | "idle-adopt" => state.cleanup.region == 3 && state.cleanup.directory == 1,
        _ => true,
    }
}

#[test]
fn resumed_loss_work_keeps_previous_attempted_prefix() {
    // Goal: recovery accounting includes work attempted before the owner was dropped.
    // Method: use genuine overwrite, reach plan, reopen, and compare cumulative counters.
    let mut fixture = proof_fixture(true, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = native_loss_case_attempt(&mut fixture, &store, &budget, "plan-live");
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut publication = attempt
        .prove_loss_with_ledger(fixture.reader(), &budget, &ledger)
        .unwrap();
    review_await_plan(&mut publication, &fixture);
    let before = publication.work();
    assert!(before.inventory.positions > 4096);
    drop(publication);
    let resumed = store
        .resume_loss_publication_with_ledger(&budget, &ledger)
        .unwrap();
    assert!(resumed.work().inventory.positions >= before.inventory.positions);
}

#[test]
fn real_overwrite_loss_then_later_ordinary_retains_every_new_row_without_recharging() {
    // Goal: count0 disposition clears the foreground without charging its losses again.
    // Method: preserve the same source through genuine overwrite and save all four new rows.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = joint_attempt(&mut fixture, &store, &budget);
        complete_loss(&mut attempt.prove_loss(fixture.reader(), &budget).unwrap());
        append_after_loss(&mut fixture, &store, &budget, false);
    }
}

#[test]
fn failed_loss_construction_keeps_attempts_in_the_callers_ledger() {
    // Goal: a failed constructor and a later owner share actual attempted work.
    // Method: remove the foreground name, fail its open, restore the same inode and resume.
    let mut fixture = proof_fixture(true, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = native_loss_case_attempt(&mut fixture, &store, &budget, "plan-live");
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut publication = attempt
        .prove_loss_with_ledger(fixture.reader(), &budget, &ledger)
        .unwrap();
    review_await_plan(&mut publication, &fixture);
    let before = publication.work();
    drop(publication);
    fs::rename(
        fixture.root.join("foreground"),
        fixture.base.join("held-foreground"),
    )
    .unwrap();
    assert!(
        store
            .resume_loss_publication_with_ledger(&budget, &ledger)
            .is_err()
    );
    let failed = ledger.work();
    assert_eq!(failed.control_file_opens, before.control_file_opens + 1);
    assert_eq!(
        failed.control_read_operations,
        before.control_read_operations
    );
    fs::rename(
        fixture.base.join("held-foreground"),
        fixture.root.join("foreground"),
    )
    .unwrap();
    let mut publication = store
        .resume_loss_publication_with_ledger(&budget, &ledger)
        .unwrap();
    assert!(publication.work().control_file_opens > failed.control_file_opens);
    complete_loss(&mut publication);
    let completed = publication.work();
    drop(publication);
    assert_eq!(
        ledger.work().control_file_opens,
        completed.control_file_opens
    );
    assert_eq!(
        ledger.work().inventory.positions,
        before.inventory.positions
    );
}

fn native_loss_accounting_child(
    base: &Path,
    root: &Path,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
    ring: &str,
) {
    let ledger = PublicationLedger::new(budget).unwrap();
    fs::write(base.join("armed"), b"a").unwrap();
    fs::write(base.join("open-armed"), b"a").unwrap();
    native_loss_accounting_start(base);
    let before = ledger.work();
    let result = store.resume_loss_proof_with_ledger(
        RingReader::open_generation(ring).unwrap(),
        budget,
        &ledger,
    );
    assert_eq!(result.err().unwrap().raw_os_error(), Some(5));
    native_loss_accounting_end(base, &before, &ledger.work());
    fs::remove_file(base.join("armed")).unwrap();
    fs::remove_file(base.join("open-armed")).unwrap();
    native_loss_accounting_start(base);
    let before = ledger.work();
    let mut publication = store
        .resume_loss_proof_with_ledger(RingReader::open_generation(ring).unwrap(), budget, &ledger)
        .unwrap();
    native_loss_accounting_end(base, &before, &publication.work());
    for _ in 0..10000 {
        publication.step().unwrap();
        if fs::read(root.join("foreground")).unwrap()[51] == 6 {
            break;
        }
    }
    assert_eq!(fs::read(root.join("foreground")).unwrap()[51], 6);
    let before = publication.work();
    drop(publication);
    for _ in 0..2 {
        native_loss_accounting_start(base);
        let prefix = ledger.work();
        let mut publication = store
            .resume_loss_publication_with_ledger(budget, &ledger)
            .unwrap();
        native_loss_accounting_end(base, &prefix, &publication.work());
        if ledger.work().control_file_sync_attempts == before.control_file_sync_attempts {
            fs::write(base.join("armed"), b"a").unwrap();
            native_loss_accounting_start(base);
            let prefix = publication.work();
            assert_eq!(publication.step().unwrap_err().raw_os_error(), Some(5));
            native_loss_accounting_end_failed_sync(base, &prefix, &publication.work());
            fs::remove_file(base.join("armed")).unwrap();
        }
        native_loss_accounting_start(base);
        let prefix = publication.work();
        publication.step().unwrap();
        native_loss_accounting_end(base, &prefix, &publication.work());
        assert_eq!(
            publication.work().inventory.positions,
            before.inventory.positions
        );
        drop(publication);
    }
    complete_loss(
        &mut store
            .resume_loss_publication_with_ledger(budget, &ledger)
            .unwrap(),
    );
    assert!(ledger.work().control_file_opens > before.control_file_opens);
}
fn native_loss_accounting_start(base: &Path) {
    fs::write(base.join("control-trace"), b"").unwrap();
    fs::write(base.join("control-armed"), b"a").unwrap();
}
fn native_loss_accounting_end(base: &Path, before: &PublicationWork, after: &PublicationWork) {
    fs::remove_file(base.join("control-armed")).unwrap();
    let bytes = fs::read(base.join("control-trace")).unwrap();
    assert!(bytes.len() < 1024);
    let count = native_control_counts(&bytes);
    assert_eq!(
        after.control_file_opens - before.control_file_opens,
        count.opens
    );
    assert_eq!(
        after.control_file_creates - before.control_file_creates,
        count.creates
    );
    assert_eq!(
        2 * (after.control_file_sync_attempts - before.control_file_sync_attempts),
        count.file_syncs
    );
    assert_eq!(
        2 * (after.control_directory_sync_attempts - before.control_directory_sync_attempts),
        count.directory_syncs
    );
    println!("native-accounting operations={bytes:?} before={before:?} after={after:?}");
}

fn native_loss_accounting_end_failed_sync(
    base: &Path,
    before: &PublicationWork,
    after: &PublicationWork,
) {
    fs::remove_file(base.join("control-armed")).unwrap();
    let bytes = fs::read(base.join("control-trace")).unwrap();
    let count = native_control_counts(&bytes);
    assert_eq!(
        after.control_file_opens - before.control_file_opens,
        count.opens
    );
    assert_eq!(
        after.control_file_sync_attempts - before.control_file_sync_attempts,
        1
    );
    assert_eq!(count.file_syncs, 1);
    assert_eq!(
        after.control_directory_sync_attempts - before.control_directory_sync_attempts,
        0
    );
    assert_eq!(count.directory_syncs, 0);
    println!(
        "native-accounting failed-sync operations={bytes:?} before={before:?} after={after:?}"
    );
}

#[test]
fn producer_announces_each_nonce_profile_only_after_immutable_initialization() {
    // Goal: the real usable-name write follows header/origin/extension readiness in all creators.
    // Method: interpose usable-name pwrite and inspect the object in a bounded child.
    if let Some(base) = std::env::var_os("FLIGHTDECK_SOURCE_INIT_BASE") {
        producer_source_init_child(Path::new(&base));
        return;
    }
    let fixture = Fixture::new(false);
    let library = native_registration_build(&fixture);
    fs::write(fixture.base.join("init-log"), b"").unwrap();
    fs::write(fixture.base.join("init-objects"), b"").unwrap();
    let mut child = OwnedChild(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                concat!(
                    "store::registration_tests::loss_proof_tests::loss_only_tests::",
                    "producer_announces_each_nonce_profile_only_after_immutable_initialization"
                ),
                "--nocapture",
            ])
            .env("DYLD_INSERT_LIBRARIES", library)
            .env("FLIGHTDECK_SOURCE_INIT_BASE", &fixture.base)
            .env(
                "FLIGHTDECK_SOURCE_INIT_PREFIX",
                fixture.base.canonicalize().unwrap().join("source-init"),
            )
            .env("FLIGHTDECK_SOURCE_INIT_LOG", fixture.base.join("init-log"))
            .env(
                "FLIGHTDECK_SOURCE_INIT_OBJECTS",
                fixture.base.join("init-objects"),
            )
            .spawn()
            .unwrap(),
    );
    let success = child.wait().success();
    let names = fs::read(fixture.base.join("init-objects")).unwrap();
    assert!(names.len() <= 5 * 32);
    for name in names
        .split(|byte| *byte == b'\n')
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name).unwrap();
        assert!(name.starts_with("/fd.si."));
        match collector::ring::unlink_generation(name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("owned init cleanup: {error}"),
        }
    }
    assert!(success);
    let bytes = fs::read(fixture.base.join("init-log")).unwrap();
    assert_eq!(bytes, b"0S0R1S1R2S2R3S3R4S4R");
    println!("source-init announcements={bytes:?}");
}
fn producer_source_init_child(base: &Path) {
    let run = RunIdentity {
        recording: Identity([1, 2]),
        run: Identity([3, 4]),
        boot: Identity([5, 6]),
    };
    let shape = Shape {
        slot_count: 1,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 1,
        coalition_capacity: 1,
        event_capacity: 4,
    };
    let capacity = collector::ring::DictionaryCapacity {
        entries: 2,
        arena_bytes: 128,
    };
    let mut identities = Vec::with_capacity(5);
    for profile in 0..5 {
        let place = collector::ring::Place {
            lock_path: base.join(format!("source-init{profile}.lock")),
            name_prefix: "/fd.si.".to_owned(),
        };
        let creator = match profile {
            0 => collector::ring::Writer::create_recording(&place, shape, &run, 0),
            1 => collector::ring::Writer::create_with_identity(&place, shape, capacity, &run, 0),
            2 => collector::ring::Writer::create_with_battery(&place, shape, capacity, &run, 0),
            3 => collector::ring::Writer::create_with_disks(&place, shape, capacity, &run, 0),
            4 => collector::ring::Writer::create_with_system(&place, shape, capacity, &run, 0),
            _ => unreachable!(),
        };
        let writer = creator.unwrap();
        let reader = RingReader::open_generation(writer.name()).unwrap();
        let binding = reader.object_binding().unwrap();
        assert!(!identities.contains(&binding.identity));
        identities.push(binding.identity);
        assert_eq!(binding.generation, 1);
        assert_eq!(binding.publication_first, 1);
        assert_eq!(binding.event_first, 1);
        assert_eq!(reader.identity(), Some(run));
        assert_eq!(reader.run_start_continuous_ns(), Some(0));
        assert_eq!(reader.battery_supported(), profile >= 2);
        assert_eq!(reader.disks_supported(), profile >= 3);
        assert_eq!(writer.system_supported(), profile >= 4);
        assert_eq!(reader.object_binding().unwrap(), binding);
        let expected = match profile {
            1 => Some(collector::ring::Writer::identity_size_bytes(shape, capacity).unwrap()),
            2 => Some(collector::ring::Writer::battery_size_bytes(shape, capacity).unwrap()),
            3 => Some(collector::ring::Writer::disks_size_bytes(shape, capacity).unwrap()),
            4 => Some(collector::ring::Writer::system_size_bytes(shape, capacity).unwrap()),
            _ => None,
        };
        if let Some(expected) = expected {
            assert_eq!(
                collector::u64_from_usize(reader.mapped_size_bytes()),
                expected
            );
        }
        collector::ring::unlink_generation(writer.name()).unwrap();
    }
}

fn native_loss_child_failure(
    base: &Path,
    root_path: &Path,
    store: &Store,
    boundary: &str,
    publication: &mut LossPublication<'_>,
) {
    let mut failed = false;
    for _ in 0..10000 {
        let state = crate::transaction::normal::NormalState::decode(
            &fs::read(root_path.join("foreground")).unwrap(),
            store.recording(),
        )
        .unwrap();
        let armed = native_loss_child_armed(boundary, &state);
        if armed {
            fs::write(base.join("armed"), b"a").unwrap();
        }
        match publication.step() {
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(5), "{boundary}: {error}");
                failed = true;
                break;
            }
            Ok(PublicationStep::Continue) => {}
            Ok(PublicationStep::Idle) => panic!("fault not reached: {boundary}"),
        }
    }
    assert!(failed, "{boundary}");
}

#[derive(Default)]
struct NativeControlCounts {
    opens: u64,
    creates: u64,
    file_syncs: u64,
    directory_syncs: u64,
}
fn native_control_counts(bytes: &[u8]) -> NativeControlCounts {
    assert!(bytes.len() < 1024);
    let mut count = NativeControlCounts::default();
    for byte in bytes {
        match byte {
            b'O' => count.opens += 1,
            b'C' => count.creates += 1,
            b'F' => count.file_syncs += 1,
            b'D' => count.directory_syncs += 1,
            _ => panic!("unknown native control operation"),
        }
    }
    count
}

#[test]
fn native_cleanup_replacement_before_admission_or_name_recheck_preserves_material() {
    // Goal: the destructive boundary refuses a substitution before either actual admission.
    // Method: replace a torn ordinal with valid original bytes at its first or second openat.
    if let Some(base) = std::env::var_os("FLIGHTDECK_CLEANUP_BASE") {
        let base = Path::new(&base);
        let store = Store::open(&base.join("recording")).unwrap();
        let independently_borrowed_root = PrivateDirectory::open(&base.join("recording")).unwrap();
        let budget = registration_budget();
        let mut publication = store.resume_loss_publication(&budget).unwrap();
        fs::write(base.join("armed"), b"a").unwrap();
        assert!(review_reaches_refusal(&mut publication));
        assert_ne!(fs::read(base.join("recording/foreground")).unwrap()[51], 0);
        drop(independently_borrowed_root);
        return;
    }
    let compiler = Fixture::new(false);
    let library = native_registration_build(&compiler);
    for captured in [false, true] {
        for second in [false, true] {
            native_cleanup_replacement(captured, second, &library);
        }
    }
}
fn native_cleanup_replacement(captured: bool, second: bool, library: &Path) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
        .unwrap();
    review_publish_pair(&mut fixture, 2);
    let rows = copy_detached_rows(&fixture);
    review_zero_index(&attempt, &fixture);
    overwrite_registered_source(&mut fixture);
    attempt
        .create_event_batch(4095)
        .unwrap()
        .write_all(b"PAR1")
        .unwrap();
    let transaction = attempt.transaction();
    let mut publication = attempt.prove_loss(fixture.reader(), &budget).unwrap();
    review_await_plan(&mut publication, &fixture);
    drop(publication);
    drop(store);
    let replacement = fixture.base.join("replacement.parquet");
    let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
    crate::encoder::encode_write(
        &mut frame,
        PrivateDirectory::open(&fixture.base)
            .unwrap()
            .create_file("replacement.parquet")
            .unwrap(),
    )
    .unwrap();
    let bytes = fs::read(&replacement).unwrap();
    let inode = fs::metadata(&replacement).unwrap().ino();
    let target = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string())
        .join("pending/events/4095.parquet");
    native_cleanup_replacement_child(&fixture, library, &target, &replacement, second);
    assert_eq!(fs::read(target).unwrap(), bytes);
    assert_eq!(
        fs::metadata(
            fixture
                .root
                .join("data")
                .join(collector::recording::identity_text(transaction).to_string())
                .join("pending/events/4095.parquet")
        )
        .unwrap()
        .ino(),
        inode
    );
    native_loss_counts(&fixture, &format!("cleanup-{captured}-{second}"));
}

fn native_cleanup_replacement_child(
    fixture: &Fixture,
    library: &Path,
    target: &Path,
    replacement: &Path,
    second: bool,
) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::loss_only_tests::",
                "native_cleanup_replacement_before_admission_or_name_recheck_preserves_material"
            ),
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_CLEANUP_BASE", &fixture.base)
        .env("FLIGHTDECK_FD_ROOT", fixture.root.canonicalize().unwrap())
        .env("FLIGHTDECK_FD_LOG", fixture.base.join("fds"))
        .env("FLIGHTDECK_CLEANUP_REPLACEMENT", replacement)
        .env("FLIGHTDECK_MUTATION_TARGET", target.canonicalize().unwrap())
        .env("FLIGHTDECK_MUTATION_KIND", "open")
        .env("FLIGHTDECK_REGISTRATION_ARMED", fixture.base.join("armed"))
        .env(
            "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
            fixture.base.join("observed"),
        );
    if second {
        command.env("FLIGHTDECK_CLEANUP_SECOND_OPEN", "1");
    }
    fs::write(fixture.base.join("observed"), b"").unwrap();
    fs::write(fixture.base.join("fds"), b"").unwrap();
    assert!(OwnedChild(command.spawn().unwrap()).wait().success());
}

#[test]
fn a_new_object_cannot_prove_pre_origin_loss_even_with_its_own_binding() {
    // Goal: correct new-object identity does not turn its declared-origin gap into overwrite.
    // Method: let a real successor report Missed, then require all proof cursors to stay put.
    for captured in [false, true] {
        let mut fixture = proof_fixture(captured, 1);
        review_publish_pair(&mut fixture, 2);
        let old_name = live_successor(&mut fixture);
        let budget = registration_budget();
        let input = exact_input(&fixture);
        assert!(input.binding.publication_first > 1);
        assert!(input.binding.event_first > 1);
        let mut source = normal_proof::SourceProof::new(
            fixture.reader(),
            &input,
            budget.reserve(64 * 1024).unwrap(),
        )
        .unwrap();
        assert!(
            source
                .publications(&input, &[], input.binding.publication_first)
                .is_err()
        );
        assert!(source.events(&input, input.binding.event_first).is_err());
        assert!(source.loss_publication_end(&input).is_err());
        assert_eq!(source.positions(), (1, 1));
        let store = Store::open(&fixture.root).unwrap();
        let before = fixture.slot();
        assert!(
            store
                .register_attempt(&input, &mut fixture.reader(), &budget)
                .is_err()
        );
        assert_eq!(fixture.slot(), before);
        collector::ring::unlink_generation(&old_name).unwrap();
    }
}
