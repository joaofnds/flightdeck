//! Logical M split with actual endpoint/source publication and generated exact original rows.
use super::*;

mod finite_events_logical_m_split_replays_and_preserves_both_kinds {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: real M packing/split, both finite fragments, and exact complete union.
        // Method: actual endpoint/overwrite and generated validated original/candidate prefix.
        // This constant-Exec workload is not maximum-native or whole-producer recovery evidence.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: real M packing/split, both finite fragments, and exact complete union.
        // Method: actual endpoint/overwrite and generated validated original/candidate prefix.
        // This constant-Exec workload is not maximum-native or whole-producer recovery evidence.
        run(true);
    }

    fn run(captured: bool) {
        let maximum = collector::u64_from_usize(crate::event_observation::ROWS_MAX);
        let started = std::time::Instant::now();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let (mut fixture, first, template, transaction) =
            finite_split_setup(captured, maximum, &budget, &ledger);
        let object = fixture
            .root
            .join("recovery-data")
            .join(collector::recording::identity_text(transaction).to_string());
        let store = Store::open(&fixture.root).unwrap();
        finite_split_original(
            &fixture,
            &object,
            maximum,
            (&first, &template),
            captured,
            &budget,
        );
        finite_split_candidate(
            &store,
            &fixture,
            &object,
            maximum,
            (&first, &template),
            &budget,
            &ledger,
        );
        finite_split_pack(&mut fixture, &store, maximum, captured, &budget, &ledger);
        finite_split_publish(&store, &fixture, &object, &budget, &ledger);
        drop(store);
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .resume_finite_events(None, &budget, &ledger)
            .unwrap()
            .unwrap();
        finish_finite(&mut owner);
        drop(owner);
        finite_split_oracle(
            &store,
            &fixture,
            transaction,
            maximum,
            (&first, &template),
            &budget,
        );
        finite_split_retirement(&store, &fixture, &object, &budget);
        eprintln!(
            "finite M captured={captured} M={maximum} total={} peak={} time={:?} footprint={:?}",
            maximum + 1,
            budget.used_peak(),
            started.elapsed(),
            crate::resources::Footprint::read().unwrap()
        );
    }
}
/// Takes the two parts of the transaction out one at a time. The older goes when the period has
/// passed it alone, and leaves the newer whole in the directory they share.
fn finite_split_retirement(
    store: &Store,
    fixture: &Fixture,
    object: &Path,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    const PERIOD_SECONDS: u64 = 60;
    const GRACE_SECONDS: u64 = 60;
    let boot = Identity([5, 5]);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    crate::retention::Period::from_seconds(PERIOD_SECONDS)
        .unwrap()
        .write(&root)
        .unwrap();
    crate::retention::Clock::started_at(&crate::retention::ClockReading {
        boot,
        continuous_ns: 0,
        wall_ns: 0,
    })
    .write(&root)
    .unwrap();
    let mut parts: Vec<(u64, String)> = crate::retention::order_entry_names(&fixture.root)
        .into_iter()
        .filter(|name| name.contains("-fragment-"))
        .map(|name| {
            let (place, rest) = name.split_once('-').unwrap();
            (
                place.parse().unwrap(),
                rest.rsplit('-').next().unwrap().to_owned(),
            )
        })
        .collect();
    parts.sort_unstable();
    assert_eq!(parts.len(), 2);
    let keeping_newer = parts[1].0 + (PERIOD_SECONDS + GRACE_SECONDS) * 1_000_000_000;
    let at = |clock_ns| crate::retention::ClockReading {
        boot,
        continuous_ns: clock_ns,
        wall_ns: clock_ns,
    };

    assert_eq!(
        store
            .retire_expired(&at(keeping_newer), &[], budget)
            .unwrap(),
        1
    );

    assert!(!object.join(format!("fragment-{}", parts[0].1)).exists());
    assert!(
        object
            .join(format!("fragment-{}", parts[1].1))
            .join(PROOF_NAME)
            .exists()
    );

    assert_eq!(store.retire_expired(&at(1 << 62), &[], budget).unwrap(), 1);

    assert!(!object.exists());
}

