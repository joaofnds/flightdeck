use super::*;
use collector::recording::bootstrap::codec::Phase;

fn inherited_fixture(captured: bool) -> (Fixture, AttemptInput) {
    let mut fixture = proof_fixture(captured, 1);
    for number in 2..=4 {
        if captured {
            publish_captured_pair_at(&mut fixture, number, number);
        } else {
            publish_numeric_pair_at(&mut fixture, number, number);
        }
    }
    let mut reader = fixture.reader();
    reader.seek_event(1).unwrap();
    assert!(matches!(
        reader.read_event().unwrap(),
        collector::ring::FoundEvent::Missed { first: 1, end: 3 }
    ));
    for expected in 3..7 {
        assert!(matches!(reader.read_event().unwrap(),
            collector::ring::FoundEvent::Event { number, .. } if number == expected));
    }
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 4,
    };
    input.losses.publication_inherited_end = 4;
    input.losses.publications = 3;
    input.losses.event_inherited_end = 5;
    input.losses.events = 2;
    (fixture, input)
}
fn inherited_register<'a>(
    store: &'a Store,
    fixture: &Fixture,
    input: &AttemptInput,
    budget: &Arc<crate::resources::ByteBudget>,
) -> RegisteredAttempt<'a> {
    assert_eq!(
        store
            .register_attempt(input, &mut fixture.reader(), budget)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Unsupported
    );
    let ledger = PublicationLedger::new(budget).unwrap();
    let mut finite = store
        .begin_finite_events(fixture.reader(), budget, &ledger)
        .unwrap();
    let state = finite_state(fixture);
    let AttemptOperation::Finite { event_end, .. } = state.input.operation else {
        panic!("finite");
    };
    assert_eq!(event_end, input.losses.event_inherited_end);
    finish_finite(&mut finite);
    drop(finite);
    if event_end == 5 {
        let tick = encode_payload_tick(&mut fixture.reader(), 4);
        let expected = crate::event_recovery::batch::frame(
            &inherited_rows(fixture, &tick)[..2],
            input.captured,
        )
        .unwrap();
        finite_rows_oracle(fixture, state.transaction, &expected, budget);
    } else {
        assert_eq!(event_end, 3);
        let metadata = finite_metadata(fixture, state.transaction, "loss");
        assert_eq!(
            metadata.kind,
            crate::transaction::CanonicalKind::FiniteEventLoss
        );
        assert_eq!(metadata.loss.events_new, 2);
        assert_eq!(metadata.loss.inherited.events, 0);
        assert_eq!(metadata.loss.event_end, 3);
    }
    let frontier = store
        .source_frontier(
            &input.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.event_end(), event_end);
    assert_eq!(frontier.publication_end(), input.losses.publication_start);
    let mut next = input.clone();
    next.losses.predecessor = frontier.predecessor().clone();
    next.losses.event_start = event_end;
    next.losses.event_inherited_end = event_end;
    next.losses.events = 0;
    store
        .register_attempt(&next, &mut fixture.reader(), budget)
        .unwrap()
}
mod inherited_events_public_store_accepts_proved_nonempty_domain {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: admit real inherited losses and retained rows without trusting a caller count.
        // Method: independently verify genuine overwrite and consume the paced Store capability.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: admit real inherited losses and retained rows without trusting a caller count.
        // Method: independently verify genuine overwrite and consume the paced Store capability.
        run(true);
    }

    fn run(captured: bool) {
        let (fixture, input) = inherited_fixture(captured);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = inherited_register(&store, &fixture, &input, &budget);
        assert_eq!(attempt.input().losses.event_start, 5);
        assert_eq!(attempt.input().losses.events, 0);
    }
}
mod inherited_events_forged_counts_preserve_idle {
    use super::*;

    #[test]
    fn numeric_too_few() {
        // Goal: a proposed count is checked against actual missing positions before registration.
        // Method: genuine two-event loss, with both smaller and larger forged counts.
        run(false, 1);
    }

    #[test]
    fn numeric_too_many() {
        // Goal: a proposed count is checked against actual missing positions before registration.
        // Method: genuine two-event loss, with both smaller and larger forged counts.
        run(false, 3);
    }

    #[test]
    fn captured_too_few() {
        // Goal: a proposed count is checked against actual missing positions before registration.
        // Method: genuine two-event loss, with both smaller and larger forged counts.
        run(true, 1);
    }

    #[test]
    fn captured_too_many() {
        // Goal: a proposed count is checked against actual missing positions before registration.
        // Method: genuine two-event loss, with both smaller and larger forged counts.
        run(true, 3);
    }

    fn run(captured: bool, count: u64) {
        let (fixture, mut input) = inherited_fixture(captured);
        input.losses.events = count;
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let slot = fixture.slot();
        let mut proof = store
            .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
            .unwrap();
        assert_eq!(proof.step().unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fixture.slot(), slot);
        assert!(
            fs::read_dir(fixture.root.join("data"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}
mod inherited_events_normal_publication_conserves_prefix {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: inherited losses and surviving pre-tick rows enter the canonical ordinary domain.
        // Method: encode all actual suffix rows with their original association, then seal
        // normally.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: inherited losses and surviving pre-tick rows enter the canonical ordinary domain.
        // Method: encode all actual suffix rows with their original association, then seal
        // normally.
        run(true);
    }

    fn run(captured: bool) {
        let (mut fixture, input) = inherited_fixture(captured);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = inherited_register(&store, &fixture, &input, &budget);
        let manifest = inherited_normal_manifest(&attempt, &fixture);
        publish_at(&mut fixture.writer, 1);
        finish_publication(&mut issue(attempt, &manifest, fixture.reader(), &budget).unwrap());
        let frontier = finite_frontier(&store, &fixture, &budget);
        assert_eq!(frontier.event_end(), 7);
        assert_eq!(frontier.publication_end(), 5);
    }
}
fn inherited_normal_manifest(attempt: &RegisteredAttempt<'_>, fixture: &Fixture) -> Manifest {
    let manifest = encode_payload(attempt, fixture, 4, false);
    assert_eq!(manifest.events_first, 5);
    assert_eq!(manifest.losses.events, 0);
    manifest
}

fn inherited_rows(
    fixture: &Fixture,
    tick: &collector::ring::Tick,
) -> Vec<crate::event_recovery::batch::Row> {
    let mut reader = fixture.reader();
    reader.seek_event(3).unwrap();
    let mut rows = Vec::with_capacity(4);
    let captured = reader.dictionary_capacity().is_some();
    for expected in 3..7 {
        let mut identity = collector::ring::IdentityObservation::EMPTY;
        let found = if captured {
            reader.read_event_identity(&mut identity).unwrap()
        } else {
            reader.read_event().unwrap()
        };
        let collector::ring::FoundEvent::Event {
            number,
            tick: run_tick,
            event,
        } = found
        else {
            panic!("expected retained suffix");
        };
        assert_eq!(number, expected);
        let mut row = crate::event_recovery::batch::copy_row(
            &reader,
            &crate::NumberedEvent {
                number,
                tick: run_tick,
                event: &event,
            },
            captured.then_some(&identity),
        )
        .unwrap();
        row.publication = if number >= tick.events.start {
            Some(4)
        } else {
            None
        };
        rows.push(row);
    }
    rows
}

mod inherited_events_row_recovery_preserves_exact_fields {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: ordinary failure preserves the inherited/new partition and every native row field.
        // Method: real sparse originals, actual ring overwrite, canonical fragment full-frame
        // oracle.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: ordinary failure preserves the inherited/new partition and every native row field.
        // Method: real sparse originals, actual ring overwrite, canonical fragment full-frame
        // oracle.
        run(true);
    }

    fn run(captured: bool) {
        let (mut fixture, input) = inherited_fixture(captured);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = inherited_register(&store, &fixture, &input, &budget);
        let tick = encode_payload_tick(&mut fixture.reader(), 4);
        let rows = inherited_rows(&fixture, &tick)
            .into_iter()
            .filter(|row| row.event.number >= attempt.input().losses.event_start)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        let expected = crate::event_recovery::batch::frame(&rows, captured).unwrap();
        let transaction = attempt.transaction();
        let mut frame = expected.clone();
        crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap())
            .unwrap();
        publish_at(&mut fixture.writer, 1);
        for number in 61..=63 {
            if captured {
                publish_captured_pair_at(&mut fixture, number, number * 1_000_000_000);
            } else {
                publish_numeric_pair_at(&mut fixture, number, number * 1_000_000_000);
            }
        }
        let ledger = PublicationLedger::new(&budget).unwrap();
        let mut owner = attempt
            .recover_rows(fixture.reader(), &budget, &ledger)
            .unwrap();
        let mut done = false;
        for _ in 0..20000 {
            match owner.step().unwrap() {
                RowPublicationStep::Continue => {}
                RowPublicationStep::Idle => panic!("lost boundary cannot clear authority"),
                RowPublicationStep::Unresolved => {
                    done = true;
                    break;
                }
            }
        }
        assert!(done);
        let object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(transaction).to_string());
        let metadata = crate::canonical::CanonicalMetadata::read(
            &mut private::open_private_file(&object.join("fragment-0/metadata"), false).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata.loss.inherited.events, 0);
        assert_eq!(metadata.loss.events_new, 0);
        assert_eq!(metadata.loss.event_end, 7);
        let crate::canonical::metadata::Body::Events(manifest) = metadata.body else {
            panic!("rows");
        };
        let actual = crate::event_recovery::read::read_batch_file(
            private::open_private_file(&object.join("fragment-0/0000.parquet"), false).unwrap(),
            &manifest.batches[0],
            captured,
            true,
            &budget,
        )
        .unwrap();
        assert!(actual.equals_missing(&expected));
    }
}

