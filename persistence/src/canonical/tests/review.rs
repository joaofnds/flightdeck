use super::*;
use polars::prelude::*;

fn terminal_after(fixture: &Fixture, prior: &CanonicalMetadata) -> CanonicalMetadata {
    let mut terminal = event_metadata(fixture, false);
    terminal.transaction = Identity([40, 41]);
    terminal.lifecycle = Lifecycle::AcquiredCompletion;
    terminal.loss = CanonicalLoss {
        inherited: LossTransfer {
            predecessor: Predecessor::Canonical {
                transaction: prior.transaction,
                kind: prior.kind,
                origin: prior.canonical_origin,
            },
            publication_start: prior.loss.publication_end,
            publication_inherited_end: prior.loss.publication_end,
            event_start: prior.loss.event_end,
            event_inherited_end: prior.loss.event_end,
            publications: 0,
            events: 0,
            unknown: None,
            prior_window: prior.effective_window(),
        },
        publication_end: prior.loss.publication_end,
        event_end: prior.loss.event_end,
        publications_new: 0,
        events_new: 0,
        unknown_flags: 0,
    };
    terminal.body = Body::Events(crate::event_recovery::Manifest {
        recovery: crate::event_recovery::Recovery {
            run: fixture.run,
            events_first: prior.loss.event_end,
            events_end: prior.loss.event_end,
            rows: 0,
            losses: crate::Losses::default(),
        },
        identities: false,
        batches: vec![],
    });
    terminal
}

fn publish_event(fixture: &Fixture, metadata: &CanonicalMetadata, namespace: Namespace) {
    let (raw, _) = fixture.publish(metadata, namespace, &[]);
    let mut frame = event_frame(false);
    frame
        .with_column(Column::new(
            "event".into(),
            [metadata.loss.inherited.event_start],
        ))
        .unwrap();
    crate::encoder::encode_write(
        &mut frame,
        PrivateDirectory::open(&raw)
            .unwrap()
            .create_file("0000.parquet")
            .unwrap(),
    )
    .unwrap();
}

#[test]
fn equal_data_frontier_preserves_terminal_lifecycle_progress() {
    // Goal: an empty terminal can certify completion after the final ordinary seal.
    // Method: admit distinct real metadata in both namespaces with identical data endpoints.
    let fixture = Fixture::new();
    let (ordinary, _) = ordinary(&fixture);
    fixture.publish(&ordinary, Namespace::Ordinary, &[]);
    let terminal = terminal_after(&fixture, &ordinary);
    fixture.publish(&terminal, Namespace::Terminal, &[]);
    let frontier = fixture.frontier().0;
    assert_eq!(frontier.publication_end(), 14);
    assert_eq!(frontier.event_end(), 100);
    assert_eq!(frontier.prior_window(), Some(10));
    assert!(frontier.complete());
}

#[test]
fn dead_generation_with_fixed_endpoint_certifies_terminal_rows_and_losses() {
    // Goal: independent death and endpoint evidence has the same finality as acquired completion.
    // Method: reopen actual terminal row and loss-only gates carrying the two independent facts.
    for loss_only in [false, true] {
        let fixture = Fixture::new();
        let mut terminal = event_metadata(&fixture, false);
        terminal.lifecycle = Lifecycle::DeadGeneration;
        if loss_only {
            terminal.kind = CanonicalKind::LossOnly;
            terminal.child = RawChild::Loss;
            terminal.body = Body::LossOnly;
            terminal.loss.events_new = 1;
            fixture.publish(&terminal, Namespace::Terminal, &[]);
        } else {
            publish_event(&fixture, &terminal, Namespace::Terminal);
        }
        assert!(fixture.frontier().0.complete());
    }
}

#[test]
fn dead_generation_without_final_endpoint_proof_is_rejected() {
    // Goal: death alone cannot turn a surviving prefix into a certified final extent.
    // Method: encode invalid endpoint/trailing combinations through the canonical boundary.
    for trailing in [false, true] {
        let fixture = Fixture::new();
        let mut terminal = event_metadata(&fixture, false);
        terminal.lifecycle = Lifecycle::DeadGeneration;
        terminal.trailing_unknown = trailing;
        terminal.evidence = if trailing {
            EndpointEvidence::FixedSource
        } else {
            EndpointEvidence::SurvivingRow
        };
        assert!(terminal.write(&mut Vec::new()).is_err());
    }
}