fn finite_split_setup(
    captured: bool,
    maximum: u64,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
) -> (
    Fixture,
    crate::event_recovery::batch::Row,
    crate::event_recovery::batch::Row,
    Identity,
) {
    let mut fixture = proof_fixture(captured, 1);
    source_capacity(&mut fixture, captured, maximum);
    publish_bulk(&mut fixture, captured, 2, 1);
    let mut first = finite_expected_rows(&fixture, 1..2).remove(0);
    first.publication = Some(2);
    let store = Store::open(&fixture.root).unwrap();
    let mut retained = store
        .capture_finite_events(fixture.reader(), budget, ledger)
        .unwrap();
    assert!(retained.step().unwrap());
    inherited_empty(&mut fixture, 3);
    publish_bulk(&mut fixture, captured, 4, usize::try_from(maximum).unwrap());
    inherited_empty(&mut fixture, 5);
    let template = finite_expected_rows(&fixture, 2..3).remove(0);
    let owner = retained.register().unwrap();
    let initial =
        crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
            .unwrap();
    let plan = initial.row_plan.unwrap();
    assert_eq!(plan.end, maximum + 2);
    assert_eq!(plan.source_first, 2);
    assert_eq!(plan.source_end, maximum + 2);
    drop(owner);
    drop(store);
    (fixture, first, template, initial.transaction)
}
fn finite_split_original(
    fixture: &Fixture,
    object: &Path,
    maximum: u64,
    rows: (
        &crate::event_recovery::batch::Row,
        &crate::event_recovery::batch::Row,
    ),
    captured: bool,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let (first, template) = rows;
    let pending = PrivateDirectory::open(&object.join("pending")).unwrap();
    let original = pending.create_directory("events").unwrap();
    let facts = seed_rows(&original, maximum, first, template, captured, budget);
    let index = crate::event_table::index::Index {
        identity: WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        first: 1,
        end: maximum + 2,
        rows: maximum,
        identities: captured,
        batches: facts,
    };
    let mut index_file = original.create_file("index").unwrap();
    index.write_file(&mut index_file).unwrap();
    private::full_sync(&index_file).unwrap();
    original.full_sync().unwrap();
    pending.full_sync().unwrap();
    drop(index_file);
    drop(original);
    drop(pending);
}
fn finite_split_candidate(
    store: &Store,
    fixture: &Fixture,
    object: &Path,
    maximum: u64,
    rows: (
        &crate::event_recovery::batch::Row,
        &crate::event_recovery::batch::Row,
    ),
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
) {
    let (first, template) = rows;
    let captured = fixture.reader().dictionary_capacity().is_some();
    let mut owner = store
        .resume_finite_events(Some(fixture.reader()), budget, ledger)
        .unwrap()
        .unwrap();
    let mut planned = false;
    for _ in 0..10000 {
        let state =
            crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording)
                .unwrap();
        if state.phase == Phase::SalvagePlanned {
            planned = true;
            break;
        }
        assert_eq!(owner.step().unwrap(), RowPublicationStep::Continue);
    }
    assert!(planned);
    let directory = PrivateDirectory::open(object)
        .unwrap()
        .create_directory("fragment-0")
        .unwrap();
    seed_rows(&directory, maximum - 1, first, template, captured, budget);
    directory.full_sync().unwrap();
    drop(directory);
    drop(owner);
}
fn finite_split_pack(
    fixture: &mut Fixture,
    store: &Store,
    maximum: u64,
    captured: bool,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
) {
    publish_bulk(fixture, captured, 6, usize::try_from(maximum - 1).unwrap());
    let mut reader = fixture.reader();
    reader.seek_event(2).unwrap();
    assert!(matches!(reader.read_event().unwrap(),
            collector::ring::FoundEvent::Missed {first:2,end} if end==maximum+1));
    assert!(matches!(reader.read_event().unwrap(),
            collector::ring::FoundEvent::Event {number,..} if number==maximum+1));
    let mut candidates = store
        .resume_row_candidates(fixture.reader(), budget, ledger)
        .unwrap();
    let mut prepared = false;
    let mut packing = false;
    for _ in 0..100_000 {
        if candidates.rows()[0] == maximum - 1 && candidates.pending_rows() > 0 {
            assert_eq!(candidates.pending_rows(), 1);
            packing = true;
        }
        match candidates.step().unwrap() {
            row_candidates::CandidateStep::Prepared => {
                prepared = true;
                break;
            }
            row_candidates::CandidateStep::Continue => {}
            row_candidates::CandidateStep::Unresolved => panic!("finite split unresolved"),
        }
    }
    assert!(packing);
    assert!(prepared);
    assert_eq!(candidates.rows(), [maximum, 1]);
    assert_eq!(candidates.plan().unwrap().split, maximum + 1);
    drop(candidates);
}
fn finite_split_publish(
    store: &Store,
    fixture: &Fixture,
    object: &Path,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
) {
    let mut owner = store
        .resume_finite_events(Some(fixture.reader()), budget, ledger)
        .unwrap()
        .unwrap();
    let mut completed = false;
    for _ in 0..100_000 {
        if object.join("fragment-1/metadata").exists() {
            completed = true;
            break;
        }
        assert_eq!(owner.step().unwrap(), RowPublicationStep::Continue);
    }
    assert!(completed);
    drop(owner);
}
fn finite_split_oracle(
    store: &Store,
    fixture: &Fixture,
    transaction: Identity,
    maximum: u64,
    rows: (
        &crate::event_recovery::batch::Row,
        &crate::event_recovery::batch::Row,
    ),
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let (first, template) = rows;
    let first_metadata = finite_metadata(fixture, transaction, "fragment-0");
    let second = finite_metadata(fixture, transaction, "fragment-1");
    assert_eq!(
        first_metadata.kind,
        crate::transaction::CanonicalKind::FiniteEvents
    );
    assert_eq!(second.kind, first_metadata.kind);
    assert_eq!(first_metadata.loss.inherited.event_start, 1);
    assert_eq!(first_metadata.loss.event_end, maximum + 1);
    assert_eq!(second.loss.inherited.event_start, maximum + 1);
    assert_eq!(second.loss.event_end, maximum + 2);
    assert_eq!(
        second.loss.inherited.predecessor,
        Predecessor::Canonical {
            transaction,
            kind: crate::transaction::CanonicalKind::FiniteEvents,
            origin: 1
        }
    );
    for metadata in [&first_metadata, &second] {
        assert_eq!(metadata.loss.inherited.events, 0);
        assert_eq!(metadata.loss.events_new, 0);
        assert_eq!(metadata.loss.publication_end, 1);
        assert_eq!(metadata.loss.inherited.prior_window, None);
    }
    verify_rows(fixture, &first_metadata, first, template, budget);
    verify_rows(fixture, &second, first, template, budget);
    let frontier = store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.event_end(), maximum + 2);
    assert_eq!(frontier.publication_end(), 1);
    assert!(!frontier.complete());
}