mod inherited_events_loss_only_partitions_real_overwrite {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: an inherited missing prefix is charged once alongside newly overwritten events.
        // Method: retain the first next-window boundary while genuinely overwriting all failed
        // rows.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: an inherited missing prefix is charged once alongside newly overwritten events.
        // Method: retain the first next-window boundary while genuinely overwriting all failed
        // rows.
        run(true);
    }

    fn run(captured: bool) {
        let (mut fixture, input) = inherited_loss_fixture(captured);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = inherited_register(&store, &fixture, &input, &budget);
        let transaction = attempt.transaction();
        crate::event_table::index::Index {
            identity: WindowIdentity {
                run: input.run,
                window: 0,
            },
            first: 3,
            end: 7,
            rows: 0,
            identities: captured,
            batches: Vec::new(),
        }
        .write_file(&mut attempt.create_event_index().unwrap())
        .unwrap();
        inherited_empty(&mut fixture, 61);
        if captured {
            publish_captured_pair_at(&mut fixture, 62, 62_000_000_000);
        } else {
            publish_numeric_pair_at(&mut fixture, 62, 62_000_000_000);
        }
        inherited_empty(&mut fixture, 63);
        if captured {
            publish_captured_pair_at(&mut fixture, 64, 64_000_000_000);
        } else {
            publish_numeric_pair_at(&mut fixture, 64, 64_000_000_000);
        }
        let mut reader = fixture.reader();
        reader.seek_event(1).unwrap();
        assert!(matches!(
            reader.read_event().unwrap(),
            collector::ring::FoundEvent::Missed { first: 1, end: 7 }
        ));
        let mut owner = attempt.prove_loss(fixture.reader(), &budget).unwrap();
        let mut done = false;
        for _ in 0..20000 {
            if owner.step().unwrap() == PublicationStep::Idle {
                done = true;
                break;
            }
        }
        assert!(done);
        inherited_loss_oracle(&fixture, transaction);
    }
}

fn inherited_loss_fixture(captured: bool) -> (Fixture, AttemptInput) {
    let mut fixture = proof_fixture(captured, 3);
    for number in 2..=2 {
        if captured {
            publish_captured_pair_at(&mut fixture, number, number);
        } else {
            publish_numeric_pair_at(&mut fixture, number, number);
        }
    }
    inherited_empty(&mut fixture, 3);
    if captured {
        publish_captured_pair_at(&mut fixture, 4, 4);
    } else {
        publish_numeric_pair_at(&mut fixture, 4, 4);
    }
    inherited_empty(&mut fixture, 5);
    if captured {
        publish_captured_pair_at(&mut fixture, 6, 6);
    } else {
        publish_numeric_pair_at(&mut fixture, 6, 6);
    }
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 4,
    };
    input.losses.publication_inherited_end = 4;
    input.losses.publications = 3;
    input.losses.event_inherited_end = 3;
    input.losses.events = 2;
    (fixture, input)
}

fn inherited_loss_oracle(fixture: &Fixture, transaction: Identity) {
    let path = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(transaction).to_string())
        .join("loss/metadata");
    let metadata = crate::canonical::CanonicalMetadata::read(
        &mut private::open_private_file(&path, false).unwrap(),
    )
    .unwrap();
    // The separate finite gate already owns the two inherited losses.
    assert_eq!(metadata.loss.inherited.events, 0);
    assert_eq!(metadata.loss.inherited.event_start, 3);
    assert_eq!(metadata.loss.events_new, 4);
    assert_eq!(metadata.loss.inherited.publications, 3);
    assert_eq!(metadata.loss.publications_new, 4);
    assert_eq!(metadata.loss.event_end, 7);
    assert_eq!(metadata.loss.publication_end, 8);
}

fn inherited_empty(fixture: &mut Fixture, number: u64) {
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
    let times = collector::ring::ObservationTimes {
        wall_ns: number,
        continuous_ns: if number >= 60 {
            number * 1_000_000_000
        } else {
            number
        },
        awake_ns: number,
    };
    if fixture.writer.dictionary_usage().unwrap().is_some() {
        fixture
            .writer
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
        fixture.writer.publish_observed(&readings, &times);
    }
}

mod inherited_events_shifted_loss_set_cannot_replace_established_provenance {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: equal counts cannot exchange a proved inherited loss for a later missing row.
        // Method: persist rows1,4,5,6 after proving losses1,2; overwrite the source and cold
        // reopen.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: equal counts cannot exchange a proved inherited loss for a later missing row.
        // Method: persist rows1,4,5,6 after proving losses1,2; overwrite the source and cold
        // reopen.
        run(true);
    }

    fn run(captured: bool) {
        let (mut fixture, input) = inherited_fixture(captured);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let attempt = inherited_register(&store, &fixture, &input, &budget);
        let tick = encode_payload_tick(&mut fixture.reader(), 4);
        let mut rows = inherited_rows(&fixture, &tick);
        rows[0].event.number = 1;
        rows[0].event.tick = 2;
        let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
        crate::encoder::encode_write(&mut frame, attempt.create_event_batch(4095).unwrap())
            .unwrap();
        drop(attempt);
        drop(store);
        for number in 61..=63 {
            if captured {
                publish_captured_pair_at(&mut fixture, number, number * 1_000_000_000);
            } else {
                publish_numeric_pair_at(&mut fixture, number, number * 1_000_000_000);
            }
        }
        let store = Store::open(&fixture.root).unwrap();
        let ledger = PublicationLedger::new(&budget).unwrap();
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
                Ok(result) => panic!("same-count loss substitution accepted: {result:?}"),
            }
        }
        assert!(refused);
    }
}

mod inherited_events_shared_budget_and_binding_refuse_without_registration {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: failed admission reserves only the caller budget and creates no durable authority.
        // Method: exhaust the shared budget and separately pass an immutable-binding mismatch.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: failed admission reserves only the caller budget and creates no durable authority.
        // Method: exhaust the shared budget and separately pass an immutable-binding mismatch.
        run(true);
    }

    fn run(captured: bool) {
        let (mut fixture, input) = inherited_fixture(captured);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let slot = fixture.slot();
        let reservation = budget.reserve(budget.maximum() - budget.used()).unwrap();
        let error = store
            .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
        drop(reservation);
        let mut changed = input.clone();
        changed.binding.generation += 1;
        let error = store
            .prepare_inherited_attempt(&changed, fixture.reader(), &budget, &ledger)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        inherited_large_source(&mut fixture, captured);
        let error = store
            .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fixture.slot(), slot);
        assert_eq!(budget.used(), 1024);
    }
}

