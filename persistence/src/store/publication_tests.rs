use super::*;
use crate::canonical::ReadWork;
use crate::{Limits, Manifest, Observation, Window, WindowIdentity};
use collector::recording::bootstrap::codec::{self, Phase, Slot, SlotKind};

fn encode_original_payload(
    attempt: &RegisteredAttempt<'_>,
    fixture: &Fixture,
    publication: u64,
) -> Manifest {
    let input = attempt.input();
    let AttemptOperation::Ordinary { window, .. } = input.operation else {
        panic!("ordinary");
    };
    let identity = WindowIdentity {
        run: input.run,
        window,
    };
    let mut reader = fixture.reader();
    reader.seek_publication(publication).unwrap();
    let shape = reader.shape();
    let mut tick = crate::tick_buffer(&shape).unwrap();
    let found = reader.read_into(&mut tick).unwrap();
    match found {
        collector::ring::Found::Publication {
            publication: number,
            ..
        } => assert_eq!(number, publication),
        other => panic!("expected publication: {other:?}"),
    }
    let observation = original_observation(publication, &tick);
    let mut staged = Window::new(identity, Limits::from_shape(&shape).unwrap());
    staged.push_tick(&observation, &tick, &[]).unwrap();
    let mut tables = encode_original_tables(attempt, &staged);
    let rows = encode_original_rows(attempt, &mut reader, &tick);
    let index = encode_original_index(attempt, &rows, &tick, identity);
    tables
        .iter_mut()
        .find(|table| table.name == "events")
        .unwrap()
        .rows = index.rows;
    Manifest {
        identity,
        publication_first: publication,
        publication_end: publication + 1,
        events_first: index.first,
        events_end: index.end,
        wall_ns: observation.wall_ns.map(|value| crate::TimeBounds {
            min: value,
            max: value,
        }),
        continuous_ns: observation.continuous_ns.map(|value| crate::TimeBounds {
            min: value,
            max: value,
        }),
        awake_ns: observation.awake_ns.map(|value| crate::TimeBounds {
            min: value,
            max: value,
        }),
        losses: crate::Losses {
            publications: publication - input.losses.publication_start,
            events: index.end - index.first - index.rows,
            unknown: input.losses.unknown.is_some(),
        },
        data_directory: format!(
            "data/{}/ready",
            collector::recording::identity_text(attempt.transaction())
        ),
        tables,
        event_batches: u32::try_from(index.batches.len()).unwrap(),
    }
}
fn encode_original<'store>(
    attempt: RegisteredAttempt<'store>,
    fixture: &mut Fixture,
    publication: u64,
    store: &'store Store,
) -> (Manifest, RegisteredAttempt<'store>) {
    let manifest = encode_original_payload(&attempt, fixture, publication);
    if !fixture.reader().completed() {
        publish_at(&mut fixture.writer, manifest.identity.window + 1);
    }
    let proof = attempt
        .prove_ordinary(&manifest, fixture.reader(), &registration_budget())
        .unwrap();
    drop(finish_proof(proof).unwrap());
    // These retained-authority fixtures deliberately reopen after durable metadata issuance.
    (manifest, store.resume_attempt().unwrap())
}
fn finish_proof(mut proof: NormalProof<'_>) -> io::Result<NormalPublication<'_>> {
    for _ in 0..10000 {
        if proof.step()? == ProofStep::Complete {
            return proof.into_publication();
        }
    }
    panic!("proof continuation bound");
}

fn finish(publication: &mut NormalPublication<'_>) {
    for _ in 0..512 {
        if bounded_publication_step(publication).unwrap() == PublicationStep::Idle {
            return;
        }
    }
    panic!("normal publication continuation bound");
}
fn advance_to_ready(fixture: &Fixture, publication: &mut NormalPublication<'_>) {
    for _ in 0..128 {
        publication.step().unwrap();
        if phase(fixture) == Some(Phase::Ready) {
            return;
        }
    }
    panic!("Ready continuation bound");
}
fn phase(fixture: &Fixture) -> Option<Phase> {
    let bytes = fixture.slot();
    match codec::decode_slot(&bytes, fixture.run.recording, SlotKind::Foreground).unwrap() {
        Slot::Idle => None,
        Slot::Occupied { phase, .. } => Some(phase),
    }
}
#[test]
fn store_normal_ordinary_ready_reopen_without_transport_and_duplicate_application() {
    // Goal: real Store Ready authority survives transport loss and commits one exact raw inode.
    // Method: encode through registered files, pause at Ready, reopen and replay publication.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let transaction = attempt.transaction();
    publish_count(&mut fixture.writer, 1);
    let (manifest, attempt) = encode_original(attempt, &mut fixture, 2, &store);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let ordinary_inode = root
        .open_directory("ordinary")
        .unwrap()
        .device_inode()
        .unwrap();
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    advance_to_ready(&fixture, &mut publication);
    drop(publication);
    drop(store);
    close_source_writer(&mut fixture);
    let store = Store::open(&fixture.root).unwrap();
    let mut publication = store.resume_publication(&registration_budget()).unwrap();
    for _ in 0..256 {
        publication.step().unwrap();
        if phase(&fixture) == Some(Phase::Published) {
            break;
        }
    }
    assert_eq!(phase(&fixture), Some(Phase::Published));
    drop(publication);
    drop(store);
    let store = Store::open(&fixture.root).unwrap();
    let mut publication = store.resume_publication(&registration_budget()).unwrap();
    finish(&mut publication);
    assert_eq!(phase(&fixture), None);
    assert_eq!(
        root.open_directory("ordinary")
            .unwrap()
            .device_inode()
            .unwrap(),
        ordinary_inode
    );
    let frontier = store
        .source_frontier(
            &fixture.run,
            &registration_budget(),
            &mut ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), manifest.publication_end);
    assert_eq!(frontier.event_end(), manifest.events_end);
    assert_eq!(
        frontier.predecessor(),
        &Predecessor::Canonical {
            transaction,
            kind: crate::transaction::CanonicalKind::Ordinary,
            origin: 2
        }
    );
    println!("normal-store-observed {:?}", publication.work());
}
#[test]
fn no_name_of_a_publication_appears_before_its_order_entry() {
    // Goal: physical removal can find everything a reader can list, whenever a crash comes.
    // Method: step an ordinary publication, and after each step compare the proof's links with the
    // order, since a link beyond the proof's own is either the entry or a name.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let transaction = collector::recording::identity_text(attempt.transaction()).to_string();
    publish_count(&mut fixture.writer, 1);
    let (_manifest, attempt) = encode_original(attempt, &mut fixture, 2, &store);
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    advance_to_ready(&fixture, &mut publication);
    let proof = fixture
        .root
        .join("data")
        .join(&transaction)
        .join("ready/metadata");

    for _step in 0..4096 {
        let step = publication.step().unwrap();
        // A proof is finally moved aside when its publication ends, by which point it has an entry.
        if step == PublicationStep::Idle {
            break;
        }
        let links = fs::metadata(&proof).unwrap().nlink();
        let entries = crate::retention::order_entry_names(&fixture.root).len();

        assert!(
            links == 1 || entries == 1,
            "links={links} entries={entries}"
        );
    }

    let entries = crate::retention::order_entry_names(&fixture.root);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].ends_with(&format!("-{transaction}-ready")));
}