#[test]
fn a_fragment_cannot_extend_certified_terminal_authority() {
    // Goal: selected numeric progress cannot erase independent final lifecycle authority.
    // Method: retain a certified terminal and a locally valid later fragment, then read both.
    let fixture = Fixture::new();
    let mut terminal = event_metadata(&fixture, false);
    terminal.lifecycle = Lifecycle::AcquiredCompletion;
    publish_event(&fixture, &terminal, Namespace::Terminal);
    let mut fragment = event_metadata(&fixture, true);
    fragment.transaction = Identity([44, 45]);
    fragment.attempted_origin = 2;
    fragment.original_end = 0;
    fragment.loss.inherited.publication_start = 2;
    fragment.loss.inherited.publication_inherited_end = 2;
    fragment.loss.publications_new = 0;
    fragment.loss.inherited.event_start = 2;
    fragment.loss.inherited.event_inherited_end = 2;
    fragment.loss.event_end = 3;
    if let Body::Events(inner) = &mut fragment.body {
        inner.recovery.events_first = 2;
        inner.recovery.events_end = 3;
        inner.recovery.losses.publications = 0;
        inner.batches[0].first = 2;
        inner.batches[0].end = 3;
    }
    publish_event(&fixture, &fragment, Namespace::Fragments);
    assert_eq!(
        fixture
            .store()
            .source_frontier(&fixture.run, &budget(), &mut ReadWork::default())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn an_old_fragment_need_not_precede_the_latest_ordinary_directly() {
    // Goal: independent tails can have intervening ordinary history without being corrupt.
    // Method: retain ordinary7, its fragment, intervening ordinary9 and latest ordinary10.
    let fixture = Fixture::new();
    let (mut ordinary, _) = ordinary(&fixture);
    let mut fragment = event_metadata(&fixture, true);
    fragment.transaction = Identity([44, 45]);
    fragment.window = 8;
    fragment.loss.inherited.prior_window = Some(7);
    fragment.loss.inherited.predecessor = Predecessor::Canonical {
        transaction: Identity([18, 19]),
        kind: CanonicalKind::Ordinary,
        origin: 1,
    };
    fragment.attempted_origin = 2;
    fragment.original_end = 3;
    fragment.loss.inherited.publication_start = 2;
    fragment.loss.inherited.publication_inherited_end = 2;
    fragment.loss.publication_end = 3;
    let mut older = ordinary.clone();
    older.transaction = Identity([18, 19]);
    older.window = 7;
    older.attempted_origin = 1;
    older.canonical_origin = 1;
    older.original_end = 2;
    older.loss.inherited.publication_start = 1;
    older.loss.inherited.publication_inherited_end = 1;
    older.loss.inherited.publications = 0;
    older.loss.publications_new = 0;
    older.loss.publication_end = 2;
    older.loss.inherited.event_start = 1;
    older.loss.inherited.event_inherited_end = 1;
    older.loss.event_end = 1;
    older.loss.inherited.prior_window = None;
    older.loss.inherited.predecessor = Predecessor::Genesis;
    let (older, _) = ordinary_encoded(&fixture, older);
    fixture.publish(&older, Namespace::Ordinary, &[]);
    publish_event(&fixture, &fragment, Namespace::Fragments);
    let middle = publish_intervening_ordinary(&fixture, &older, &fragment);
    ordinary.loss.inherited.predecessor = Predecessor::Canonical {
        transaction: middle.transaction,
        kind: middle.kind,
        origin: 9,
    };
    fixture.publish(&ordinary, Namespace::Ordinary, &[]);
    let (frontier, work) = fixture.frontier();
    assert_eq!(work.metadata_admissions, 2);
    assert_eq!(frontier.publication_end(), 14);
    assert_eq!(frontier.event_end(), 100);
    assert_eq!(frontier.prior_window(), Some(10));
}

#[test]
fn certified_terminal_cannot_be_followed_by_ordinary_history() {
    // Goal: reversed namespace visitation cannot hide a terminal-before-ordinary conflict.
    // Method: visit the later ordinary tail first, then admit an earlier certified terminal.
    let fixture = Fixture::new();
    let (ordinary, _) = ordinary(&fixture);
    fixture.publish(&ordinary, Namespace::Ordinary, &[]);
    let mut terminal = event_metadata(&fixture, false);
    terminal.lifecycle = Lifecycle::AcquiredCompletion;
    publish_event(&fixture, &terminal, Namespace::Terminal);
    assert_eq!(
        fixture
            .store()
            .source_frontier(&fixture.run, &budget(), &mut ReadWork::default())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn terminal_after_a_fragment_preserves_equal_data_ends() {
    // Goal: reversed visitation also retains a legitimate final lifecycle-only transition.
    // Method: admit a terminal before its exact preceding fragment is visited.
    let fixture = Fixture::new();
    let fragment = event_metadata(&fixture, true);
    publish_event(&fixture, &fragment, Namespace::Fragments);
    let mut terminal = terminal_after(&fixture, &fragment);
    terminal.loss.inherited.predecessor = Predecessor::Canonical {
        transaction: fragment.transaction,
        kind: CanonicalKind::Fragment,
        origin: 1,
    };
    fixture.publish(&terminal, Namespace::Terminal, &[]);
    assert!(fixture.frontier().0.complete());
}

#[test]
fn equal_ends_do_not_authorize_an_unrelated_terminal() {
    // Goal: accepting lifecycle progress does not accept an unrelated ownership claim.
    // Method: keep matching endpoints but bind the terminal to a different transaction.
    let fixture = Fixture::new();
    let (ordinary, _) = ordinary(&fixture);
    fixture.publish(&ordinary, Namespace::Ordinary, &[]);
    let mut terminal = terminal_after(&fixture, &ordinary);
    terminal.loss.inherited.predecessor = Predecessor::Canonical {
        transaction: Identity([98, 99]),
        kind: CanonicalKind::Ordinary,
        origin: 13,
    };
    fixture.publish(&terminal, Namespace::Terminal, &[]);
    assert_eq!(
        fixture
            .store()
            .source_frontier(&fixture.run, &budget(), &mut ReadWork::default())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn canonical_gate_admission_enforces_the_shared_owned_payload_boundary() {
    // Goal: gate-derived source authority cannot bypass the physical owned-byte bound.
    // Method: publish valid captured terminal metadata over the same at-limit/one-over batches.
    for extra in [0, 1] {
        let fixture = Fixture::new();
        let mut metadata = event_metadata(&fixture, false);
        metadata.captured = true;
        metadata.loss.event_end = 16_385;
        if let Body::Events(inner) = &mut metadata.body {
            inner.identities = true;
            inner.recovery.events_end = 16_385;
            inner.recovery.rows = 16_384;
            inner.batches[0].end = 16_385;
            inner.batches[0].rows = 16_384;
        }
        let (raw, _) = fixture.publish(&metadata, Namespace::Terminal, &[]);
        let mut frame = crate::event_recovery::read::physical_captured_frame(extra);
        crate::encoder::encode_write(
            &mut frame,
            PrivateDirectory::open(&raw)
                .unwrap()
                .create_file("0000.parquet")
                .unwrap(),
        )
        .unwrap();
        let result =
            fixture
                .store()
                .source_frontier(&fixture.run, &budget(), &mut ReadWork::default());
        assert_eq!(result.is_ok(), extra == 0);
        if extra == 1 {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }
}

#[test]
fn canonical_gate_rejects_unknown_numeric_kind_in_both_profiles() {
    // Goal: canonical source ownership uses the same numeric semantics as original census.
    // Method: change only kind in complete otherwise-valid terminal raw objects.
    for captured in [false, true] {
        let fixture = Fixture::new();
        let mut metadata = event_metadata(&fixture, false);
        metadata.captured = captured;
        let count = if captured { 16_384 } else { 1 };
        metadata.loss.event_end = count + 1;
        if let Body::Events(inner) = &mut metadata.body {
            inner.identities = captured;
            inner.recovery.events_end = count + 1;
            inner.recovery.rows = count;
            inner.batches[0].end = count + 1;
            inner.batches[0].rows = count;
        }
        let (raw, _) = fixture.publish(&metadata, Namespace::Terminal, &[]);
        let mut frame = if captured {
            crate::event_recovery::read::physical_captured_frame(0)
        } else {
            event_frame(false)
        };
        frame
            .with_column(Column::new("kind".into(), vec![255_u8; frame.height()]))
            .unwrap();
        crate::encoder::encode_write(
            &mut frame,
            PrivateDirectory::open(&raw)
                .unwrap()
                .create_file("0000.parquet")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixture
                .store()
                .source_frontier(&fixture.run, &budget(), &mut ReadWork::default())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn canonical_rows_preserve_valid_start_exec_and_exit_semantics_in_both_profiles() {
    // Goal: stricter numeric validation retains producer-defined positive variants.
    // Method: admit actual gate metadata over rows copied from real producer event types.
    for captured in [false, true] {
        let fixture = Fixture::new();
        let mut metadata = event_metadata(&fixture, false);
        metadata.captured = captured;
        metadata.loss.event_end = 4;
        if let Body::Events(inner) = &mut metadata.body {
            inner.identities = captured;
            inner.recovery.events_end = 4;
            inner.recovery.rows = 3;
            inner.batches[0].end = 4;
            inner.batches[0].rows = 3;
        }
        let events = [
            collector::tracker::Event::Start {
                pid: 41,
                coalition: Some(9),
            },
            collector::tracker::Event::Exec { pid: 41 },
            collector::tracker::Event::Exit {
                pid: 41,
                last: Err(collector::processes::Unreadable::Refused),
                noticed: collector::tracker::Noticed::OnExit,
            },
        ];
        let rows: Vec<_> = events
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let event = crate::window::EventRecord::copy(&crate::NumberedEvent {
                    number: collector::u64_from_usize(index) + 1,
                    tick: 1,
                    event: source,
                });
                let identity = captured.then_some(crate::owned_identity::OwnedIdentity {
                    pid: 41,
                    creation: None,
                    availability: crate::schema::identity_availability::UNSUPPORTED,
                    usage: event.availability,
                    program: None,
                });
                crate::event_recovery::batch::Row {
                    publication: None,
                    event,
                    identity,
                    name: None,
                    app: None,
                }
            })
            .collect();
        let (raw, _) = fixture.publish(&metadata, Namespace::Terminal, &[]);
        let mut frame = crate::event_recovery::batch::frame(&rows, captured).unwrap();
        crate::encoder::encode_write(
            &mut frame,
            PrivateDirectory::open(&raw)
                .unwrap()
                .create_file("0000.parquet")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(fixture.frontier().0.event_end(), 4);
    }
}

fn publish_intervening_ordinary(
    fixture: &Fixture,
    older: &CanonicalMetadata,
    fragment: &CanonicalMetadata,
) -> CanonicalMetadata {
    let mut middle = older.clone();
    middle.transaction = Identity([20, 21]);
    middle.window = 9;
    middle.attempted_origin = 3;
    middle.canonical_origin = 9;
    middle.original_end = 10;
    middle.loss.inherited.publication_start = 3;
    middle.loss.inherited.publication_inherited_end = 3;
    middle.loss.publication_end = 10;
    middle.loss.publications_new = 6;
    middle.loss.inherited.event_start = 2;
    middle.loss.inherited.event_inherited_end = 2;
    middle.loss.event_end = 100;
    middle.loss.events_new = 98;
    middle.loss.inherited.prior_window = Some(7);
    middle.loss.inherited.predecessor = Predecessor::Canonical {
        transaction: fragment.transaction,
        kind: fragment.kind,
        origin: 1,
    };
    let (middle, _) = ordinary_encoded(fixture, middle);
    fixture.publish(&middle, Namespace::Ordinary, &[]);
    middle
}