mod inherited_events_pacing_and_abandoned_work_remain_bounded {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: a nonempty inherited domain larger than128 yields and keeps attempted-work charges.
        // Method: produce160 actual rows, drop/reconstruct the capability under one shared ledger.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: a nonempty inherited domain larger than128 yields and keeps attempted-work charges.
        // Method: produce160 actual rows, drop/reconstruct the capability under one shared ledger.
        run(true);
    }

    fn run(captured: bool) {
        let mut fixture = proof_fixture(captured, 1);
        inherited_large_source(&mut fixture, captured);
        let mut input = fixture.input(false);
        input.losses.unknown = None;
        input.operation = AttemptOperation::Ordinary {
            window: 0,
            attempted_origin: 41,
        };
        input.losses.publication_inherited_end = 41;
        input.losses.publications = 40;
        input.losses.event_inherited_end = 161;
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let slot = fixture.slot();
        let mut proof = store
            .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
            .unwrap();
        assert!(!proof.step().unwrap());
        assert_eq!(ledger.work().source_reads, 128);
        assert_eq!(fixture.slot(), slot);
        drop(proof);
        let mut proof = store
            .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
            .unwrap();
        assert!(!proof.step().unwrap());
        assert!(proof.step().unwrap());
        assert_eq!(ledger.work().source_reads, 288);
        assert_eq!(ledger.work().proof_operations, 288);
        assert_eq!(
            proof.register().err().unwrap().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(fixture.slot(), slot);
    }
}
fn inherited_large_source(fixture: &mut Fixture, captured: bool) {
    let mut shape = fixture.reader().shape();
    shape.event_capacity = 512;
    let place = collector::ring::Place {
        lock_path: fixture.base.join("inherited-pacing.lock"),
        name_prefix: format!(
            "/fdi{:x}{:x}",
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
    for number in 2..=41 {
        if captured {
            publish_captured_events_at(fixture, number, number, 4);
        } else {
            publish_numeric_events_at(fixture, number, number, 4);
        }
    }
}

mod inherited_events_observed_rows_overwritten_before_durability_do_not_grant_authority {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: neither an observed row nor Event-then-Missed counts can settle inherited transfer.
        // Method: scan128 real events, genuinely overwrite them, then observe a later missing
        // suffix.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: neither an observed row nor Event-then-Missed counts can settle inherited transfer.
        // Method: scan128 real events, genuinely overwrite them, then observe a later missing
        // suffix.
        run(true);
    }

    fn run(captured: bool) {
        let mut fixture = proof_fixture(captured, 1);
        inherited_large_source(&mut fixture, captured);
        let mut input = fixture.input(false);
        input.losses.unknown = None;
        input.operation = AttemptOperation::Ordinary {
            window: 0,
            attempted_origin: 41,
        };
        input.losses.publication_inherited_end = 41;
        input.losses.publications = 40;
        input.losses.event_inherited_end = 161;
        input.losses.events = 32;
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let slot = fixture.slot();
        let mut observation = store
            .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
            .unwrap();
        assert!(!observation.step().unwrap());
        for number in 42..=201 {
            if captured {
                publish_captured_events_at(&mut fixture, number, number, 4);
            } else {
                publish_numeric_events_at(&mut fixture, number, number, 4);
            }
        }
        let mut reader = fixture.reader();
        reader.seek_event(129).unwrap();
        assert!(matches!(
            reader.read_event().unwrap(),
            collector::ring::FoundEvent::Missed {
                first: 129,
                end: 289
            }
        ));
        assert!(observation.step().unwrap());
        let error = observation.register().err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(fixture.slot(), slot);
        assert!(
            fs::read_dir(fixture.root.join("data"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}
#[test]
fn inherited_events_captured_public_store_required_route() {
    // Goal: retain the required public captured-profile counterexample until composition exists.
    // Method: genuine same-object loss plus independently checked surviving captured events.
    let (fixture, input) = inherited_fixture(true);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    assert_eq!(
        store
            .register_attempt(&input, &mut fixture.reader(), &budget)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Unsupported
    );
    let attempt = inherited_register(&store, &fixture, &input, &budget);
    assert_eq!(attempt.input().losses.event_start, 5);
    assert_eq!(attempt.input().losses.events, 0);
}

mod inherited_events_retained_source_roundtrip_preserves_native_and_association {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: the retained prefix/suffix encoder preserves full fields across the shared decoder.
        // Method: actual ring rows with independent numeric/native literals and full-frame
        // comparison.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: the retained prefix/suffix encoder preserves full fields across the shared decoder.
        // Method: actual ring rows with independent numeric/native literals and full-frame
        // comparison.
        run(true);
    }

    fn run(captured: bool) {
        let (fixture, _) = inherited_fixture(captured);
        let tick = encode_payload_tick(&mut fixture.reader(), 4);
        let rows = inherited_rows(&fixture, &tick);
        for (position, row) in rows.iter().enumerate() {
            inherited_retained_literal(row, position, captured);
        }
        let expected = crate::event_recovery::batch::frame(&rows, captured).unwrap();
        let path = fixture.base.join("retained-prefix.parquet");
        let mut encoded = expected.clone();
        crate::encoder::encode_write(
            &mut encoded,
            private::open_private_file(&path, true).unwrap(),
        )
        .unwrap();
        let actual = crate::event_recovery::read::read_batch_file(
            private::open_private_file(&path, false).unwrap(),
            &crate::event_recovery::Batch {
                first: 3,
                end: 7,
                rows: 4,
            },
            captured,
            true,
            &registration_budget(),
        )
        .unwrap();
        assert!(actual.equals_missing(&expected));
    }
}

fn inherited_retained_literal(
    row: &crate::event_recovery::batch::Row,
    position: usize,
    captured: bool,
) {
    assert_eq!(row.event.number, collector::u64_from_usize(position) + 3);
    assert_eq!(row.event.tick, collector::u64_from_usize(position / 2) + 3);
    assert_eq!(row.event.pid, 41);
    assert!(row.event.usage.is_none());
    assert_eq!(row.publication, if position < 2 { None } else { Some(4) });
    if captured {
        assert_eq!(row.name.as_deref(), Some(b"program-\xff".as_slice()));
        assert_eq!(
            row.app.as_deref(),
            Some(b"/Applications/Binary-\xfe.app".as_slice())
        );
        let identity = row.identity.as_ref().unwrap();
        assert_eq!(identity.pid, 41);
        let creation = identity.creation.as_ref().unwrap();
        assert_eq!(creation.unique_id, 991);
        assert_eq!(creation.pid_version, 7);
        assert_eq!(
            identity.availability,
            crate::schema::identity_availability::MEASURED
        );
    } else {
        assert!(row.identity.is_none());
    }
    assert_eq!(row.event.kind, crate::schema::event_kind::EXEC);
    assert!(row.event.coalition.is_none());
    assert!(row.event.noticed.is_none());
}

mod inherited_events_source_observation_alone_does_not_settle {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: a source-only observation cannot issue settled inherited event ownership.
        // Method: complete an accurate observation with retained exceptions but no durable rows.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: a source-only observation cannot issue settled inherited event ownership.
        // Method: complete an accurate observation with retained exceptions but no durable rows.
        run(true);
    }

    fn run(captured: bool) {
        let (fixture, input) = inherited_fixture(captured);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let slot = fixture.slot();
        let mut observation = store
            .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
            .unwrap();
        assert!(observation.step().unwrap());
        assert_eq!(
            observation.register().err().unwrap().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(fixture.slot(), slot);
        assert!(
            fs::read_dir(fixture.root.join("data"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}

mod inherited_events_failed_source_operation_keeps_proof_position {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: EIO after a real source observation cannot invent a missing position or advance
        // proof.
        // Method: the existing checked-read owner's operation returns EIO after reading real
        // Missed.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: EIO after a real source observation cannot invent a missing position or advance
        // proof.
        // Method: the existing checked-read owner's operation returns EIO after reading real
        // Missed.
        run(true);
    }

    fn run(captured: bool) {
        let (fixture, input) = inherited_fixture(captured);
        let budget = registration_budget();
        let mut proof = normal_proof::SourceProof::new(
            fixture.reader(),
            &input,
            budget.reserve(64 * 1024).unwrap(),
        )
        .unwrap();
        let result: io::Result<()> = proof.checked_read(&input, |reader| {
            reader.seek_event(1)?;
            assert!(matches!(
                reader.read_event()?,
                collector::ring::FoundEvent::Missed { first: 1, end: 3 }
            ));
            Err(io::Error::from_raw_os_error(5))
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
        assert_eq!(proof.positions(), (1, 1));
        assert_eq!(proof.losses, [0; 4]);
        assert_eq!(proof.reads, 1);
    }
}
mod inherited_events_actual_header_change_keeps_cursor_and_exact_attempted_observations {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: failed immutable-header readmission charges only observations actually attempted.
        // Method: change the owned object's nonce, attempt one step, restore before
        // assertions/retry.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: failed immutable-header readmission charges only observations actually attempted.
        // Method: change the owned object's nonce, attempt one step, restore before
        // assertions/retry.
        run(true);
    }

    fn run(captured: bool) {
        let (fixture, input) = inherited_fixture(captured);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let mut header = WritableHeader::open(&fixture);
        let mut observation = store
            .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
            .unwrap();
        let before = ledger.work();
        let old = header.word(40).fetch_xor(1, Ordering::Release);
        let result = observation.step();
        header.word(40).store(old, Ordering::Release);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(observation.position(), 1);
        assert_eq!(
            ledger.work().source_observations - before.source_observations,
            1
        );
        assert_eq!(ledger.work().source_reads - before.source_reads, 1);
        assert!(observation.step().unwrap());
        assert_eq!(observation.position(), 5);
    }
}

mod inherited_events_canonical_predecessor_remains_independent_of_provisional_events {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: a real predecessor owns earlier positions without absorbing uncommitted event loss.
        // Method: publish an ordinary gate, genuinely wrap later events, then observe the new
        // domain.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: a real predecessor owns earlier positions without absorbing uncommitted event loss.
        // Method: publish an ordinary gate, genuinely wrap later events, then observe the new
        // domain.
        run(true);
    }

    fn run(captured: bool) {
        let (fixture, input) = inherited_predecessor_fixture(captured);
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let slot = fixture.slot();
        let mut observation = store
            .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
            .unwrap();
        assert!(observation.step().unwrap());
        assert!(ledger.work().canonical.metadata_admissions > 0);
        assert_eq!(input.losses.publication_start, 2);
        assert_eq!(input.losses.prior_window, Some(0));
        assert_eq!(input.losses.event_start, 1);
        assert_eq!(
            observation.register().err().unwrap().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(fixture.slot(), slot);
        let mut forged = input.clone();
        let Predecessor::Canonical { transaction, .. } = &mut forged.losses.predecessor else {
            panic!("canonical predecessor required");
        };
        *transaction = Identity([987, 654]);
        let error = store
            .prepare_inherited_attempt(&forged, fixture.reader(), &budget, &ledger)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fixture.slot(), slot);
    }
}
fn inherited_predecessor_fixture(captured: bool) -> (Fixture, AttemptInput) {
    let mut fixture = proof_fixture(captured, 4);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &budget)
        .unwrap();
    let manifest = encode_payload(&attempt, &fixture, 1, false);
    publish_at(&mut fixture.writer, 1);
    finish_publication(&mut issue(attempt, &manifest, fixture.reader(), &budget).unwrap());
    let frontier = finite_frontier(&store, &fixture, &budget);
    assert_eq!(frontier.publication_end(), 2);
    assert_eq!(frontier.event_end(), 1);
    for number in 61..=62 {
        if captured {
            publish_captured_pair_at(&mut fixture, number, number * 1_000_000_000);
        } else {
            publish_numeric_pair_at(&mut fixture, number, number * 1_000_000_000);
        }
    }
    for number in 65..=67 {
        inherited_empty(&mut fixture, number);
    }
    if captured {
        publish_captured_pair_at(&mut fixture, 68, 68_000_000_000);
    } else {
        publish_numeric_pair_at(&mut fixture, 68, 68_000_000_000);
    }
    input.operation = AttemptOperation::Ordinary {
        window: 1,
        attempted_origin: 5,
    };
    input.losses.predecessor = frontier.predecessor().clone();
    input.losses.prior_window = frontier.prior_window();
    input.losses.publication_start = 2;
    input.losses.publication_inherited_end = 5;
    input.losses.publications = 3;
    input.losses.event_inherited_end = 5;
    input.losses.events = 2;
    drop(store);
    (fixture, input)
}

mod finite_events_public_store_completes_before_ordinary {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: settle orphan events through a finite gate before ordinary publication resumes.
        // Method: real overwrite, independently checked rows, both profiles and a later actual
        // seal.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: settle orphan events through a finite gate before ordinary publication resumes.
        // Method: real overwrite, independently checked rows, both profiles and a later actual
        // seal.
        run(true);
    }

    fn run(captured: bool) {
        let (mut fixture, _) = inherited_fixture(captured);
        let tick = encode_payload_tick(&mut fixture.reader(), 4);
        assert_eq!(tick.events.start, 5);
        let expected =
            crate::event_recovery::batch::frame(&inherited_rows(&fixture, &tick)[..2], captured)
                .unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let mut finite = store
            .begin_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        finish_finite(&mut finite);
        drop(finite);
        let frontier = finite_frontier(&store, &fixture, &budget);
        assert_eq!(frontier.event_end(), 5);
        assert_eq!(frontier.publication_end(), 1);
        assert_eq!(frontier.prior_window(), None);
        assert!(!frontier.complete());
        let mut input = fixture.input(false);
        input.losses.unknown = None;
        input.losses.predecessor = frontier.predecessor().clone();
        input.losses.event_start = 5;
        input.losses.event_inherited_end = 5;
        input.operation = AttemptOperation::Ordinary {
            window: 0,
            attempted_origin: 4,
        };
        input.losses.publication_inherited_end = 4;
        input.losses.publications = 3;
        let attempt = store
            .register_attempt(&input, &mut fixture.reader(), &budget)
            .unwrap();
        let manifest = encode_payload(&attempt, &fixture, 4, false);
        assert_eq!(manifest.events_first, 5);
        assert_eq!(manifest.losses.events, 0);
        publish_at(&mut fixture.writer, 1);
        finish_publication(&mut issue(attempt, &manifest, fixture.reader(), &budget).unwrap());
        assert_eq!(expected.height(), 2);
        let frontier = finite_frontier(&store, &fixture, &budget);
        assert_eq!(frontier.event_end(), 7);
        assert_eq!(frontier.publication_end(), 5);
    }
}

fn finish_finite(owner: &mut FiniteEvents<'_>) {
    for pass in 0..20000 {
        match owner
            .step()
            .unwrap_or_else(|error| panic!("finite pass {pass}: {error:?}"))
        {
            RowPublicationStep::Continue => {}
            RowPublicationStep::Idle => return,
            RowPublicationStep::Unresolved => panic!("finite proof unexpectedly unresolved"),
        }
    }
    panic!("finite did not converge");
}
fn finite_metadata(
    fixture: &Fixture,
    transaction: Identity,
    child: &str,
) -> crate::canonical::CanonicalMetadata {
    crate::canonical::CanonicalMetadata::read(
        &mut private::open_private_file(
            &fixture
                .root
                .join("recovery-data")
                .join(collector::recording::identity_text(transaction).to_string())
                .join(child)
                .join("metadata"),
            false,
        )
        .unwrap(),
    )
    .unwrap()
}
mod finite_events_all_loss_after_registered_overwrite {
    use super::*;
    use crate::{canonical::ReadWork, resources::ByteBudget};

    #[test]
    fn numeric() {
        // Goal: zero-row refinement settles actual complement, never a prior inherited count.
        // Method: overwrite the admitted retained exceptions under the same object after
        // Registered.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: zero-row refinement settles actual complement, never a prior inherited count.
        // Method: overwrite the admitted retained exceptions under the same object after
        // Registered.
        run(true);
    }

    #[test]
    fn lists_an_event_object_that_says_what_it_lost() {
        // Goal: an object that kept no row still names the events it covers and what they lost,
        // so a list of event objects counts them (P02.1).
        // Method: settle the refinement, then read the fragments after the empty terminals.
        let (mut fixture, _) = inherited_fixture(false);
        let budget = registration_budget();
        let store = Store::open(&fixture.root).unwrap();
        run_settled(&mut fixture, &store, &budget, false);
        let catalog = crate::canonical::catalog::Catalog::open(&fixture.root, &budget).unwrap();
        let terminals = catalog
            .event_objects_page(None, 8, &mut ReadWork::default())
            .unwrap();

        let fragments = catalog
            .event_objects_page(terminals.next.as_ref(), 8, &mut ReadWork::default())
            .unwrap();

        let recovery = fragments.objects[0].recovery;
        assert_eq!(fragments.objects.len(), 1);
        assert_eq!(recovery.rows, 0);
        assert_eq!(recovery.events_first, 1);
        assert_eq!(recovery.events_end, 5);
        assert_eq!(recovery.losses.events, 4);
    }

    fn run(captured: bool) {
        let (mut fixture, _) = inherited_fixture(captured);
        let budget = registration_budget();
        let store = Store::open(&fixture.root).unwrap();
        let transaction = run_settled(&mut fixture, &store, &budget, captured);
        let metadata = finite_metadata(&fixture, transaction, "loss");
        assert_eq!(
            metadata.kind,
            crate::transaction::CanonicalKind::FiniteEventLoss
        );
        assert_eq!(metadata.loss.inherited.event_start, 1);
        assert_eq!(metadata.loss.inherited.event_inherited_end, 1);
        assert_eq!(metadata.loss.inherited.events, 0);
        assert_eq!(metadata.loss.events_new, 4);
        assert_eq!(metadata.loss.publication_end, 1);
        assert_eq!(metadata.loss.event_end, 5);
        assert!(matches!(
            metadata.body,
            crate::canonical::metadata::Body::LossOnly
        ));
        let frontier = finite_frontier(&store, &fixture, &budget);
        assert_eq!(frontier.event_end(), 5);
        assert_eq!(frontier.publication_end(), 1);
        assert_eq!(frontier.prior_window(), None);
        assert!(!frontier.complete());
    }

    /// Overwrites what the finite events admitted while they settle, and returns their object.
    fn run_settled(
        fixture: &mut Fixture,
        store: &Store,
        budget: &Arc<ByteBudget>,
        captured: bool,
    ) -> Identity {
        let ledger = PublicationLedger::new(budget).unwrap();
        let mut finite = store
            .begin_finite_events(fixture.reader(), budget, &ledger)
            .unwrap();
        let transaction = finite_state(fixture).transaction;
        for number in 5..=6 {
            if captured {
                publish_captured_pair_at(fixture, number, number);
            } else {
                publish_numeric_pair_at(fixture, number, number);
            }
        }
        let mut reader = fixture.reader();
        reader.seek_event(3).unwrap();
        assert!(matches!(reader.read_event().unwrap(),
                collector::ring::FoundEvent::Missed { first:3, end } if end >= 5));
        finish_finite(&mut finite);
        drop(finite);

        transaction
    }
}

fn finite_rows_oracle(
    fixture: &Fixture,
    transaction: Identity,
    expected: &polars::prelude::DataFrame,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let metadata = finite_metadata(fixture, transaction, "fragment-0");
    assert_eq!(
        metadata.kind,
        crate::transaction::CanonicalKind::FiniteEvents
    );
    assert_eq!(metadata.loss.inherited.event_start, 1);
    assert_eq!(metadata.loss.inherited.event_inherited_end, 1);
    assert_eq!(metadata.loss.inherited.events, 0);
    assert_eq!(metadata.loss.events_new, 2);
    assert_eq!(metadata.loss.event_end, 5);
    assert_eq!(metadata.loss.publication_end, 1);
    assert_eq!(metadata.loss.publications_new, 0);
    assert_eq!(metadata.loss.inherited.publications, 0);
    assert_eq!(metadata.binding, fixture.reader().object_binding().unwrap());
    assert_eq!(metadata.lifecycle, crate::canonical::Lifecycle::Active);
    assert_eq!(metadata.window, 0);
    assert!(!metadata.trailing_unknown);
    let crate::canonical::metadata::Body::Events(manifest) = metadata.body else {
        panic!("finite rows");
    };
    let path = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(transaction).to_string())
        .join("fragment-0");
    let mut actual = None;
    for (ordinal, batch) in manifest.batches.iter().enumerate() {
        let frame = crate::event_recovery::read::read_batch_file(
            private::open_private_file(
                &path.join(crate::event_recovery::batch_name(ordinal)),
                false,
            )
            .unwrap(),
            batch,
            metadata.captured,
            true,
            budget,
        )
        .unwrap();
        if let Some(actual) = &mut actual {
            polars::prelude::DataFrame::vstack_mut(actual, &frame).unwrap();
        } else {
            actual = Some(frame);
        }
    }
    assert!(actual.unwrap().equals_missing(expected));
    assert!(!path.parent().unwrap().join("pending").exists());
    assert!(!path.parent().unwrap().join("original-census").exists());
}
mod finite_events_true_store_reopen_at_durable_boundaries {
    use super::*;

    #[test]
    fn numeric_registered() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(false, 0);
    }

    #[test]
    fn numeric_partial_census() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(false, 1);
    }

    #[test]
    fn numeric_planned() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(false, 2);
    }

    #[test]
    fn numeric_candidate() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(false, 3);
    }

    #[test]
    fn numeric_metadata() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(false, 4);
    }

    #[test]
    fn numeric_publishing() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(false, 5);
    }

    #[test]
    fn numeric_published() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(false, 6);
    }

    #[test]
    fn numeric_cleanup() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(false, 7);
    }

    #[test]
    fn numeric_retirement() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(false, 8);
    }

    #[test]
    fn captured_registered() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(true, 0);
    }

    #[test]
    fn captured_partial_census() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(true, 1);
    }

    #[test]
    fn captured_planned() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(true, 2);
    }

    #[test]
    fn captured_candidate() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(true, 3);
    }

    #[test]
    fn captured_metadata() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(true, 4);
    }

    #[test]
    fn captured_publishing() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(true, 5);
    }

    #[test]
    fn captured_published() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(true, 6);
    }

    #[test]
    fn captured_cleanup() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(true, 7);
    }

    #[test]
    fn captured_retirement() {
        // Goal: Store reacquisition preserves fixed intent, exact rows and independent frontiers.
        // Method: drop Store at each substantive construction/publication/cleanup boundary.
        run(true, 8);
    }

    fn run(captured: bool, barrier: u8) {
        eprintln!(
            "finite reopen captured={captured} barrier={barrier} size={}",
            size_of::<FiniteEvents>()
        );
        let (fixture, _) = inherited_fixture(captured);
        let tick = encode_payload_tick(&mut fixture.reader(), 4);
        let expected =
            crate::event_recovery::batch::frame(&inherited_rows(&fixture, &tick)[..2], captured)
                .unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .begin_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        let initial = finite_state(&fixture);
        let transaction = initial.transaction;
        let object = fixture
            .root
            .join("recovery-data")
            .join(collector::recording::identity_text(transaction).to_string());
        let mut reached = false;
        for _ in 0..10000 {
            let state = finite_state(&fixture);
            let census = fs::metadata(object.join("original-census"))
                .ok()
                .map(|file| file.len());
            let metadata = object.join("fragment-0/metadata").exists();
            reached = finite_reopen_barrier(barrier, &state, &object, census, metadata);
            if reached {
                break;
            }
            assert_eq!(owner.step().unwrap(), RowPublicationStep::Continue);
        }
        assert!(reached, "barrier {barrier}");
        drop(owner);
        drop(store);
        let store = Store::open(&fixture.root).unwrap();
        let source = if barrier >= 4 {
            None
        } else {
            Some(fixture.reader())
        };
        let mut owner = store
            .resume_finite_events(source, &budget, &ledger)
            .unwrap()
            .unwrap();
        finish_finite(&mut owner);
        drop(owner);
        finite_rows_oracle(&fixture, transaction, &expected, &budget);
        let frontier = finite_frontier(&store, &fixture, &budget);
        assert_eq!(frontier.event_end(), 5);
        assert_eq!(frontier.publication_end(), 1);
        assert_eq!(frontier.prior_window(), None);
        assert!(!frontier.complete());
    }
}