#[test]
fn store_normal_terminal_commits_fixed_lifecycle_with_unchanged_event_endpoint() {
    // Goal: terminal registration publishes lifecycle independently of numeric endpoint growth.
    // Method: encode a zero-row fixed terminal, then read its certified canonical frontier.
    let mut fixture = Fixture::new(false);
    fixture.writer.finish();
    let store = Store::open(&fixture.root).unwrap();
    let (input, _) = settle_latest_ordinary(&mut fixture, &store);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    let transaction = collector::recording::identity_text(attempt.transaction()).to_string();
    drop(
        finish_proof(
            attempt
                .prove_terminal(
                    &crate::event_recovery::Recovery {
                        run: fixture.run,
                        events_first: 1,
                        events_end: 1,
                        rows: 0,
                        losses: crate::Losses {
                            publications: 0,
                            events: 0,
                            unknown: false,
                        },
                    },
                    &[],
                    fixture.reader(),
                    &registration_budget(),
                )
                .unwrap(),
        )
        .unwrap(),
    );
    let attempt = store.resume_attempt().unwrap();
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    finish(&mut publication);
    assert!(
        crate::retention::order_entry_names(&fixture.root)
            .iter()
            .any(|name| name.ends_with(&format!("-{transaction}-ready")))
    );
    let frontier = store
        .source_frontier(
            &fixture.run,
            &registration_budget(),
            &mut ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert!(frontier.complete());
    assert_eq!(frontier.event_end(), 1);
}

#[test]
fn native_normal_publication_restores_sync_effects_and_stays_within_namespace_budget() {
    // Goal: actual Store interruption restores Ready, Busy, structure, Quiet and Idle effects.
    // Method: native full-sync interposition returns EIO after a successful syscall, then reopen.
    if let Some(root) = std::env::var_os("FLIGHTDECK_PUBLICATION_ROOT") {
        native_publication_child(Path::new(&root));
        return;
    }
    let compiler = Fixture::new(false);
    let library = native_registration_build(&compiler);
    for boundary in [
        "ready-control",
        "ticks",
        "metadata",
        "index",
        "batch",
        "pending-directory",
        "object",
        "ready-directory",
        "rename-ready",
        "rename-install",
        "link",
        "busy",
        "structure",
        "quiet",
        "idle",
        "split-link",
        "split-rename",
        "split-unlink",
    ] {
        let mut fixture = Fixture::new_profile(false, boundary == "batch");
        let store = Store::open(&fixture.root).unwrap();
        let transaction = native_registered_payload(&mut fixture, &store, boundary);
        drop(store);
        let target = native_publication_target(&fixture, &transaction, boundary);
        let mut command = native_publication_command(&fixture, &library, boundary, &target);
        let mut child = OwnedChild(command.spawn().unwrap());
        let status = child.wait();
        match collector::ring::unlink_generation(fixture.writer.name()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("owned fixture cleanup: {error}"),
        }
        fixture.transport_removed = true;
        assert!(status.success(), "boundary {boundary}");
        native_publication_observed(&fixture, boundary);
    }
}
#[test]
fn native_visible_control_adoption_requires_file_and_parent_sync() {
    // Goal: a visible rename cannot advance publication until both adoption barriers succeed.
    // Method: fail after each control rename, then fail its file or parent sync on retry.
    if let Some(root) = std::env::var_os("FLIGHTDECK_PUBLICATION_ROOT") {
        native_publication_child(Path::new(&root));
        return;
    }

    let compiler = Fixture::new(false);
    let library = native_registration_build(&compiler);
    for phase in ["ready", "busy", "quiet", "idle"] {
        for barrier in ["file", "directory"] {
            let boundary = format!("adopt-{phase}-{barrier}");
            let mut fixture = Fixture::new(false);
            let store = Store::open(&fixture.root).unwrap();
            let transaction = native_registered_payload(&mut fixture, &store, &boundary);
            drop(store);
            let target = native_publication_target(&fixture, &transaction, &boundary);
            let mut command = native_publication_command(&fixture, &library, &boundary, &target);
            let mut child = OwnedChild(command.spawn().unwrap());
            let status = child.wait();
            match collector::ring::unlink_generation(fixture.writer.name()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("owned fixture cleanup: {error}"),
            }
            fixture.transport_removed = true;
            assert!(status.success(), "boundary {boundary}");
            assert_eq!(
                fs::read(fixture.base.join("publication-observed")).unwrap(),
                b"xx"
            );
            native_publication_observed(&fixture, &boundary);
        }
    }
}
fn native_publication_adoption_failure(
    root: &PrivateDirectory,
    publication: &mut NormalPublication<'_>,
) {
    let arm = PathBuf::from(std::env::var_os("FLIGHTDECK_SYNC_ARMED").unwrap());
    let trace = PathBuf::from(std::env::var_os("FLIGHTDECK_PUBLICATION_TRACE").unwrap());
    let mut foreground = [0; 4096];
    let mut namespace = [0; 256];
    root.open_file("foreground")
        .unwrap()
        .read_exact(&mut foreground)
        .unwrap();
    root.open_file("namespace-state")
        .unwrap()
        .read_exact(&mut namespace)
        .unwrap();
    let offset = fs::metadata(&trace).unwrap().len();
    fs::write(&arm, b"armed").unwrap();

    let error = bounded_publication_step(publication).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(5));

    let mut after_foreground = [0; 4096];
    let mut after_namespace = [0; 256];
    root.open_file("foreground")
        .unwrap()
        .read_exact(&mut after_foreground)
        .unwrap();
    root.open_file("namespace-state")
        .unwrap()
        .read_exact(&mut after_namespace)
        .unwrap();
    assert_eq!(after_foreground, foreground);
    assert_eq!(after_namespace, namespace);
    let trace = fs::read(trace).unwrap();
    let operations = &trace[usize::try_from(offset).unwrap()..];
    assert!(!operations.is_empty());
    assert!(operations.iter().all(|operation| *operation == b'S'));
    fs::remove_file(arm).unwrap();
}
fn native_publication_child(path: &Path) {
    let boundary = std::env::var("FLIGHTDECK_PUBLICATION_BOUNDARY").unwrap();
    let armed = PathBuf::from(std::env::var_os("FLIGHTDECK_REGISTRATION_ARMED").unwrap());
    let root = PrivateDirectory::open(path).unwrap();
    let mut bytes = [0; 4096];
    root.open_file("foreground")
        .unwrap()
        .read_exact(&mut bytes)
        .unwrap();
    let roots_before = publication_root_inodes(&root);
    let store = Store::open(path).unwrap();
    let (transaction, input) = AttemptInput::decode_registered(&bytes, store.recording()).unwrap();
    let original = path
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    let metadata = crate::canonical::CanonicalMetadata::read(
        &mut File::open(original.join("pending/metadata")).unwrap(),
    )
    .unwrap();
    let expected_end = metadata.original_end;
    let tick_bytes = fs::read(original.join("pending/ticks.parquet")).unwrap();
    let memory = registration_budget();
    let mut publication = store.resume_publication(&memory).unwrap();
    native_publication_fail(&root, &input, &mut publication, &armed, &boundary);
    if boundary.starts_with("adopt-") {
        native_publication_adoption_failure(&root, &mut publication);
    }
    fs::remove_file(armed).unwrap();
    let already_idle = boundary.starts_with("adopt-idle-");
    if already_idle {
        finish(&mut publication);
    }
    let before_reopen = publication.work();
    drop(publication);
    drop(store);
    collector::ring::unlink_generation(&std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap())
        .unwrap();
    let store = Store::open(path).unwrap();
    let mut publication = if already_idle {
        assert!(store.resume_publication(&memory).is_err());
        None
    } else {
        let mut publication = store.resume_publication(&memory).unwrap();
        finish(&mut publication);
        Some(publication)
    };
    assert_eq!(
        fs::read(original.join("ready/ticks.parquet")).unwrap(),
        tick_bytes
    );
    let frontier = store
        .source_frontier(&input.run, &memory, &mut ReadWork::default())
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), expected_end);
    assert_eq!(publication_root_inodes(&root), roots_before);
    assert_eq!(memory.used() > 0, publication.is_some());
    let footprint = crate::resources::Footprint::read().unwrap();
    assert!(footprint.lifetime_bytes_max <= 256 * 1024 * 1024);
    println!(
        "native publication final work={:?} reservation_peak={} footprint={footprint:?}",
        publication
            .as_ref()
            .map_or(before_reopen, NormalPublication::work),
        memory.used_peak()
    );
    drop(publication.take());
    assert_eq!(memory.used(), 0);
}

fn publish_at(writer: &mut collector::ring::Writer, window: u64) {
    let memory = collector::machine::Memory::default();
    let readings = collector::ring::Readings {
        run_tick: window * 60,
        cpus: &[],
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: collector::ring::Dropped::default(),
    };
    let times = collector::ring::ObservationTimes {
        wall_ns: window * 60 + 1,
        continuous_ns: window * 60_000_000_000 + 1,
        awake_ns: window * 60_000_000_000 + 1,
    };
    if writer.dictionary_usage().unwrap().is_some() {
        writer
            .publish_with_identity(
                &readings,
                Some(&times),
                &collector::ring::IdentityPublication {
                    processes: &[],
                    events: &[],
                },
            )
            .unwrap();
    } else {
        writer.publish_observed(&readings, &times);
    }
}

