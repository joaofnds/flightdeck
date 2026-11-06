//! The production event writer preserves a retained prefix separately from its loss origin.
use super::*;

#[test]
fn ordinary_leading_gap_numeric_recovers_production_payload() {
    // Goal: a genuinely overwritten event prefix does not strand complete numeric metrics.
    // Method: stage through the production event writer, lose the manifest, and reacquire Store.
    leading_gap(false, true);
}

#[test]
fn ordinary_leading_gap_captured_recovers_production_payload() {
    // Goal: native rows and associations survive a proved leading event loss.
    // Method: retain detached and associated rows from the exact completed captured producer.
    leading_gap(true, true);
}

#[test]
fn ordinary_leading_gap_numeric_readable_prefix_cannot_be_loss() {
    // Goal: omitted readable prefix events cannot acquire loss authority.
    // Method: start fixture output too late while those events remain readable, then recover.
    leading_gap(false, false);
}

#[test]
fn ordinary_leading_gap_captured_readable_prefix_cannot_be_loss() {
    // Goal: captured prefix omission remains occupied rather than becoming certified loss.
    // Method: omit readable native events while retaining complete selected metrics.
    leading_gap(true, false);
}

fn leading_gap_stage(
    captured: bool,
    overwritten: bool,
) -> (Fixture, PathBuf, Arc<crate::resources::ByteBudget>) {
    let mut fixture = metric_fixture(captured, 1);
    let ticks = if overwritten { 3 } else { 2 };
    for run_tick in 0..ticks {
        metric_publish(&mut fixture, run_tick, true, true);
    }
    fixture.writer.finish();
    eprintln!(
        "ORDINARY-LEADING root={} source={}",
        fixture.base.display(),
        fixture.writer.name()
    );
    let publication = ticks + 1;
    let mut input = complete_input(&fixture);
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: publication,
    };
    input.losses.publication_inherited_end = publication;
    input.losses.publications = ticks;
    let budget = registration_budget();
    let object;
    {
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(&input, &mut fixture.reader(), &budget)
            .unwrap();
        object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        leading_gap_payload(
            &attempt,
            &fixture,
            &object.join("pending"),
            publication,
            overwritten,
        );
    }
    (fixture, object, budget)
}

fn leading_gap(captured: bool, overwritten: bool) {
    let (fixture, object, budget) = leading_gap_stage(captured, overwritten);
    let raw = raw_bytes(&object.join("pending"));
    let occupied = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    if overwritten {
        finish_recovery(&mut owner);
        drop(owner);
        assert_eq!(raw_bytes(&object.join("ready")), raw);
        leading_gap_check(&store, &object, &fixture, &budget);
    } else {
        let error = failure(&mut owner);
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::InvalidData
        ));
        drop(owner);
        assert_eq!(fixture.slot(), occupied);
        assert_eq!(raw_bytes(&object.join("pending")), raw);
        assert!(!object.join("pending/metadata").exists());
    }
}

fn leading_gap_payload(
    attempt: &RegisteredAttempt<'_>,
    fixture: &Fixture,
    pending: &Path,
    publication: u64,
    overwritten: bool,
) {
    let mut reader = fixture.reader();
    let window = leading_gap_window(
        &mut reader,
        fixture.run,
        publication,
        attempt.input().captured,
    );
    let mut losses = crate::Losses::default();
    let mut events = crate::event_table::writer::Writer::charged(
        PrivateDirectory::open(pending)
            .unwrap()
            .create_directory("events")
            .unwrap(),
        window.identity,
        if overwritten { 1 } else { 3 },
        attempt.input().captured,
        &Arc::new(crate::resources::ByteBudget::new(256 * 1024 * 1024)),
    )
    .unwrap();
    events
        .publication(
            &mut reader,
            &window.ticks[0],
            window.programs.as_deref(),
            &mut losses,
        )
        .unwrap();
    let index = events
        .finish(&mut reader, window.ticks[0].events_first)
        .unwrap();
    assert_eq!(index.first, 3);
    assert_eq!(index.end, if overwritten { 7 } else { 5 });
    assert_eq!(index.rows, if overwritten { 4 } else { 2 });
    assert_eq!(losses.events, if overwritten { 2 } else { 0 });
    for table in crate::schema::TABLES {
        if matches!(table, crate::schema::Table::Events) {
            continue;
        }
        let mut frame = crate::encoder::frame(table, &window)
            .unwrap()
            .sort(
                table.catalog().key.to_vec(),
                SortMultipleOptions {
                    nulls_last: vec![true],
                    ..Default::default()
                },
            )
            .unwrap();
        crate::encoder::encode_write(
            &mut frame,
            attempt
                .create_file(&format!("{}.parquet", table.name()))
                .unwrap(),
        )
        .unwrap();
    }
}