mod finite_events_absent_unproved_source_keeps_phase8_then_resumes {
    use super::*;

    #[test]
    fn numeric_without_candidate() {
        // Goal: absent source is occupied uncertainty until actual matching proof returns.
        // Method: cold reopen before census and after candidate output, with no completed metadata.
        run(false, false);
    }

    #[test]
    fn numeric_with_candidate() {
        // Goal: absent source is occupied uncertainty until actual matching proof returns.
        // Method: cold reopen before census and after candidate output, with no completed metadata.
        run(false, true);
    }

    #[test]
    fn captured_without_candidate() {
        // Goal: absent source is occupied uncertainty until actual matching proof returns.
        // Method: cold reopen before census and after candidate output, with no completed metadata.
        run(true, false);
    }

    #[test]
    fn captured_with_candidate() {
        // Goal: absent source is occupied uncertainty until actual matching proof returns.
        // Method: cold reopen before census and after candidate output, with no completed metadata.
        run(true, true);
    }

    fn run(captured: bool, candidate: bool) {
        let (fixture, _) = inherited_fixture(captured);
        let tick = encode_payload_tick(&mut fixture.reader(), 4);
        let expected =
            crate::event_recovery::batch::frame(&inherited_rows(&fixture, &tick)[..2], captured)
                .unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .begin_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        let initial = finite_state(&fixture);
        let object = fixture
            .root
            .join("recovery-data")
            .join(collector::recording::identity_text(initial.transaction).to_string());
        if candidate {
            for _ in 0..10000 {
                if object.join("fragment-0/0000.parquet").exists() {
                    break;
                }
                assert_eq!(owner.step().unwrap(), RowPublicationStep::Continue);
            }
            assert!(object.join("fragment-0/0000.parquet").exists());
        }
        drop(owner);
        drop(store);
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .resume_finite_events(None, &budget, &ledger)
            .unwrap()
            .unwrap();
        let mut unresolved = false;
        for _ in 0..10000 {
            if owner.step().unwrap() == RowPublicationStep::Unresolved {
                unresolved = true;
                break;
            }
        }
        assert!(unresolved);
        drop(owner);
        let state = finite_state(&fixture);
        assert_eq!(state.transaction, initial.transaction);
        assert_eq!(state.input, initial.input);
        assert_eq!(state.phase, Phase::UnresolvedExtent);
        assert_eq!(state.census.kind, 2);
        assert_eq!(state.sequence, 0);
        assert_eq!(state.next, 0);
        assert!(state.structure.is_none());
        assert!(object.join("pending").exists());
        assert!(!object.join("fragment-0/metadata").exists());
        drop(store);
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .resume_finite_events(Some(fixture.reader()), &budget, &ledger)
            .unwrap()
            .unwrap();
        finish_finite(&mut owner);
        drop(owner);
        finite_rows_oracle(&fixture, state.transaction, &expected, &budget);
    }
}