#[test]
fn store_129th_ordinary_splits_child_and_preserves_root_and_exact_latest_source() {
    // Goal: the real Store route grows beyond128 and keeps the permanent scaffold unchanged.
    // Method: commit129 ascending source windows, observing the registered split on the last.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let root_inode = root
        .open_directory("ordinary")
        .unwrap()
        .device_inode()
        .unwrap();
    let memory = registration_budget();
    let mut saw_split = false;
    let mut maximum_nodes = 0;
    for window in 0..129 {
        let result = publish_counted_window(&mut fixture, &store, window, &memory);
        saw_split |= result.0;
        maximum_nodes = maximum_nodes.max(result.1);
        assert_eq!(
            root.open_directory("ordinary")
                .unwrap()
                .device_inode()
                .unwrap(),
            root_inode
        );
    }
    assert!(saw_split);
    assert!(maximum_nodes <= 4096);
    let frontier = store
        .source_frontier(&fixture.run, &memory, &mut ReadWork::default())
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 130);
    assert_eq!(frontier.prior_window(), Some(128));
    println!(
        "Store129 namespace-node-attempts-per-step-max={maximum_nodes} reservation_peak={}",
        memory.used_peak()
    );
    let mut second = source_for_root(&fixture, Identity([8, 10]));
    second.writer.finish();
    let attempt = store
        .register_attempt(&second.input(false), &mut second.reader(), &memory)
        .unwrap();
    let (_, attempt) = encode_original(attempt, &mut second, 1, &store);
    assert!(finish_observe_wrapper(
        &second,
        &mut attempt.normal_publication(&memory).unwrap()
    ));
    assert_eq!(
        root.open_directory("ordinary")
            .unwrap()
            .device_inode()
            .unwrap(),
        root_inode
    );
}