fn source_capacity(fixture: &mut Fixture, captured: bool, capacity: u64) {
    let mut shape = fixture.reader().shape();
    shape.event_capacity = capacity;
    let place = collector::ring::Place {
        lock_path: fixture.base.join("finite-m-source.lock"),
        name_prefix: format!(
            "/fdm{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let writer = if captured {
        collector::ring::Writer::create_with_identity(
            &place,
            shape,
            collector::ring::DictionaryCapacity {
                entries: 4,
                arena_bytes: 16 * 1024,
            },
            &fixture.run,
            1,
        )
        .unwrap()
    } else {
        collector::ring::Writer::create_recording(&place, shape, &fixture.run, 1).unwrap()
    };
    let old = fixture.writer.name().to_owned();
    fixture.writer = writer;
    collector::ring::unlink_generation(&old).unwrap();
    fixture.binding = fixture.reader().object_binding().unwrap();
    fixture.source = crate::transaction::SourceTiming::from_reader(&fixture.reader());
    fixture.place = place;
    publish_at(&mut fixture.writer, 0);
}
fn publish_bulk(fixture: &mut Fixture, captured: bool, tick: u64, count: usize) {
    assert!(count <= crate::event_observation::ROWS_MAX);
    let events = (0..count)
        .map(|_| collector::tracker::Event::Exec { pid: 41 })
        .collect::<Vec<_>>();
    let memory = collector::machine::Memory::default();
    let readings = collector::ring::Readings {
        run_tick: tick,
        cpus: &[],
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &events,
        dropped: collector::ring::Dropped::default(),
    };
    let times = collector::ring::ObservationTimes {
        wall_ns: tick,
        continuous_ns: tick,
        awake_ns: tick,
    };
    if captured {
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
        let identities = vec![identity; count];
        fixture
            .writer
            .publish_with_identity(
                &readings,
                Some(&times),
                &collector::ring::IdentityPublication {
                    processes: &[],
                    events: &identities,
                },
            )
            .unwrap();
    } else {
        fixture.writer.publish_observed(&readings, &times);
    }
}
fn rows(
    first: &crate::event_recovery::batch::Row,
    template: &crate::event_recovery::batch::Row,
    start: u64,
    end: u64,
) -> Vec<crate::event_recovery::batch::Row> {
    assert!(end - start <= 16_384);
    (start..end)
        .map(|number| {
            let source = if number == 1 { first } else { template };
            let mut event = source.event.clone();
            event.number = number;
            crate::event_recovery::batch::Row {
                publication: source.publication,
                event,
                identity: source.identity,
                name: source.name.clone(),
                app: source.app.clone(),
            }
        })
        .collect()
}
fn seed_rows(
    directory: &PrivateDirectory,
    count: u64,
    first: &crate::event_recovery::batch::Row,
    template: &crate::event_recovery::batch::Row,
    captured: bool,
    budget: &Arc<crate::resources::ByteBudget>,
) -> Vec<crate::event_recovery::Batch> {
    let _workspace = budget
        .reserve(crate::event_recovery::batch::WORKING_BYTES)
        .unwrap();
    let mut facts = Vec::with_capacity(4096);
    let mut next = 1;
    for ordinal in 0..4096 {
        if next == count + 1 {
            break;
        }
        let end = (next + 16_384).min(count + 1);
        let rows = rows(first, template, next, end);
        let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
        let mut file = directory
            .create_file(&crate::event_recovery::batch_name(ordinal))
            .unwrap();
        crate::encoder::encode_write(&mut frame, &mut file).unwrap();
        private::full_sync(&file).unwrap();
        facts.push(crate::event_recovery::Batch {
            first: next,
            end,
            rows: end - next,
        });
        next = end;
    }
    assert_eq!(next, count + 1);
    facts
}
fn verify_rows(
    fixture: &Fixture,
    metadata: &crate::canonical::CanonicalMetadata,
    first: &crate::event_recovery::batch::Row,
    template: &crate::event_recovery::batch::Row,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let crate::canonical::metadata::Body::Events(manifest) = &metadata.body else {
        panic!("finite fragment");
    };
    assert!(manifest.batches.len() <= 4096);
    let _workspace = budget
        .reserve(crate::event_recovery::batch::WORKING_BYTES)
        .unwrap();
    let object = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(metadata.transaction).to_string())
        .join(metadata.child.name());
    let mut total = 0;
    for (ordinal, batch) in manifest.batches.iter().enumerate() {
        assert!(batch.rows <= 16_384);
        let actual = crate::event_recovery::read::read_batch_file(
            private::open_private_file(
                &object.join(crate::event_recovery::batch_name(ordinal)),
                false,
            )
            .unwrap(),
            batch,
            metadata.captured,
            true,
            budget,
        )
        .unwrap();
        let expected = crate::event_recovery::batch::frame(
            &rows(first, template, batch.first, batch.end),
            metadata.captured,
        )
        .unwrap();
        assert!(actual.equals_missing(&expected));
        total += batch.rows;
    }
    assert_eq!(total, manifest.recovery.rows);
}