#[path = "finite_events_native_tests.rs"]
mod finite_events_native_tests;

fn finite_expected_rows(
    fixture: &Fixture,
    range: std::ops::Range<u64>,
) -> Vec<crate::event_recovery::batch::Row> {
    assert!(range.end - range.start <= 1024);
    let mut reader = fixture.reader();
    let captured = reader.dictionary_capacity().is_some();
    let mut rows = Vec::with_capacity(usize::try_from(range.end - range.start).unwrap());
    reader.seek_event(range.start).unwrap();
    for expected in range {
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
            panic!("expected actual source row {expected}");
        };
        assert_eq!(number, expected);
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
fn finite_domain_oracle(
    fixture: &Fixture,
    transaction: Identity,
    first: u64,
    end: u64,
    expected: &[crate::event_recovery::batch::Row],
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let child = if expected.is_empty() {
        "loss"
    } else {
        "fragment-0"
    };
    let metadata = finite_metadata(fixture, transaction, child);
    assert_eq!(
        metadata.kind,
        if expected.is_empty() {
            crate::transaction::CanonicalKind::FiniteEventLoss
        } else {
            crate::transaction::CanonicalKind::FiniteEvents
        }
    );
    assert_eq!(metadata.loss.inherited.event_start, first);
    assert_eq!(metadata.loss.inherited.event_inherited_end, first);
    assert_eq!(metadata.loss.inherited.events, 0);
    assert_eq!(metadata.loss.event_end, end);
    assert_eq!(
        metadata.loss.events_new,
        end - first - collector::u64_from_usize(expected.len())
    );
    assert_eq!(metadata.loss.inherited.publications, 0);
    assert_eq!(metadata.loss.publications_new, 0);
    assert_eq!(
        metadata.loss.inherited.publication_start,
        metadata.loss.publication_end
    );
    assert_eq!(
        metadata.loss.inherited.publication_inherited_end,
        metadata.loss.publication_end
    );
    finite_domain_rows(fixture, transaction, child, &metadata, expected, budget);
}

mod finite_events_nonprefix_live_interleaving_rescues_retained_rows {
    use super::*;

    #[test]
    fn numeric_retained() {
        // Goal: Event→Missed→Event is valid and durable rescue defines positional complement.
        // Method: retain128 rows, genuinely wrap one object before raw durability, exclude new
        // suffix.
        run(false, false);
    }

    #[test]
    fn numeric_abandoned() {
        // Goal: Event→Missed→Event is valid and durable rescue defines positional complement.
        // Method: retain128 rows, genuinely wrap one object before raw durability, exclude new
        // suffix.
        run(false, true);
    }

    #[test]
    fn captured_retained() {
        // Goal: Event→Missed→Event is valid and durable rescue defines positional complement.
        // Method: retain128 rows, genuinely wrap one object before raw durability, exclude new
        // suffix.
        run(true, false);
    }

    #[test]
    fn captured_abandoned() {
        // Goal: Event→Missed→Event is valid and durable rescue defines positional complement.
        // Method: retain128 rows, genuinely wrap one object before raw durability, exclude new
        // suffix.
        run(true, true);
    }

    fn run(captured: bool, abandon: bool) {
        let mut fixture = proof_fixture(captured, 1);
        inherited_large_source(&mut fixture, captured);
        finite_publish_four(&mut fixture, captured, 42..=175);
        let mut expected = finite_expected_rows(&fixture, 185..313);
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .begin_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        let initial = finite_state(&fixture);
        let plan = initial.row_plan.unwrap();
        assert_eq!(plan.first, 1);
        assert_eq!(plan.end, 693);
        assert_eq!(plan.source_first, 185);
        assert_eq!(plan.source_end, 693);
        let before = ledger.work();
        assert_eq!(owner.step().unwrap(), RowPublicationStep::Continue);
        assert_eq!(ledger.work().source_reads - before.source_reads, 128);
        let original = fixture
            .root
            .join("recovery-data")
            .join(collector::recording::identity_text(initial.transaction).to_string())
            .join("pending");
        assert_eq!(fs::read_dir(&original).unwrap().count(), 0);
        finite_publish_four(&mut fixture, captured, 176..=257);
        let mut source = fixture.reader();
        source.seek_event(313).unwrap();
        assert!(matches!(
            source.read_event().unwrap(),
            collector::ring::FoundEvent::Missed {
                first: 313,
                end: 513
            }
        ));
        assert!(matches!(
            source.read_event().unwrap(),
            collector::ring::FoundEvent::Event { number: 513, .. }
        ));
        expected.extend(finite_expected_rows(&fixture, 513..693));
        if abandon {
            // Volatile rows have no settled loss count. Restart may lose these exceptions.
            expected.drain(..128);
            drop(owner);
            drop(store);
            let store = Store::open(&fixture.root).unwrap();
            let mut owner = store
                .resume_finite_events(Some(fixture.reader()), &budget, &ledger)
                .unwrap()
                .unwrap();
            finish_finite(&mut owner);
            drop(owner);
            finite_domain_oracle(&fixture, initial.transaction, 1, 693, &expected, &budget);
        } else {
            finish_finite(&mut owner);
            drop(owner);
            finite_domain_oracle(&fixture, initial.transaction, 1, 693, &expected, &budget);
            let frontier = finite_frontier(&store, &fixture, &budget);
            assert_eq!(frontier.event_end(), 693);
            assert_eq!(frontier.publication_end(), 1);
        }
    }
}
mod finite_events_all_retained_and_canonical_predecessor {
    use super::*;

    #[test]
    fn numeric_genesis() {
        // Goal: finite ownership keeps prior saved-window/publication ownership independent.
        // Method: all-retained genesis and a real canonical predecessor, both actual capture
        // profiles.
        run(false, false);
    }

    #[test]
    fn numeric_predecessor() {
        // Goal: finite ownership keeps prior saved-window/publication ownership independent.
        // Method: all-retained genesis and a real canonical predecessor, both actual capture
        // profiles.
        run(false, true);
    }

    #[test]
    fn captured_genesis() {
        // Goal: finite ownership keeps prior saved-window/publication ownership independent.
        // Method: all-retained genesis and a real canonical predecessor, both actual capture
        // profiles.
        run(true, false);
    }

    #[test]
    fn captured_predecessor() {
        // Goal: finite ownership keeps prior saved-window/publication ownership independent.
        // Method: all-retained genesis and a real canonical predecessor, both actual capture
        // profiles.
        run(true, true);
    }

    fn run(captured: bool, predecessor: bool) {
        let fixture = if predecessor {
            inherited_predecessor_fixture(captured).0
        } else {
            let mut fixture = proof_fixture(captured, 1);
            for number in 2..=3 {
                if captured {
                    publish_captured_pair_at(&mut fixture, number, number);
                } else {
                    publish_numeric_pair_at(&mut fixture, number, number);
                }
            }
            fixture
        };
        let end = if predecessor { 5 } else { 3 };
        let expected = finite_expected_rows(&fixture, if predecessor { 3..5 } else { 1..3 });
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .begin_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        let state = finite_state(&fixture);
        finish_finite(&mut owner);
        drop(owner);
        finite_domain_oracle(&fixture, state.transaction, 1, end, &expected, &budget);
        let frontier = finite_frontier(&store, &fixture, &budget);
        assert_eq!(frontier.event_end(), end);
        assert_eq!(frontier.publication_end(), if predecessor { 2 } else { 1 });
        assert_eq!(
            frontier.prior_window(),
            if predecessor { Some(0) } else { None }
        );
        assert!(!frontier.complete());
    }
}

mod finite_events_budget_source_binding_and_kind_aliases_refuse {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: no budget/source/metadata alias can bypass fresh finite construction authority.
        // Method: exhausted shared budget, actual replacement, exact metadata kind/reserved
        // mutations.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: no budget/source/metadata alias can bypass fresh finite construction authority.
        // Method: exhausted shared budget, actual replacement, exact metadata kind/reserved
        // mutations.
        run(true);
    }

    fn run(captured: bool) {
        let (mut fixture, _) = inherited_fixture(captured);
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let idle = fixture.slot();
        let reservation = budget.reserve(budget.maximum() - budget.used()).unwrap();
        assert_eq!(
            store
                .begin_finite_events(fixture.reader(), &budget, &ledger)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::OutOfMemory
        );
        assert_eq!(fixture.slot(), idle);
        assert_eq!(
            fs::read_dir(fixture.root.join("recovery-data"))
                .unwrap()
                .count(),
            0
        );
        drop(reservation);
        let owner = store
            .begin_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        let state = finite_state(&fixture);
        let slot = fixture.slot();
        drop(owner);
        drop(store);
        inherited_large_source(&mut fixture, captured);
        let store = Store::open(&fixture.root).unwrap();
        assert_eq!(
            store
                .resume_finite_events(Some(fixture.reader()), &budget, &ledger)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fixture.slot(), slot);
        assert!(
            fixture
                .root
                .join("recovery-data")
                .join(collector::recording::identity_text(state.transaction).to_string())
                .join("pending")
                .exists()
        );
        drop(store);
        finite_events_kind_aliases(captured, &budget, &ledger);
    }
}