#[test]
fn origin_zero_survives_transport_loss_and_selected_canonical_window_validation() {
    // Goal: zero is present evidence; canonical reads validate elapsed windows after slot clear.
    // Method: persist origin0, clear authority without transport, then change only the prefix
    // origin.
    let mut fixture = Fixture::new_profile_origin(false, false, 0);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let transaction = attempt.transaction();
    let (_, attempt) = encode_original(attempt, &mut fixture, 1, &store);
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    advance_to_ready(&fixture, &mut publication);
    drop(publication);
    drop(store);
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.transport_removed = true;
    let store = Store::open(&fixture.root).unwrap();
    let mut publication = store.resume_publication(&registration_budget()).unwrap();
    finish(&mut publication);
    assert_eq!(phase(&fixture), None);
    drop(publication);
    assert!(
        store
            .source_frontier(
                &fixture.run,
                &registration_budget(),
                &mut ReadWork::default()
            )
            .unwrap()
            .is_some()
    );
    let metadata = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string())
        .join("ready/metadata");
    let original = fs::read(&metadata).unwrap();
    assert_eq!(&original[192..196], &1u32.to_be_bytes());
    assert_eq!(&original[200..208], &0u64.to_be_bytes());
    let mut changed = original.clone();
    changed[200..208].copy_from_slice(&2u64.to_be_bytes());
    fs::write(&metadata, changed).unwrap();
    assert_eq!(
        store
            .source_frontier(
                &fixture.run,
                &registration_budget(),
                &mut ReadWork::default()
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    fs::write(&metadata, original).unwrap();
}
#[test]
fn retained_ready_refuses_source_slot_prefix_mismatch_without_canonical_mutation() {
    // Goal: independently valid origin evidence cannot replace registered provenance.
    // Method: change only metadata origin after Ready and compare retained slot/root contents.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let transaction = attempt.transaction();
    let (_, attempt) = encode_original(attempt, &mut fixture, 1, &store);
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    for _ in 0..128 {
        publication.step().unwrap();
        if phase(&fixture) == Some(Phase::Ready) {
            break;
        }
    }
    drop(publication);
    drop(store);
    let metadata = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string())
        .join("ready/metadata");
    let mut bytes = fs::read(&metadata).unwrap();
    bytes[200..208].copy_from_slice(&0u64.to_be_bytes());
    fs::write(metadata, bytes).unwrap();
    let before = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let mut publication = store.resume_publication(&registration_budget()).unwrap();
    let mut refused = false;
    for _ in 0..128 {
        if let Err(error) = publication.step() {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert_eq!(fixture.slot(), before);
    PrivateDirectory::open(&fixture.root)
        .unwrap()
        .open_directory("ordinary")
        .unwrap()
        .visit_names(0, |_| panic!("unvalidated Ready became canonical"))
        .unwrap();
}

#[test]
fn captured_ordinary_preserves_actual_binary_program_and_event_bytes_after_slot_clear() {
    // Goal: normal publication retains captured source bytes without lossy UTF8 conversion.
    // Method: publish a native identity-bearing Exec, encode its actual row and compare raw bytes.
    let mut fixture = Fixture::new_profile(false, true);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    publish_captured_exec(&mut fixture);
    let transaction = attempt.transaction();
    let (_, attempt) = encode_original(attempt, &mut fixture, 2, &store);
    let original = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string());
    let encoded = fs::read(original.join("pending/events/0000.parquet")).unwrap();
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    finish(&mut publication);
    assert_eq!(phase(&fixture), None);
    assert_eq!(
        fs::read(original.join("ready/events/0000.parquet")).unwrap(),
        encoded
    );
    let frame = crate::event_recovery::read::read_batch_file(
        PrivateDirectory::open(&original.join("ready/events"))
            .unwrap()
            .open_file("0000.parquet")
            .unwrap(),
        &crate::event_recovery::Batch {
            first: 1,
            end: 2,
            rows: 1,
        },
        true,
        true,
        &registration_budget(),
    )
    .unwrap();
    assert_captured_batch(&frame);
    assert_eq!(
        store
            .source_frontier(
                &fixture.run,
                &registration_budget(),
                &mut ReadWork::default()
            )
            .unwrap()
            .unwrap()
            .event_end(),
        2
    );
}

#[test]
fn acquired_terminal_cannot_close_over_an_unowned_retained_publication() {
    // Goal: source completion never hides the final ordinary reading from later registration.
    // Method: acquire a finished source while its last tick remains unowned and compare authority.
    let mut fixture = Fixture::new(false);
    fixture.writer.finish();
    let store = Store::open(&fixture.root).unwrap();
    let before = fixture.slot();
    let error = store
        .register_attempt(
            &fixture.input(true),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .err()
        .expect("retained final publication must be saved first");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(fixture.slot(), before);
    PrivateDirectory::open(&fixture.root)
        .unwrap()
        .open_directory("recovery-data")
        .unwrap()
        .visit_names(0, |_| panic!("premature terminal artifact"))
        .unwrap();
}

pub(super) fn settle_latest_ordinary(
    fixture: &mut Fixture,
    store: &Store,
) -> (AttemptInput, Manifest) {
    let mut reader = fixture.reader();
    let publication = reader.publication_newest().unwrap();
    reader.seek_publication(publication).unwrap();
    let mut timing = collector::ring::PublicationTiming::default();
    assert!(matches!(
        reader.read_timing_into(&mut timing).unwrap(),
        collector::ring::Found::Publication { .. }
    ));
    let window = crate::worker::timing_window_number(&reader, &timing).unwrap();
    let mut input = fixture.input(false);
    if let Some(frontier) = store
        .source_frontier(
            &fixture.run,
            &registration_budget(),
            &mut ReadWork::default(),
        )
        .unwrap()
    {
        input.losses.predecessor = frontier.predecessor().clone();
        input.losses.publication_start = frontier.publication_end();
        input.losses.event_start = frontier.event_end();
        input.losses.event_inherited_end = frontier.event_end();
        input.losses.prior_window = frontier.prior_window();
    }
    input.operation = AttemptOperation::Ordinary {
        window,
        attempted_origin: publication,
    };
    input.losses.publication_inherited_end = publication;
    input.losses.publications = publication - input.losses.publication_start;
    input.losses.unknown = None;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    let (manifest, attempt) = encode_original(attempt, fixture, publication, store);
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    finish(&mut publication);
    drop(publication);
    (terminal_from_frontier(fixture, store), manifest)
}
fn terminal_from_frontier(fixture: &Fixture, store: &Store) -> AttemptInput {
    let frontier = store
        .source_frontier(
            &fixture.run,
            &registration_budget(),
            &mut ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    let mut terminal = fixture.input(true);
    terminal.operation = AttemptOperation::Terminal {
        event_end: fixture.reader().event_newest().unwrap() + 1,
    };
    terminal.losses.predecessor = frontier.predecessor().clone();
    terminal.losses.publication_start = frontier.publication_end();
    terminal.losses.publication_inherited_end = frontier.publication_end();
    terminal.losses.event_start = frontier.event_end();
    terminal.losses.event_inherited_end = frontier.event_end();
    terminal.losses.prior_window = frontier.prior_window();
    terminal.losses.unknown = None;
    terminal
}

#[path = "publication_reader_tests.rs"]
mod reader_tests;

#[path = "publication_terminal_tests.rs"]
mod terminal_tests;

#[test]
fn a_manifest_cannot_certify_a_still_active_elapsed_window() {
    // Goal: a valid raw index and last row do not prove that an active window ended.
    // Method: encode an actual tick and demand refusal before immutable metadata exists.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let manifest = encode_original_payload(&attempt, &fixture, 1);
    assert_eq!(
        finish_proof(
            attempt
                .prove_ordinary(&manifest, fixture.reader(), &registration_budget())
                .unwrap()
        )
        .err()
        .expect("active window must not seal")
        .kind(),
        io::ErrorKind::WouldBlock
    );
    let attempt = store.resume_attempt().unwrap();
    assert_eq!(
        attempt.open_file("metadata").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    fixture.writer.finish();
    drop(
        finish_proof(
            attempt
                .prove_ordinary(&manifest, fixture.reader(), &registration_budget())
                .unwrap(),
        )
        .unwrap(),
    );
    let attempt = store.resume_attempt().unwrap();
    finish(&mut attempt.normal_publication(&registration_budget()).unwrap());
}

#[test]
fn a_matching_torn_unpublished_metadata_prefix_can_finish_without_rewriting_raw_rows() {
    // Goal: a failed immutable metadata write is retryable only from a proved typed target.
    // Method: retain a matching prefix beside Registered, retry with source proof and compare
    // ticks.
    let mut fixture = Fixture::new(false);
    fixture.writer.finish();
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let manifest = encode_original_payload(&attempt, &fixture, 1);
    drop(
        finish_proof(
            attempt
                .prove_ordinary(&manifest, fixture.reader(), &registration_budget())
                .unwrap(),
        )
        .unwrap(),
    );
    let attempt = store.resume_attempt().unwrap();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let metadata = object.join("pending/metadata");
    let original = fs::read(&metadata).unwrap();
    let ticks = fs::read(object.join("pending/ticks.parquet")).unwrap();
    fs::write(&metadata, &original[..128]).unwrap();
    drop(
        finish_proof(
            attempt
                .prove_ordinary(&manifest, fixture.reader(), &registration_budget())
                .unwrap(),
        )
        .unwrap(),
    );
    let attempt = store.resume_attempt().unwrap();
    assert_eq!(fs::read(&metadata).unwrap(), original);
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    finish(&mut publication);
    assert_eq!(fs::read(object.join("ready/ticks.parquet")).unwrap(), ticks);
}

#[test]
fn retained_ready_and_cleared_gate_survive_erasure_of_both_real_derivatives() {
    // Goal: optional App outputs never become authority for completion or exact raw rows.
    // Method: derive both actual tables, erase them at Ready, remove transport and reopen Store.
    let mut fixture = Fixture::new(false);
    fixture.writer.finish();
    let store = Store::open(&fixture.root).unwrap();
    let input = fixture.input(false);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &registration_budget())
        .unwrap();
    let mut manifest = encode_original_payload(&attempt, &fixture, 1);
    write_derivatives(&attempt, &fixture, &mut manifest);
    drop(
        finish_proof(
            attempt
                .prove_ordinary(&manifest, fixture.reader(), &registration_budget())
                .unwrap(),
        )
        .unwrap(),
    );
    let attempt = store.resume_attempt().unwrap();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    advance_to_ready(&fixture, &mut publication);
    drop(publication);
    drop(store);
    let ticks = fs::read(object.join("ready/ticks.parquet")).unwrap();
    for name in crate::schema::DERIVED_TABLES {
        fs::remove_file(object.join(format!("ready/{name}.parquet"))).unwrap();
    }
    close_source_writer(&mut fixture);
    let store = Store::open(&fixture.root).unwrap();
    finish(&mut store.resume_publication(&registration_budget()).unwrap());
    assert_eq!(phase(&fixture), None);
    assert_eq!(fs::read(object.join("ready/ticks.parquet")).unwrap(), ticks);
    let frontier = store
        .source_frontier(
            &fixture.run,
            &registration_budget(),
            &mut ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 2);
    assert_eq!(frontier.event_end(), 1);
}

fn write_derivatives(attempt: &RegisteredAttempt<'_>, fixture: &Fixture, manifest: &mut Manifest) {
    use crate::worker::Derive;
    let mut reader = fixture.reader();
    let mut tick = crate::tick_buffer(&reader.shape()).unwrap();
    assert!(matches!(
        reader.read_into(&mut tick).unwrap(),
        collector::ring::Found::Publication { .. }
    ));
    let mut window = Window::new(
        manifest.identity,
        Limits::from_shape(&reader.shape()).unwrap(),
    );
    window
        .push_tick(
            &Observation {
                publication: 1,
                wall_ns: tick.times.map(|time| time.wall_ns),
                continuous_ns: tick.times.map(|time| time.continuous_ns),
                awake_ns: tick.times.map(|time| time.awake_ns),
            },
            &tick,
            &[],
        )
        .unwrap();
    let derived = fixture.base.join("derived-fixture");
    private::create_private_dir(&derived).unwrap();
    let tables = crate::sealed_apps::SealedApps::default()
        .derive(&crate::worker::Derivation {
            window: &window,
            pending: &PrivateDirectory::open(&derived).unwrap(),
            saved: None,
        })
        .unwrap();
    for table in &tables {
        let name = format!("{}.parquet", table.name);
        let bytes = fs::read(derived.join(&name)).unwrap();
        attempt
            .create_file(&name)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
    }
    manifest.tables.extend(tables);
}

fn ordinary_input(
    fixture: &Fixture,
    store: &Store,
    window: u64,
    memory: &Arc<crate::resources::ByteBudget>,
) -> AttemptInput {
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.operation = AttemptOperation::Ordinary {
        window,
        attempted_origin: window + 1,
    };
    if window > 0 {
        let frontier = store
            .source_frontier(&fixture.run, memory, &mut ReadWork::default())
            .unwrap()
            .unwrap();
        input.losses.predecessor = frontier.predecessor().clone();
        input.losses.publication_start = frontier.publication_end();
        input.losses.publication_inherited_end = frontier.publication_end();
        input.losses.event_start = frontier.event_end();
        input.losses.event_inherited_end = frontier.event_end();
        input.losses.prior_window = frontier.prior_window();
    }
    input
}

fn native_publication_fail(
    root: &PrivateDirectory,
    input: &AttemptInput,
    publication: &mut NormalPublication<'_>,
    armed: &Path,
    boundary: &str,
) {
    let mut bytes = [0; 4096];
    let mut failed = false;
    for _ in 0..512 {
        root.open_file("foreground")
            .unwrap()
            .read_exact(&mut bytes)
            .unwrap();
        let state =
            crate::transaction::normal::NormalState::decode(&bytes, input.run.recording).unwrap();
        let activate = native_publication_fail_active(&state, boundary);
        if activate && !armed.exists() {
            fs::write(armed, b"armed").unwrap();
        }
        let footprint = crate::resources::Footprint::read().unwrap();
        assert!(footprint.lifetime_bytes_max <= 256 * 1024 * 1024);
        match bounded_publication_step(publication) {
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(5));
                failed = true;
                break;
            }
            Ok(PublicationStep::Continue) => {}
            Ok(PublicationStep::Idle) => panic!("fault boundary not reached"),
        }
    }
    assert!(failed);
}

fn native_publication_fail_active(
    state: &crate::transaction::normal::NormalState,
    boundary: &str,
) -> bool {
    match boundary {
        "split-link" | "split-rename" | "split-unlink" => {
            state.phase == Phase::Publishing
                && state.structure.as_ref().is_some_and(|plan| {
                    plan.kind == 1 && plan.parent.namespace == crate::canonical::Namespace::Ordinary
                })
        }
        "ready-control"
        | "ready-directory"
        | "ticks"
        | "metadata"
        | "index"
        | "pending-directory"
        | "object"
        | "rename-ready"
        | "batch"
        | "adopt-ready-file"
        | "adopt-ready-directory" => true,
        "link" => {
            state.phase == Phase::Publishing
                && state
                    .structure
                    .as_ref()
                    .is_some_and(|plan| plan.parent.namespace == crate::canonical::Namespace::Known)
        }
        "rename-install" => {
            state.phase == Phase::Publishing
                && state.structure.as_ref().is_some_and(|plan| {
                    plan.parent.namespace == crate::canonical::Namespace::Ordinary
                        && plan.stage == 3
                })
        }
        "busy" | "adopt-busy-file" | "adopt-busy-directory" => state.phase == Phase::Ready,
        "structure" => {
            state.phase == Phase::Publishing
                && state.structure.as_ref().is_some_and(|plan| {
                    plan.parent.namespace == crate::canonical::Namespace::Ordinary
                })
        }
        "quiet" | "adopt-quiet-file" | "adopt-quiet-directory" => state.phase == Phase::Published,
        "idle" | "adopt-idle-file" | "adopt-idle-directory" => state.phase == Phase::Cleanup,
        _ => panic!("boundary"),
    }
}

fn encode_original_tables(
    attempt: &RegisteredAttempt<'_>,
    staged: &Window,
) -> Vec<crate::TableManifest> {
    let mut tables = Vec::new();
    for table in crate::schema::TABLES {
        if matches!(table, crate::schema::Table::Events) {
            tables.push(crate::TableManifest {
                name: "events",
                rows: 0,
            });
            continue;
        }
        let mut frame = crate::frame(table, staged).unwrap();
        tables.push(crate::TableManifest {
            name: table.name(),
            rows: collector::u64_from_usize(frame.height()),
        });
        crate::encoder::encode_write(
            &mut frame,
            attempt
                .create_file(&format!("{}.parquet", table.name()))
                .unwrap(),
        )
        .unwrap();
    }
    tables
}

fn encode_original_rows(
    attempt: &RegisteredAttempt<'_>,
    reader: &mut RingReader,
    tick: &collector::ring::Tick,
) -> Vec<crate::event_recovery::batch::Row> {
    let input = attempt.input();
    let shape = reader.shape();
    let publication = reader.next_publication() - 1;
    let mut rows = Vec::with_capacity(usize::try_from(shape.event_capacity).unwrap());
    reader.seek_event(input.losses.event_start).unwrap();
    for _ in 0..shape.event_capacity {
        if reader.next_event() >= tick.events.end {
            break;
        }
        let mut identity = collector::ring::IdentityObservation::EMPTY;
        let found = if input.captured {
            reader.read_event_identity(&mut identity).unwrap()
        } else {
            reader.read_event().unwrap()
        };
        match found {
            collector::ring::FoundEvent::Event {
                number,
                tick: run_tick,
                event,
            } => {
                let mut row = crate::event_recovery::batch::copy_row(
                    reader,
                    &crate::NumberedEvent {
                        number,
                        tick: run_tick,
                        event: &event,
                    },
                    input.captured.then_some(&identity),
                )
                .unwrap();
                row.publication = if number >= tick.events.start {
                    Some(publication)
                } else {
                    None
                };
                rows.push(row);
            }
            collector::ring::FoundEvent::Missed { .. } => {}
            collector::ring::FoundEvent::Nothing => break,
        }
    }
    rows
}

fn encode_original_index(
    attempt: &RegisteredAttempt<'_>,
    rows: &[crate::event_recovery::batch::Row],
    tick: &collector::ring::Tick,
    identity: WindowIdentity,
) -> crate::event_table::index::Index {
    let input = attempt.input();
    let batches = if rows.is_empty() {
        vec![]
    } else {
        let mut frame = crate::event_recovery::batch::frame(rows, input.captured).unwrap();
        crate::encoder::encode_write(&mut frame, attempt.create_event_batch(0).unwrap()).unwrap();
        vec![crate::event_recovery::Batch {
            first: rows[0].event.number,
            end: rows.last().unwrap().event.number + 1,
            rows: collector::u64_from_usize(rows.len()),
        }]
    };
    let index = crate::event_table::index::Index {
        identity,
        first: input.losses.event_start,
        end: tick.events.end,
        rows: collector::u64_from_usize(rows.len()),
        identities: input.captured,
        batches,
    };
    index
        .write_file(&mut attempt.create_event_index().unwrap())
        .unwrap();
    index
}

fn original_observation(publication: u64, tick: &collector::ring::Tick) -> Observation {
    Observation {
        publication,
        wall_ns: tick.times.map(|times| times.wall_ns),
        continuous_ns: tick.times.map(|times| times.continuous_ns),
        awake_ns: tick.times.map(|times| times.awake_ns),
    }
}

fn source_for_root(anchor: &Fixture, run: Identity) -> Fixture {
    let mut fixture = Fixture::new(false);
    let identity = RunIdentity {
        recording: anchor.run.recording,
        run,
        boot: anchor.run.boot,
    };
    let place = collector::ring::Place {
        lock_path: fixture.base.join("source.lock"),
        name_prefix: format!(
            "/fds{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let mut writer =
        collector::ring::Writer::create_recording(&place, fixture.reader().shape(), &identity, 1)
            .unwrap();
    publish_count(&mut writer, 1);
    let old_name = fixture.writer.name().to_owned();
    fixture.writer = writer;
    fixture.binding = fixture.reader().object_binding().unwrap();
    collector::ring::unlink_generation(&old_name).unwrap();
    fixture.place = place;
    fixture.root = anchor.root.clone();
    fixture.run = identity;
    fixture.source = crate::transaction::SourceTiming::from_reader(&fixture.reader());
    fixture
}
fn finish_observe_wrapper(fixture: &Fixture, publication: &mut NormalPublication<'_>) -> bool {
    let mut wrapped = false;
    for _ in 0..512 {
        if publication.step().unwrap() == PublicationStep::Idle {
            return wrapped;
        }
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if let Some(plan) = state.structure {
            if plan.parent.namespace == crate::canonical::Namespace::Ordinary
                && plan.kind == 2
                && plan.inode != 0
            {
                assert!(plan.replacement.nibbles > 0);
                assert!(plan.replacement.nibbles < plan.old.nibbles);
                wrapped = true;
            }
        }
    }
    panic!("wrapper continuation bound");
}
#[test]
fn store_first_children_route_all_sixteen_nibbles_and_reopen_the_permanent_root() {
    // Goal: actual Store publication fills every permanent-root route without a root rename.
    // Method: use sixteen actual identified producers, close their transports and reopen Store.
    let fixture = Fixture::new(false);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let original = root
        .open_directory("ordinary")
        .unwrap()
        .device_inode()
        .unwrap();
    for digit in 0..16 {
        let mut source = source_for_root(&fixture, Identity([digit << 60, 101]));
        source.writer.finish();
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(
                &source.input(false),
                &mut source.reader(),
                &registration_budget(),
            )
            .unwrap();
        let (_, attempt) = encode_original(attempt, &mut source, 1, &store);
        finish(&mut attempt.normal_publication(&registration_budget()).unwrap());
        assert_eq!(
            root.open_directory("ordinary")
                .unwrap()
                .device_inode()
                .unwrap(),
            original
        );
        drop(store);
        let store = Store::open(&fixture.root).unwrap();
        assert_eq!(
            store
                .source_frontier(
                    &source.run,
                    &registration_budget(),
                    &mut ReadWork::default()
                )
                .unwrap()
                .unwrap()
                .publication_end(),
            2
        );
    }
    let mut count = 0;
    root.open_directory("ordinary")
        .unwrap()
        .visit_names(16, |name| {
            assert_eq!(name.len(), 1);
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 16);
}

fn native_registered_payload(fixture: &mut Fixture, store: &Store, boundary: &str) -> String {
    let memory = registration_budget();
    let count = if boundary.starts_with("split-") {
        128
    } else {
        0
    };
    for window in 0..count {
        let input = ordinary_input(fixture, store, window, &memory);
        let attempt = store
            .register_attempt(&input, &mut fixture.reader(), &memory)
            .unwrap();
        let (_, attempt) = encode_original(attempt, fixture, window + 1, store);
        finish(&mut attempt.normal_publication(&memory).unwrap());
    }
    let input = ordinary_input(fixture, store, count, &memory);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &memory)
        .unwrap();
    if boundary == "batch" {
        publish_captured_exec(fixture);
    }
    let transaction = collector::recording::identity_text(attempt.transaction()).to_string();
    let (_, attempt) = encode_original(
        attempt,
        fixture,
        if boundary == "batch" { 2 } else { count + 1 },
        store,
    );
    drop(attempt);
    transaction
}

fn native_publication_target(fixture: &Fixture, transaction: &str, boundary: &str) -> PathBuf {
    let root = fixture.root.canonicalize().unwrap();
    let object_path = root.join("data").join(transaction);
    let pending_path = object_path.join("pending");
    match boundary {
        "split-unlink" | "split-link" | "split-rename" => {
            let key = crate::canonical::CanonicalKey::source(
                crate::canonical::Namespace::Ordinary,
                &fixture.run,
                1,
            )
            .unwrap()
            .file_name();
            let prefix = format!("txn-{transaction}");
            match boundary {
                "split-unlink" => root
                    .join("ordinary")
                    .join(format!("{prefix}-backup"))
                    .join(key),
                "split-rename" => root.join("ordinary").join(format!("{prefix}-backup")),
                _ => root
                    .join("ordinary")
                    .join(format!("{prefix}-build"))
                    .join("0")
                    .join(key),
            }
        }
        "adopt-busy-file"
        | "adopt-busy-directory"
        | "adopt-quiet-file"
        | "adopt-quiet-directory" => root.join("namespace-state"),
        "adopt-ready-file"
        | "adopt-ready-directory"
        | "adopt-idle-file"
        | "adopt-idle-directory" => root.join("foreground"),
        "ticks" => pending_path.join("ticks.parquet"),
        "metadata" => pending_path.join("metadata"),
        "index" => pending_path.join("events/index"),
        "batch" => pending_path.join("events/0000.parquet"),
        "pending-directory" => pending_path.clone(),
        "object" => object_path.clone(),
        "ready-directory" | "rename-ready" => object_path.join("ready"),
        "busy" | "quiet" => root.join("namespace-state.pending"),
        "structure" => root.join("ordinary"),
        "rename-install" => root.join("ordinary/0"),
        "link" => {
            let directory = PrivateDirectory::open(&pending_path).unwrap();
            let metadata = crate::canonical::CanonicalMetadata::read(
                &mut directory.open_file("metadata").unwrap(),
            )
            .unwrap();
            let keys = crate::canonical::rows::observation_keys(
                &directory,
                &metadata,
                &registration_budget(),
                &mut ReadWork::default(),
                None,
            )
            .unwrap();
            root.join(keys[0].0.name())
                .join(format!("txn-{transaction}-build"))
                .join(keys[0].1.file_name())
        }
        _ => root.join("foreground.pending"),
    }
}

fn native_publication_command(
    fixture: &Fixture,
    library: &Path,
    boundary: &str,
    target: &Path,
) -> std::process::Command {
    let root = fixture.root.canonicalize().unwrap();
    let counts = fixture.base.join("publication-fds");
    fs::write(&counts, b"").unwrap();
    let observed = fixture.base.join("publication-observed");
    fs::write(&observed, b"").unwrap();
    let armed = fixture.base.join("publication-armed");
    let trace = fixture.base.join("publication-trace");
    fs::write(&trace, b"").unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            if boundary.starts_with("adopt-") {
                concat!(
                    "store::registration_tests::publication_tests::",
                    "native_visible_control_adoption_requires_file_and_parent_sync"
                )
            } else {
                concat!(
                    "store::registration_tests::publication_tests::",
                    "native_normal_publication_restores_sync_effects_",
                    "and_stays_within_namespace_budget"
                )
            },
            "--nocapture",
        ])
        .env("DYLD_INSERT_LIBRARIES", library)
        .env("FLIGHTDECK_PUBLICATION_ROOT", &root)
        .env("FLIGHTDECK_PUBLICATION_BOUNDARY", boundary)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env("FLIGHTDECK_REGISTRATION_ARMED", &armed)
        .env("FLIGHTDECK_PRIVATE_SYNC_OBSERVED", &observed)
        .env("FLIGHTDECK_FD_ROOT", &root)
        .env("FLIGHTDECK_FD_LOG", &counts)
        .env("FLIGHTDECK_PUBLICATION_TRACE", &trace);
    native_publication_command_faults(&mut command, fixture, boundary, target, &root);
    command
}

fn native_publication_command_faults(
    command: &mut std::process::Command,
    fixture: &Fixture,
    boundary: &str,
    target: &Path,
    root: &Path,
) {
    let mutation = match boundary {
        "rename-ready" | "rename-install" | "split-rename" => Some("rename"),
        "link" | "split-link" => Some("link"),
        "split-unlink" => Some("unlink"),
        _ => None,
    };
    if boundary.starts_with("adopt-") {
        command
            .env("FLIGHTDECK_MUTATION_TARGET", target)
            .env("FLIGHTDECK_MUTATION_KIND", "rename")
            .env("FLIGHTDECK_SYNC_ARMED", fixture.base.join("adoption-armed"))
            .env(
                "FLIGHTDECK_PRIVATE_SYNC_TARGET",
                if boundary.ends_with("directory") {
                    root
                } else {
                    target
                },
            );
    } else if let Some(kind) = mutation {
        command
            .env("FLIGHTDECK_MUTATION_TARGET", target)
            .env("FLIGHTDECK_MUTATION_KIND", kind);
    } else {
        command
            .env("FLIGHTDECK_PRIVATE_SYNC_TARGET", target)
            .env("FLIGHTDECK_SYNC_EFFECT", "1");
    }
}

fn native_publication_observed(fixture: &Fixture, boundary: &str) {
    let counts = fixture.base.join("publication-fds");
    let trace = fixture.base.join("publication-trace");
    let observed = fixture.base.join("publication-observed");
    if let Some(evidence) = std::env::var_os("FLIGHTDECK_PUBLICATION_EVIDENCE") {
        let evidence = PathBuf::from(evidence);
        fs::create_dir_all(&evidence).unwrap();
        fs::copy(&counts, evidence.join(format!("{boundary}-fds.bin"))).unwrap();
        fs::copy(&trace, evidence.join(format!("{boundary}-syscalls.bin"))).unwrap();
        fs::copy(&observed, evidence.join(format!("{boundary}-fault.bin"))).unwrap();
    }
    assert!(!fs::read(observed).unwrap().is_empty());
    let operations = fs::read(&trace).unwrap();
    let mut counts_by_byte = [0_u64; 256];
    for operation in operations {
        counts_by_byte[usize::from(operation)] += 1;
    }
    println!(
        "native-operations boundary={boundary} fsync={} link={} rename={} unlink={}",
        counts_by_byte[usize::from(b'S')],
        counts_by_byte[usize::from(b'L')],
        counts_by_byte[usize::from(b'R')],
        counts_by_byte[usize::from(b'U')]
    );
    let bytes = fs::read(&counts).unwrap();
    assert_eq!(bytes.len() % 4, 0);
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
    assert!(namespace_peak <= 8, "{boundary}: {namespace_peak}");
    println!(
        concat!(
            "publication-native boundary={} namespace_peak={} ",
            "payload_gate_peak={} samples={}"
        ),
        boundary,
        namespace_peak,
        raw_peak,
        bytes.len() / 4
    );
}

fn publish_captured_exec(fixture: &mut Fixture) {
    let mut reference = None;
    fixture
        .writer
        .intern_program(
            &collector::ring::ProgramBytes {
                executable_name: b"program-\xff",
                app_path: Some(b"/Applications/Binary-\xfe.app"),
            },
            &mut reference,
        )
        .unwrap();
    let identity = collector::ring::IdentityObservation {
        pid: 41,
        usage: collector::ring::UsageAvailability::NotApplicable,
        identity: collector::ring::ProcessIdentity::Measured {
            creation: collector::ring::ProcessCreation {
                unique_id: 991,
                pid_version: 7,
            },
            program: reference.unwrap(),
        },
    };
    let memory = collector::machine::Memory::default();
    fixture
        .writer
        .publish_with_identity(
            &collector::ring::Readings {
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
            },
            Some(&collector::ring::ObservationTimes {
                wall_ns: 2,
                continuous_ns: 2,
                awake_ns: 2,
            }),
            &collector::ring::IdentityPublication {
                processes: &[],
                events: &[identity],
            },
        )
        .unwrap();
}

fn publish_counted_window(
    fixture: &mut Fixture,
    store: &Store,
    window: u64,
    memory: &Arc<crate::resources::ByteBudget>,
) -> (bool, u64) {
    let mut saw_split = false;
    let mut maximum_nodes = 0;
    let input = ordinary_input(fixture, store, window, memory);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), memory)
        .unwrap();
    let (_, attempt) = encode_original(attempt, fixture, window + 1, store);
    let mut publication = attempt.normal_publication(memory).unwrap();
    let mut done = false;
    for _ in 0..512 {
        let before = publication.work().canonical.node_attempts;
        let step = publication.step().unwrap();
        maximum_nodes = maximum_nodes.max(publication.work().canonical.node_attempts - before);
        if step == PublicationStep::Idle {
            done = true;
            break;
        }
        let slot = fixture.slot();
        let state =
            crate::transaction::normal::NormalState::decode(&slot, fixture.run.recording).unwrap();
        if let Some(plan) = state.structure {
            if plan.kind == 1 && plan.parent.namespace == crate::canonical::Namespace::Ordinary {
                assert_eq!(plan.copied, 128);
                assert!(plan.replacement.nibbles > 0);
                saw_split = true;
            }
        }
    }
    assert!(done);
    (saw_split, maximum_nodes)
}