fn leading_gap_window(
    reader: &mut RingReader,
    run: RunIdentity,
    publication: u64,
    captured: bool,
) -> Window {
    let mut tick = crate::tick_buffer(&reader.shape()).unwrap();
    let mut processes =
        Vec::with_capacity(usize::try_from(reader.shape().process_capacity).unwrap());
    reader.seek_publication(publication).unwrap();
    let found = if captured {
        reader
            .read_identity_into(&mut tick, &mut processes)
            .unwrap()
    } else {
        reader.read_into(&mut tick).unwrap()
    };
    let collector::ring::Found::Publication {
        publication: actual,
        ..
    } = found
    else {
        panic!("selected publication must be retained");
    };
    assert_eq!(actual, publication);
    reader.seek_event(tick.events.start).unwrap();
    let (rows, identities) = leading_gap_window_events(reader, captured);
    let numbered: Vec<_> = rows
        .iter()
        .map(|(number, tick, event)| crate::NumberedEvent {
            number: *number,
            tick: *tick,
            event,
        })
        .collect();
    let mut window = Window::new(
        WindowIdentity { run, window: 0 },
        Limits::from_shape(&reader.shape()).unwrap(),
    );
    let observation = original_observation(publication, &tick);
    if captured {
        window
            .push_tick_with_identity(
                &observation,
                &tick,
                &numbered,
                &crate::IdentitySource {
                    reader,
                    processes: &processes,
                    events: &identities,
                },
            )
            .unwrap();
    } else {
        window.push_tick(&observation, &tick, &numbered).unwrap();
    }
    window
}

fn leading_gap_window_events(
    reader: &mut RingReader,
    captured: bool,
) -> (
    Vec<(u64, u64, collector::tracker::Event)>,
    Vec<collector::ring::IdentityObservation>,
) {
    let mut rows = Vec::with_capacity(2);
    let mut identities = Vec::with_capacity(2);
    for _ in 0..2 {
        let mut identity = collector::ring::IdentityObservation::EMPTY;
        let found = if captured {
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
            panic!("selected tick events must be retained");
        };
        rows.push((number, tick, event));
        identities.push(identity);
    }
    (rows, identities)
}

fn leading_gap_check(
    store: &Store,
    object: &Path,
    fixture: &Fixture,
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
    assert_eq!(frontier.publication_end(), 5);
    assert_eq!(frontier.event_end(), 7);
    let metadata = crate::canonical::CanonicalMetadata::read(
        &mut File::open(object.join("ready/metadata")).unwrap(),
    )
    .unwrap();
    assert_eq!(metadata.loss.inherited.publication_start, 1);
    assert_eq!(metadata.loss.inherited.publications, 3);
    assert_eq!(metadata.loss.publications_new, 0);
    assert_eq!(metadata.loss.inherited.event_start, 1);
    assert_eq!(metadata.loss.inherited.events, 0);
    assert_eq!(metadata.loss.events_new, 2);
    let crate::canonical::metadata::Body::Ordinary(manifest) = &metadata.body else {
        panic!("ordinary gate required");
    };
    let index = crate::event_table::index::Index::read_file(
        &mut File::open(object.join("ready/events/index")).unwrap(),
        manifest,
    )
    .unwrap();
    assert_eq!(manifest.events_first, 1);
    assert_eq!(index.first, 3);
    let mut insufficient = manifest.clone();
    insufficient.losses.events = 1;
    assert!(index.validate_manifest(&insufficient).is_err());
    let mut wrong_origin = manifest.clone();
    wrong_origin.events_first = 4;
    assert!(index.validate_manifest(&wrong_origin).is_err());
    let rows = crate::event_recovery::read::read_batch_file(
        File::open(object.join("ready/events/0000.parquet")).unwrap(),
        &index.batches[0],
        fixture.input(false).captured,
        true,
        budget,
    )
    .unwrap();
    assert_eq!(
        rows.column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [3, 4, 5, 6]
    );
    let publications = rows.column("publication").unwrap().u64().unwrap();
    assert_eq!(
        (0..rows.height())
            .map(|index| publications.get(index))
            .collect::<Vec<_>>(),
        [None, None, Some(4), Some(4)]
    );
}

#[test]
fn ordinary_leading_gap_numeric_source_free_after_proof() {
    // Goal: completed loss proof survives removal of the numeric source transport.
    // Method: stop after complete metadata, unlink the ring, then reacquire normal publication.
    leading_gap_source_free(false);
}

#[test]
fn ordinary_leading_gap_captured_source_free_after_proof() {
    // Goal: captured leading losses and native rows remain recoverable without transport.
    // Method: reacquire Store after complete proof and verify exact canonical ownership.
    leading_gap_source_free(true);
}

fn leading_gap_source_free(captured: bool) {
    let (mut fixture, object, budget) = leading_gap_stage(captured, true);
    let raw = raw_bytes(&object.join("pending"));
    let ledger = PublicationLedger::new(&budget).unwrap();
    {
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
            .unwrap();
        leading_gap_source_free_metadata(&mut owner, &object);
    }
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
    let mut owner = store.resume_publication_counted(&budget, &ledger).unwrap();
    finish_publication(&mut owner);
    drop(owner);
    assert_eq!(raw_bytes(&object.join("ready")), raw);
    leading_gap_check(&store, &object, &fixture, &budget);
}

fn leading_gap_source_free_metadata(owner: &mut OrdinaryRecovery<'_>, object: &Path) {
    for _ in 0..200_000 {
        assert_eq!(owner.step().unwrap(), PublicationStep::Continue);
        let path = object.join("pending/metadata");
        if path.exists() {
            crate::canonical::CanonicalMetadata::read(&mut File::open(path).unwrap()).unwrap();
            return;
        }
    }
    panic!("complete proof metadata did not appear");
}