mod finite_events_retained_capability_preserves_unsaved_association {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: retained rows carry source facts, native bytes and unsaved publication association.
        // Method: capture a real tick, overwrite it under one object, then settle and seal
        // ordinary.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: retained rows carry source facts, native bytes and unsaved publication association.
        // Method: capture a real tick, overwrite it under one object, then settle and seal
        // ordinary.
        run(true);
    }

    fn run(captured: bool) {
        let mut fixture = proof_fixture(captured, 1);
        if captured {
            publish_captured_events_at(&mut fixture, 2, 2, 1);
        } else {
            publish_numeric_events_at(&mut fixture, 2, 2, 1);
        }
        let mut expected = finite_expected_rows(&fixture, 1..2);
        expected[0].publication = Some(2);
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let idle = fixture.slot();
        let mut retained = store
            .capture_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        assert!(retained.step().unwrap());
        assert_eq!(fixture.slot(), idle);
        inherited_empty(&mut fixture, 3);
        for number in 4..=5 {
            if captured {
                publish_captured_pair_at(&mut fixture, number, number);
            } else {
                publish_numeric_pair_at(&mut fixture, number, number);
            }
        }
        let mut reader = fixture.reader();
        reader.seek_event(1).unwrap();
        assert!(matches!(
            reader.read_event().unwrap(),
            collector::ring::FoundEvent::Missed { first: 1, end: 2 }
        ));
        expected.extend(finite_expected_rows(&fixture, 2..4));
        let mut owner = retained.register().unwrap();
        let state = finite_state(&fixture);
        assert_eq!(state.row_plan.unwrap().end, 4);
        assert_eq!(state.row_plan.unwrap().source_first, 2);
        finish_finite(&mut owner);
        drop(owner);
        finite_domain_oracle(&fixture, state.transaction, 1, 4, &expected, &budget);
        let frontier = finite_frontier(&store, &fixture, &budget);
        assert_eq!(frontier.publication_end(), 1);
        assert_eq!(frontier.event_end(), 4);
        assert_eq!(frontier.prior_window(), None);
        finite_retained_ordinary(&mut fixture, &store, &budget);
        let frontier = finite_frontier(&store, &fixture, &budget);
        assert_eq!(frontier.event_end(), 6);
        assert_eq!(frontier.publication_end(), 6);
    }
}