fn assert_captured_batch(frame: &polars::prelude::DataFrame) {
    assert_eq!(
        frame
            .column("executable_name")
            .unwrap()
            .binary()
            .unwrap()
            .get(0),
        Some(b"program-\xff".as_slice())
    );
    assert_eq!(
        frame.column("app_path").unwrap().binary().unwrap().get(0),
        Some(b"/Applications/Binary-\xfe.app".as_slice())
    );
    assert_eq!(
        frame.column("publication").unwrap().u64().unwrap().get(0),
        Some(2)
    );
}

#[test]
fn publishing_hints_cannot_skip_observations_or_duplicate_canonical_ownership() {
    // Goal: a stale or ahead publication cursor remains a hint after restart.
    // Method: replace only the retained hint before any links, then check exact link count/end.
    use std::os::unix::fs::MetadataExt;
    for hint in [0, 61] {
        let mut fixture = Fixture::new(false);
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(
                &fixture.input(false),
                &mut fixture.reader(),
                &registration_budget(),
            )
            .unwrap();
        let transaction = attempt.transaction();
        let (_, attempt) = encode_original(attempt, &mut fixture, 1, &store);
        let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
        advance_to_ready(&fixture, &mut publication);
        for _ in 0..128 {
            publication.step().unwrap();
            if phase(&fixture) == Some(Phase::Publishing) {
                break;
            }
        }
        assert_eq!(phase(&fixture), Some(Phase::Publishing));
        drop(publication);
        let mut state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        assert!(state.structure.is_none());
        state.next = hint;
        let mut bytes = [0; 4096];
        state.encode(&mut bytes).unwrap();
        fs::write(fixture.root.join("foreground"), bytes).unwrap();
        finish(&mut store.resume_publication(&registration_budget()).unwrap());
        let metadata = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(transaction).to_string())
            .join("ready/metadata");
        // Its raw name, its one reading's name, its window's name and its retention order entry.
        assert_eq!(fs::metadata(metadata).unwrap().nlink(), 4);
        assert_eq!(
            store
                .source_frontier(
                    &fixture.run,
                    &registration_budget(),
                    &mut ReadWork::default()
                )
                .unwrap()
                .unwrap()
                .publication_end(),
            2
        );
    }
}
#[test]
fn torn_metadata_refuses_foreign_bytes_and_shared_inodes_without_discarding_evidence() {
    // Goal: an interrupted metadata retry cannot delete contradictory or canonically shared data.
    // Method: alter a retained prefix or add another hardlink and compare bytes/slot after refusal.
    for linked in [false, true] {
        let mut fixture = Fixture::new(false);
        fixture.writer.finish();
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(
                &fixture.input(false),
                &mut fixture.reader(),
                &registration_budget(),
            )
            .unwrap();
        let manifest = encode_original_payload(&attempt, &fixture, 1);
        drop(
            finish_proof(
                attempt
                    .prove_ordinary(&manifest, fixture.reader(), &registration_budget())
                    .unwrap(),
            )
            .unwrap(),
        );
        let attempt = store.resume_attempt().unwrap();
        let path = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string())
            .join("pending/metadata");
        let mut bytes = fs::read(&path).unwrap()[..128].to_vec();
        if linked {
            fs::hard_link(&path, fixture.base.join("shared-metadata")).unwrap();
        } else {
            bytes[0] ^= 1;
        }
        fs::write(&path, &bytes).unwrap();
        let before = fixture.slot();
        assert_eq!(
            finish_proof(
                attempt
                    .prove_ordinary(&manifest, fixture.reader(), &registration_budget())
                    .unwrap()
            )
            .err()
            .unwrap()
            .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fixture.slot(), before);
    }
}
#[test]
fn foreign_registered_build_refuses_without_removing_the_name_or_clearing_busy() {
    // Goal: a registered scratch name does not authorize arbitrary content deletion.
    // Method: interrupt before first prepare, inject an unknown file and resume actual Store.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let (_, attempt) = encode_original(attempt, &mut fixture, 1, &store);
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    let mut planned = None;
    for _ in 0..128 {
        publication.step().unwrap();
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if let Some(plan) = state.structure {
            planned = Some((state.transaction, plan));
            break;
        }
    }
    let (transaction, plan) = planned.unwrap();
    assert_eq!(plan.stage, 1);
    assert_eq!(plan.parent.nibbles, 0);
    drop(publication);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let parent = root.open_directory(plan.parent.namespace.name()).unwrap();
    let name = format!(
        "txn-{}-build",
        collector::recording::identity_text(transaction)
    );
    let build = parent.create_directory(&name).unwrap();
    build
        .create_file("foreign")
        .unwrap()
        .write_all(b"evidence")
        .unwrap();
    let before = fixture.slot();
    let mut resumed = store.resume_publication(&registration_budget()).unwrap();
    let mut refused = false;
    for _ in 0..128 {
        if let Err(error) = resumed.step() {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert_eq!(fixture.slot(), before);
    assert_eq!(
        fs::read(
            fixture
                .root
                .join(plan.parent.namespace.name())
                .join(name)
                .join("foreign")
        )
        .unwrap(),
        b"evidence"
    );
    assert!(matches!(
        publication_control::read_state(&root, fixture.run.recording).unwrap(),
        codec::NamespaceState::Busy { .. }
    ));
}

fn publication_root_inodes(root: &PrivateDirectory) -> [(u64, u64); 5] {
    use crate::canonical::Namespace;
    [
        Namespace::Ordinary,
        Namespace::Known,
        Namespace::Unknown,
        Namespace::Terminal,
        Namespace::Fragments,
    ]
    .map(|namespace| {
        root.open_directory(namespace.name())
            .unwrap()
            .device_inode()
            .unwrap()
    })
}
fn bounded_publication_step(
    publication: &mut NormalPublication<'_>,
) -> io::Result<PublicationStep> {
    let before = publication.work();
    let result = publication.step();
    let after = publication.work();
    let nodes = after.canonical.node_attempts - before.canonical.node_attempts;
    let entries = after.canonical.entries - before.canonical.entries;
    let native = after.canonical.native_entries - before.canonical.native_entries;
    assert!(nodes <= 4096);
    assert!(entries <= 2112);
    assert!(native <= 4224);
    if nodes > 32 || result.is_err() {
        println!(
            "publication-step nodes={nodes} entries={entries} native_entries={native} error={:?}",
            result.as_ref().err()
        );
    }
    result
}

fn close_source_writer(fixture: &mut Fixture) {
    // Keep the fixture's concrete Writer field usable for Drop, with an unrelated unnamed ring.
    // The admitted original generation loses its writer, mapping and transport name here.
    let name = fixture.writer.name().to_owned();
    let place = collector::ring::Place {
        lock_path: fixture.base.join("closed-source-placeholder.lock"),
        name_prefix: format!(
            "/fdz{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let placeholder = collector::ring::Writer::create_recording(
        &place,
        fixture.reader().shape(),
        &fixture.run,
        0,
    )
    .unwrap();
    collector::ring::unlink_generation(placeholder.name()).unwrap();
    fixture.writer = placeholder;
    collector::ring::unlink_generation(&name).unwrap();
    fixture.transport_removed = true;
    assert_eq!(
        RingReader::open_generation(&name).err().unwrap().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn ordinary_routes_sixty_one_actual_clocks_and_keeps_partial_publication_busy() {
    // Goal: each actual clock selects its route and Quiet never exposes a partial minute.
    // Method: retain61 mixed-clock ticks, publish through Store and count exact immutable links.
    use std::os::unix::fs::MetadataExt;
    let mut fixture = Fixture::new(false);
    let manifest_window = sixty_one_source_window(&mut fixture);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let mut manifest = encode_original_payload_from_window(&attempt, &manifest_window);
    manifest.losses.unknown = true;
    drop(
        finish_proof(
            attempt
                .prove_ordinary(&manifest, fixture.reader(), &registration_budget())
                .unwrap(),
        )
        .unwrap(),
    );
    let attempt = store.resume_attempt().unwrap();
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let mut done = false;
    for _ in 0..512 {
        if bounded_publication_step(&mut publication).unwrap() == PublicationStep::Idle {
            done = true;
            break;
        }
        if phase(&fixture) == Some(Phase::Publishing) {
            assert!(matches!(
                publication_control::read_state(&root, fixture.run.recording).unwrap(),
                codec::NamespaceState::Busy { .. }
            ));
        }
    }
    assert!(done);
    assert_eq!(
        fs::metadata(object.join("ready/metadata")).unwrap().nlink(),
        64
    );
    let metadata = fs::metadata(object.join("ready/metadata")).unwrap();
    for namespace in ["observations-known", "observations-unknown"] {
        assert_eq!(
            observation_link_names(&root, namespace, (metadata.dev(), metadata.ino())),
            expected_observation_names(&manifest_window, namespace),
        );
    }
    assert_eq!(
        store
            .source_frontier(
                &fixture.run,
                &registration_budget(),
                &mut ReadWork::default()
            )
            .unwrap()
            .unwrap()
            .publication_end(),
        62
    );
}
fn sixty_one_source_window(fixture: &mut Fixture) -> Window {
    let place = collector::ring::Place {
        lock_path: fixture.base.join("many.lock"),
        name_prefix: format!(
            "/fdm{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let mut shape = fixture.reader().shape();
    shape.slot_count = 64;
    let writer = collector::ring::Writer::create_recording(&place, shape, &fixture.run, 1).unwrap();
    let old = fixture.writer.name().to_owned();
    fixture.writer = writer;
    fixture.binding = fixture.reader().object_binding().unwrap();
    collector::ring::unlink_generation(&old).unwrap();
    fixture.place = place;
    for number in 0..61 {
        publish_mixed_clock(&mut fixture.writer, number);
    }
    fixture.writer.finish();
    let mut window = Window::new(
        WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        Limits::from_shape(&shape).unwrap(),
    );
    let mut reader = fixture.reader();
    let mut tick = crate::tick_buffer(&shape).unwrap();
    for publication in 1..=61 {
        assert!(matches!(
            reader.read_into(&mut tick).unwrap(),
            collector::ring::Found::Publication { .. }
        ));
        window
            .push_tick(&original_observation(publication, &tick), &tick, &[])
            .unwrap();
    }
    window
}
fn publish_mixed_clock(writer: &mut collector::ring::Writer, number: u64) {
    let memory = collector::machine::Memory::default();
    let readings = collector::ring::Readings {
        run_tick: number,
        cpus: &[],
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: collector::ring::Dropped::default(),
    };
    let elapsed = number * 900_000_000;
    let times = collector::ring::ObservationTimes {
        wall_ns: elapsed + (number % 4) * 3_600_000_000_000,
        continuous_ns: elapsed + 1,
        awake_ns: elapsed + 1,
    };
    if number.is_multiple_of(2) {
        writer.publish_observed(&readings, &times);
    } else {
        writer.publish(&readings);
    }
}
fn encode_original_payload_from_window(
    attempt: &RegisteredAttempt<'_>,
    window: &Window,
) -> Manifest {
    let mut tables = encode_original_tables(attempt, window);
    let index = crate::event_table::index::Index {
        identity: window.identity,
        first: 1,
        end: 1,
        rows: 0,
        identities: false,
        batches: vec![],
    };
    index
        .write_file(&mut attempt.create_event_index().unwrap())
        .unwrap();
    tables
        .iter_mut()
        .find(|table| table.name == "events")
        .unwrap()
        .rows = 0;
    let mut manifest = crate::worker::seal_manifest(window, tables, crate::Losses::default(), 0);
    manifest.data_directory = format!(
        "data/{}/ready",
        collector::recording::identity_text(attempt.transaction())
    );
    manifest
}
fn observation_link_names(
    root: &PrivateDirectory,
    namespace: &str,
    metadata_identity: (u64, u64),
) -> std::collections::BTreeSet<String> {
    use std::os::unix::fs::MetadataExt;
    let directory = root.open_directory(namespace).unwrap();
    let mut names = std::collections::BTreeSet::new();
    directory
        .visit_names(16, |name| {
            let leaf = directory.open_directory(name)?;
            leaf.visit_names(128, |name| {
                let metadata = leaf.open_file(name)?.metadata()?;
                assert_eq!((metadata.dev(), metadata.ino()), metadata_identity);
                assert!(names.insert(name.to_owned()));
                Ok(())
            })
        })
        .unwrap();
    names
}
fn expected_observation_names(
    window: &Window,
    namespace: &str,
) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for tick in window.observations() {
        if tick.wall_ns.is_some() != (namespace == "observations-known") {
            continue;
        }
        let mut bytes = Vec::with_capacity(60);
        if let Some(clock) = tick.wall_ns {
            bytes.extend_from_slice(
                &u32::try_from(clock / 3_600_000_000_000)
                    .unwrap()
                    .to_be_bytes(),
            );
            bytes.extend_from_slice(&(clock % 3_600_000_000_000).to_be_bytes());
        }
        for identity in [window.identity.run.run, window.identity.run.boot] {
            for word in identity.0 {
                bytes.extend_from_slice(&word.to_be_bytes());
            }
        }
        bytes.extend_from_slice(&tick.publication.to_be_bytes());
        bytes.extend_from_slice(&0_u64.to_be_bytes());
        let mut name = String::with_capacity(125);
        for byte in bytes {
            name.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            name.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
        }
        name.push_str(".seal");
        assert!(names.insert(name));
    }
    names
}

#[test]
fn ready_adopts_busy_effect_with_exact_old_quiet_scratch_and_refuses_a_foreign_sequence() {
    // Goal: a matching Busy effect tolerates only its exact prior Quiet scratch.
    // Method: retain Ready, complete Busy, restore the old slot and inject typed scratch bytes.
    for foreign in [false, true] {
        let mut fixture = Fixture::new(false);
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(
                &fixture.input(false),
                &mut fixture.reader(),
                &registration_budget(),
            )
            .unwrap();
        let (_, attempt) = encode_original(attempt, &mut fixture, 1, &store);
        let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
        advance_to_ready(&fixture, &mut publication);
        let ready = fixture.slot();
        let quiet = fs::read(fixture.root.join("namespace-state")).unwrap();
        for _ in 0..128 {
            publication.step().unwrap();
            if phase(&fixture) == Some(Phase::Publishing) {
                break;
            }
        }
        assert_eq!(phase(&fixture), Some(Phase::Publishing));
        drop(publication);
        fs::write(fixture.root.join("foreground"), ready).unwrap();
        let mut scratch = quiet;
        if foreign {
            scratch[39] ^= 1;
        }
        PrivateDirectory::open(&fixture.root)
            .unwrap()
            .create_file("namespace-state.pending")
            .unwrap()
            .write_all(&scratch)
            .unwrap();
        let mut publication = store.resume_publication(&registration_budget()).unwrap();
        if foreign {
            let mut refused = false;
            for _ in 0..128 {
                if publication.step().is_err() {
                    refused = true;
                    break;
                }
            }
            assert!(refused);
            assert_eq!(
                fs::read(fixture.root.join("namespace-state.pending")).unwrap(),
                scratch
            );
        } else {
            finish(&mut publication);
            assert_eq!(phase(&fixture), None);
        }
    }
}

#[test]
fn restarting_the_final_gate_plan_proves_prior_observation_links_before_mutation() {
    // Goal: occupied final-gate intent cannot stand in for durable observation ownership.
    // Method: pause at its registered plan, remove an observation, reopen and compare gate absence.
    let mut fixture = Fixture::new(false);
    let store = Store::open(&fixture.root).unwrap();
    let attempt = store
        .register_attempt(
            &fixture.input(false),
            &mut fixture.reader(),
            &registration_budget(),
        )
        .unwrap();
    let transaction = attempt.transaction();
    let (_, attempt) = encode_original(attempt, &mut fixture, 1, &store);
    let mut publication = attempt.normal_publication(&registration_budget()).unwrap();
    advance_to_gate_plan(&fixture, &mut publication);
    drop(publication);
    let ready = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string())
        .join("ready");
    let directory = PrivateDirectory::open(&ready).unwrap();
    let metadata =
        crate::canonical::CanonicalMetadata::read(&mut directory.open_file("metadata").unwrap())
            .unwrap();
    let raw_bytes = fs::read(ready.join("metadata")).unwrap();
    let keys = crate::canonical::rows::observation_keys(
        &directory,
        &metadata,
        &registration_budget(),
        &mut ReadWork::default(),
        None,
    )
    .unwrap();
    let (namespace, key) = keys[0];
    let name = key.file_name();
    let observation = fixture
        .root
        .join(namespace.name())
        .join(&name[..1])
        .join(&name);
    fs::remove_file(&observation).unwrap();
    let mut resumed = store.resume_publication(&registration_budget()).unwrap();
    let mut refused = false;
    for _ in 0..128 {
        if resumed.step().is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert_eq!(
        fs::read_dir(fixture.root.join("ordinary")).unwrap().count(),
        0
    );
    assert_eq!(phase(&fixture), Some(Phase::Publishing));
    assert_eq!(fs::read(ready.join("metadata")).unwrap(), raw_bytes);
}

fn advance_to_gate_plan(fixture: &Fixture, publication: &mut NormalPublication<'_>) {
    for _ in 0..128 {
        publication.step().unwrap();
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if state.structure.is_some_and(|plan| {
            plan.parent.namespace == crate::canonical::Namespace::Ordinary && plan.stage == 1
        }) {
            return;
        }
    }
    panic!("final gate plan continuation bound");
}

#[path = "retirement_tests.rs"]
mod retirement_tests;