#[path = "finite_split_tests.rs"]
mod finite_split_tests;

mod finite_events_native_usage_nulls_and_associations_are_exact {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: finite output preserves every measured counter, nullable field and native identity.
        // Method: actual Start/Exec/measured/Gone producer events, retained before genuine
        // overwrite.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: finite output preserves every measured counter, nullable field and native identity.
        // Method: actual Start/Exec/measured/Gone producer events, retained before genuine
        // overwrite.
        run(true);
    }

    fn run(captured: bool) {
        let mut fixture = proof_fixture(captured, 1);
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
        finite_usage_publish(&mut fixture, captured, usage);
        let mut expected = finite_expected_rows(&fixture, 1..5);
        for row in &mut expected {
            row.publication = Some(2);
        }
        assert_eq!(expected[0].event.coalition, Some(77));
        assert_eq!(expected[1].event.usage, Some(usage));
        assert!(expected[3].event.usage.is_none());
        if captured {
            assert_eq!(expected[1].name.as_deref(), Some(b"native-\xff".as_slice()));
            assert_eq!(expected[1].app.as_deref(), Some(b"/A-\xfe.app".as_slice()));
            assert_eq!(
                expected[1]
                    .identity
                    .as_ref()
                    .unwrap()
                    .creation
                    .unwrap()
                    .unique_id,
                991
            );
            assert!(expected[3].identity.as_ref().unwrap().creation.is_none());
        }
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let mut rows = store
            .capture_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        assert!(rows.step().unwrap());
        inherited_empty(&mut fixture, 3);
        if captured {
            publish_captured_pair_at(&mut fixture, 4, 4);
        } else {
            publish_numeric_pair_at(&mut fixture, 4, 4);
        }
        let mut owner = rows.register().unwrap();
        let transaction = finite_state(&fixture).transaction;
        finish_finite(&mut owner);
        drop(owner);
        finite_domain_oracle(&fixture, transaction, 1, 5, &expected, &budget);
    }
}

mod finite_events_associated_original_conflicts_refuse_exact_native_facts {
    use super::*;

    #[test]
    fn numeric_association() {
        // Goal: persisted association is preserved, while conflicting common source facts refuse.
        // Method: valid measured associated rows; change an available row's tick or native bytes.
        run(false, false);
    }

    #[test]
    fn captured_association() {
        // Goal: persisted association is preserved, while conflicting common source facts refuse.
        // Method: valid measured associated rows; change an available row's tick or native bytes.
        run(true, false);
    }

    #[test]
    fn captured_native_bytes() {
        // Goal: persisted association is preserved, while conflicting common source facts refuse.
        // Method: valid measured associated rows; change an available row's tick or native bytes.
        run(true, true);
    }

    fn run(captured: bool, native: bool) {
        if native {
            assert!(captured);
        }

        let mut fixture = proof_fixture(captured, 1);
        if captured {
            publish_captured_events_at(&mut fixture, 2, 2, 4);
        } else {
            publish_numeric_events_at(&mut fixture, 2, 2, 4);
        }
        let mut rows = finite_expected_rows(&fixture, 1..5);
        for row in &mut rows {
            row.publication = Some(2);
        }
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let mut captured_rows = store
            .capture_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        assert!(captured_rows.step().unwrap());
        inherited_empty(&mut fixture, 3);
        if captured {
            publish_captured_pair_at(&mut fixture, 4, 4);
        } else {
            publish_numeric_pair_at(&mut fixture, 4, 4);
        }
        let mut owner = captured_rows.register().unwrap();
        let transaction = finite_state(&fixture).transaction;
        let original = fixture
            .root
            .join("recovery-data")
            .join(collector::recording::identity_text(transaction).to_string())
            .join("pending/events");
        for _ in 0..100 {
            if original.join("index").exists() {
                break;
            }
            assert_eq!(owner.step().unwrap(), RowPublicationStep::Continue);
        }
        assert!(original.join("index").exists());
        drop(owner);
        drop(store);
        finite_conflict_refuses(
            &fixture, &original, &mut rows, captured, native, &budget, &ledger,
        );
    }
}
mod finite_events_immutable_header_failure_has_no_source_progress {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: binding refusal charges the attempted observation and preserves occupied inputs.
        // Method: mutate and restore an actual immutable word before one construction read.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: binding refusal charges the attempted observation and preserves occupied inputs.
        // Method: mutate and restore an actual immutable word before one construction read.
        run(true);
    }

    fn run(captured: bool) {
        let (fixture, _) = inherited_fixture(captured);
        let mut header = WritableHeader::open(&fixture);
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let mut owner = store
            .begin_finite_events(fixture.reader(), &budget, &ledger)
            .unwrap();
        let slot = fixture.slot();
        let before = ledger.work();
        let position = owner.construction_position();
        let old = header.word(40).fetch_xor(1, Ordering::Release);
        let result = owner.step();
        header.word(40).store(old, Ordering::Release);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fixture.slot(), slot);
        assert_eq!(owner.construction_position(), position);
        assert_eq!(ledger.work().source_reads - before.source_reads, 0);
        assert_eq!(
            ledger.work().source_observations - before.source_observations,
            1
        );
        let transaction =
            crate::transaction::normal::NormalState::decode(&slot, fixture.run.recording)
                .unwrap()
                .transaction;
        let tick = encode_payload_tick(&mut fixture.reader(), 4);
        let expected =
            crate::event_recovery::batch::frame(&inherited_rows(&fixture, &tick)[..2], captured)
                .unwrap();
        finish_finite(&mut owner);
        drop(owner);
        finite_rows_oracle(&fixture, transaction, &expected, &budget);
    }
}

fn finite_state(fixture: &Fixture) -> crate::transaction::normal::NormalState {
    crate::transaction::normal::NormalState::decode(&fixture.slot(), fixture.run.recording).unwrap()
}
fn finite_frontier(
    store: &Store,
    fixture: &Fixture,
    budget: &Arc<crate::resources::ByteBudget>,
) -> crate::canonical::SourceFrontier {
    store
        .source_frontier(
            &fixture.run,
            budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap()
}

fn finite_events_kind_aliases(
    captured: bool,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
) {
    // A different isolated transaction supplies independently readable completed metadata.
    let (fixture, _) = inherited_fixture(captured);
    let store = Store::open(&fixture.root).unwrap();
    let mut owner = store
        .begin_finite_events(fixture.reader(), budget, ledger)
        .unwrap();
    let transaction = finite_state(&fixture).transaction;
    finish_finite(&mut owner);
    drop(owner);
    let metadata = finite_metadata(&fixture, transaction, "fragment-0");
    let mut bytes = Vec::new();
    metadata.write(&mut bytes).unwrap();
    assert_eq!(&bytes[..8], b"FDCAN002");
    assert_eq!(u32::from_be_bytes(bytes[12..16].try_into().unwrap()), 5);
    assert_eq!(u32::from_be_bytes(bytes[80..84].try_into().unwrap()), 2);
    assert_eq!(&bytes[64..80], &bytes[88..104]);
    assert_eq!(&bytes[112..136], &[0; 24]);
    assert_eq!(u32::from_be_bytes(bytes[168..172].try_into().unwrap()), 3);
    assert_eq!(bytes.len(), 512 + 128 + 24);
    finite_kind_grammar_control(&fixture, &bytes, 3);
    for offset in [88, 152, 168, 172, 180, 184, 188, 196, 255] {
        let mut changed = bytes.clone();
        changed[offset] ^= 128;
        let path = fixture
            .base
            .join(format!("finite-invalid-metadata-{offset}"));
        private::open_private_file(&path, true)
            .unwrap()
            .write_all(&changed)
            .unwrap();
        assert!(
            crate::canonical::CanonicalMetadata::read(
                &mut private::open_private_file(&path, false).unwrap()
            )
            .is_err(),
            "metadata byte {offset}"
        );
    }
}

fn finite_usage_publish(fixture: &mut Fixture, captured: bool, usage: collector::processes::Usage) {
    let events = finite_usage_events(usage);
    let memory = collector::machine::Memory::default();
    let readings = collector::ring::Readings {
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
        events: &events,
        dropped: collector::ring::Dropped::default(),
    };
    let times = collector::ring::ObservationTimes {
        wall_ns: 2,
        continuous_ns: 2,
        awake_ns: 2,
    };
    if captured {
        let mut program = None;
        fixture
            .writer
            .intern_program(
                &collector::ring::ProgramBytes {
                    executable_name: b"native-\xff",
                    app_path: Some(b"/A-\xfe.app"),
                },
                &mut program,
            )
            .unwrap();
        let measured = collector::ring::ProcessIdentity::Measured {
            creation: collector::ring::ProcessCreation {
                unique_id: 991,
                pid_version: 7,
            },
            program: program.unwrap(),
        };
        let identities = finite_usage_identities(measured);
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

fn finite_usage_identities(
    measured: collector::ring::ProcessIdentity,
) -> [collector::ring::IdentityObservation; 4] {
    [
        collector::ring::IdentityObservation {
            pid: 41,
            usage: collector::ring::UsageAvailability::NotApplicable,
            identity: measured,
        },
        collector::ring::IdentityObservation {
            pid: 41,
            usage: collector::ring::UsageAvailability::Measured,
            identity: measured,
        },
        collector::ring::IdentityObservation {
            pid: 41,
            usage: collector::ring::UsageAvailability::NotApplicable,
            identity: measured,
        },
        collector::ring::IdentityObservation {
            pid: 41,
            usage: collector::ring::UsageAvailability::Gone,
            identity: collector::ring::ProcessIdentity::Unavailable {
                creation: None,
                reason: collector::ring::IdentityUnavailable::Gone,
            },
        },
    ]
}

fn finite_publish_four(
    fixture: &mut Fixture,
    captured: bool,
    range: std::ops::RangeInclusive<u64>,
) {
    for number in range {
        if captured {
            publish_captured_events_at(fixture, number, number, 4);
        } else {
            publish_numeric_events_at(fixture, number, number, 4);
        }
    }
}

fn finite_reopen_barrier(
    barrier: u8,
    state: &crate::transaction::normal::NormalState,
    object: &Path,
    census: Option<u64>,
    metadata: bool,
) -> bool {
    match barrier {
        0 => state.phase == Phase::Registered && state.census.kind == 0,
        1 => census.is_some_and(|length| length > 256 && length < 394_720),
        2 => state.phase == Phase::SalvagePlanned && !object.join("fragment-0").exists(),
        3 => object.join("fragment-0/0000.parquet").exists() && !metadata,
        4 => metadata && state.phase == Phase::SalvagePlanned,
        5 => state.phase == Phase::Publishing,
        6 => state.phase == Phase::Published,
        7 => state.phase == Phase::SalvageCommitted && state.cleanup.ordinal > 0,
        8 => state.cleanup.region == 8,
        _ => unreachable!(),
    }
}

fn finite_conflict_refuses(
    fixture: &Fixture,
    original: &Path,
    rows: &mut [crate::event_recovery::batch::Row],
    captured: bool,
    native: bool,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
) {
    if native {
        rows[2].name = Some(b"changed-\xff".to_vec());
    } else {
        rows[2].event.tick = 3;
    }
    let mut changed = crate::event_recovery::batch::frame(rows, captured).unwrap();
    crate::event_recovery::batch::validate_frame(&changed, captured).unwrap();
    let replacement = fixture.base.join("replacement-original.parquet");
    crate::encoder::encode_write(
        &mut changed,
        private::open_private_file(&replacement, true).unwrap(),
    )
    .unwrap();
    fs::rename(&replacement, original.join("0000.parquet")).unwrap();
    let bytes = fs::read(original.join("0000.parquet")).unwrap();
    let store = Store::open(&fixture.root).unwrap();
    let mut owner = store
        .resume_finite_events(Some(fixture.reader()), budget, ledger)
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
            Ok(result) => panic!("conflict accepted: {result:?}"),
        }
    }
    assert!(refused);
    assert_eq!(fs::read(original.join("0000.parquet")).unwrap(), bytes);
    assert!(original.parent().unwrap().exists());
    assert!(
        store
            .source_frontier(
                &fixture.run,
                budget,
                &mut crate::canonical::ReadWork::default()
            )
            .unwrap()
            .is_none()
    );
}

fn finite_usage_events(usage: collector::processes::Usage) -> [collector::tracker::Event; 4] {
    [
        collector::tracker::Event::Start {
            pid: 41,
            coalition: Some(77),
        },
        collector::tracker::Event::Exit {
            pid: 41,
            last: Ok(usage),
            noticed: collector::tracker::Noticed::OnExit,
        },
        collector::tracker::Event::Exec { pid: 41 },
        collector::tracker::Event::Exit {
            pid: 41,
            last: Err(collector::processes::Unreadable::Gone),
            noticed: collector::tracker::Noticed::AlreadyExited,
        },
    ]
}

mod finite_events_consumes_prepared_frontier_without_direct_registration_bypass {
    use super::*;

    #[test]
    fn numeric() {
        // Goal: default and prepared finite admission share actual source/frontier authority.
        // Method: reject the old count transfer, then consume a completed query and exact rows.
        run(false);
    }

    #[test]
    fn captured() {
        // Goal: default and prepared finite admission share actual source/frontier authority.
        // Method: reject the old count transfer, then consume a completed query and exact rows.
        run(true);
    }

    fn run(captured: bool) {
        let (fixture, input) = inherited_fixture(captured);
        let tick = encode_payload_tick(&mut fixture.reader(), 4);
        let expected =
            crate::event_recovery::batch::frame(&inherited_rows(&fixture, &tick)[..2], captured)
                .unwrap();
        let budget = registration_budget();
        let ledger = PublicationLedger::new(&budget).unwrap();
        let store = Store::open(&fixture.root).unwrap();
        let root = PrivateDirectory::open(&fixture.root).unwrap();
        let mut state = [0; 256];
        let mut query =
            crate::canonical::SourceFrontierRead::begin(&root, fixture.run, &mut state, &budget)
                .unwrap();
        for _ in 0..3 {
            query.step().unwrap();
        }
        assert!(query.result().unwrap().is_none());
        assert_eq!(
            store
                .register_attempt_from_frontier(&input, &mut fixture.reader(), query,)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Unsupported
        );
        let mut query =
            crate::canonical::SourceFrontierRead::begin(&root, fixture.run, &mut state, &budget)
                .unwrap();
        for _ in 0..3 {
            query.step().unwrap();
        }
        let mut owner = store
            .begin_finite_events_from_frontier(fixture.reader(), query, &budget, &ledger)
            .unwrap();
        drop(root);
        let transaction = finite_state(&fixture).transaction;
        finish_finite(&mut owner);
        drop(owner);
        finite_rows_oracle(&fixture, transaction, &expected, &budget);
        assert_eq!(finite_frontier(&store, &fixture, &budget).event_end(), 5);
        assert!(ledger.work().canonical.namespace_attempts > 0);
    }
}

fn finite_domain_rows(
    fixture: &Fixture,
    transaction: Identity,
    child: &str,
    metadata: &crate::canonical::CanonicalMetadata,
    expected: &[crate::event_recovery::batch::Row],
    budget: &Arc<crate::resources::ByteBudget>,
) {
    if let crate::canonical::metadata::Body::Events(manifest) = &metadata.body {
        let path = fixture
            .root
            .join("recovery-data")
            .join(collector::recording::identity_text(transaction).to_string())
            .join(child);
        let mut actual = polars::prelude::DataFrame::empty();
        for (ordinal, batch) in manifest.batches.iter().enumerate() {
            let frame = crate::event_recovery::read::read_batch_file(
                private::open_private_file(
                    &path.join(crate::event_recovery::batch_name(ordinal)),
                    false,
                )
                .unwrap(),
                batch,
                metadata.captured,
                true,
                budget,
            )
            .unwrap();
            if ordinal == 0 {
                actual = frame;
            } else {
                actual.vstack_mut(&frame).unwrap();
            }
        }
        assert!(actual.equals_missing(
            &crate::event_recovery::batch::frame(expected, metadata.captured).unwrap()
        ));
    } else {
        assert!(expected.is_empty());
    }
}
fn finite_retained_ordinary(
    fixture: &mut Fixture,
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let frontier = finite_frontier(store, fixture, budget);
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 5,
    };
    input.losses.predecessor = frontier.predecessor().clone();
    input.losses.event_start = 4;
    input.losses.event_inherited_end = 4;
    input.losses.publication_inherited_end = 5;
    input.losses.publications = 4;
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), budget)
        .unwrap();
    let manifest = encode_payload(&attempt, fixture, 5, false);
    assert_eq!(manifest.events_first, 4);
    assert_eq!(manifest.events_end, 6);
    assert_eq!(manifest.losses.events, 0);
    publish_at(&mut fixture.writer, 1);
    finish_publication(&mut issue(attempt, &manifest, fixture.reader(), budget).unwrap());
}

fn finite_kind_grammar_control(fixture: &Fixture, bytes: &[u8], kind: u8) {
    let mut overlap = bytes.to_vec();
    overlap[15] = kind;
    let path = fixture.base.join(format!("finite-kind-grammar-{kind}"));
    private::open_private_file(&path, true)
        .unwrap()
        .write_all(&overlap)
        .unwrap();
    let admitted = crate::canonical::CanonicalMetadata::read(
        &mut private::open_private_file(&path, false).unwrap(),
    )
    .unwrap();
    assert_eq!(
        admitted.kind,
        if kind == 3 {
            crate::transaction::CanonicalKind::Fragment
        } else {
            crate::transaction::CanonicalKind::LossOnly
        }
    );
    assert_eq!(admitted.lifecycle, crate::canonical::Lifecycle::Active);
}

#[path = "finite_kind_tests.rs"]
mod finite_kind_tests;

#[path = "finite_frontier_tests.rs"]
mod finite_frontier_tests;

#[path = "finite_review_tests.rs"]
mod finite_review_tests;
