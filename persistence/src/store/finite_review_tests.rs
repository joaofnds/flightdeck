use super::*;

#[test]
fn finite_review_inherited_helper_refuses_numeric_writer() {
    // Goal: caller-selected finite bounds cannot grant original writer authority.
    // Method: offer an event end beyond the actual empty prefix through the public helper.
    finite_review_inherited_helper_refuses_writer(false);
}

#[test]
fn finite_review_inherited_helper_refuses_captured_writer() {
    // Goal: captured finite input has the same consuming admission boundary.
    // Method: offer an event end beyond the actual empty prefix through the public helper.
    finite_review_inherited_helper_refuses_writer(true);
}

fn finite_review_inherited_helper_refuses_writer(captured: bool) {
    let fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let tick = encode_payload_tick(&mut fixture.reader(), 1);
    assert_eq!(tick.events.start, 1);
    assert_eq!(tick.events.end, 1);
    let slot = fixture.slot();
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.operation = AttemptOperation::Finite {
        event_end: 2,
        source_first: 1,
        order_floor: 0,
    };
    input.validate().unwrap();

    let result = (|| -> io::Result<()> {
        let mut observation =
            store.prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)?;
        assert!(observation.step()?);
        let attempt = observation.register()?;
        let batch = attempt.create_event_batch(0)?;
        drop(batch);
        Ok(())
    })();

    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(fixture.slot(), slot);
    assert_eq!(
        fs::read_dir(fixture.root.join("recovery-data"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(fs::read_dir(fixture.root.join("data")).unwrap().count(), 0);
}

#[test]
fn finite_review_inherited_helper_keeps_empty_ordinary_transfer() {
    // Goal: the finite refusal preserves ordinary registration with no inherited events.
    // Method: consume a real empty observation and obtain the supported ordinary writer.
    let fixture = proof_fixture(false, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    let mut observation = store
        .prepare_inherited_attempt(&input, fixture.reader(), &budget, &ledger)
        .unwrap();
    assert!(observation.step().unwrap());
    let attempt = observation.register().unwrap();
    assert_eq!(attempt.input(), &input);
    assert!(attempt.create_event_batch(0).is_ok());
}

#[derive(Clone, Copy)]
enum FiniteAdmission {
    Direct,
    Prepared,
    Capture,
}
#[derive(Clone, Copy)]
enum SourceContinuity {
    Matching,
    Profile,
    Timing,
}

#[test]
fn finite_review_continuity_numeric_direct_matching() {
    // Goal: finite direct admission preserves canonical source continuity.
    // Method: use a real predecessor and a matching successor object before construction.
    finite_review_continuity(false, FiniteAdmission::Direct, SourceContinuity::Matching);
}

#[test]
fn finite_review_continuity_numeric_direct_profile() {
    // Goal: finite direct admission preserves canonical source continuity.
    // Method: use a real predecessor and a profile successor object before construction.
    finite_review_continuity(false, FiniteAdmission::Direct, SourceContinuity::Profile);
}

#[test]
fn finite_review_continuity_numeric_direct_timing() {
    // Goal: finite direct admission preserves canonical source continuity.
    // Method: use a real predecessor and a timing successor object before construction.
    finite_review_continuity(false, FiniteAdmission::Direct, SourceContinuity::Timing);
}

#[test]
fn finite_review_continuity_numeric_prepared_matching() {
    // Goal: finite prepared admission preserves canonical source continuity.
    // Method: use a real predecessor and a matching successor object before construction.
    finite_review_continuity(false, FiniteAdmission::Prepared, SourceContinuity::Matching);
}

#[test]
fn finite_review_continuity_numeric_prepared_profile() {
    // Goal: finite prepared admission preserves canonical source continuity.
    // Method: use a real predecessor and a profile successor object before construction.
    finite_review_continuity(false, FiniteAdmission::Prepared, SourceContinuity::Profile);
}

#[test]
fn finite_review_continuity_numeric_prepared_timing() {
    // Goal: finite prepared admission preserves canonical source continuity.
    // Method: use a real predecessor and a timing successor object before construction.
    finite_review_continuity(false, FiniteAdmission::Prepared, SourceContinuity::Timing);
}

#[test]
fn finite_review_continuity_numeric_capture_matching() {
    // Goal: finite capture admission preserves canonical source continuity.
    // Method: use a real predecessor and a matching successor object before construction.
    finite_review_continuity(false, FiniteAdmission::Capture, SourceContinuity::Matching);
}

#[test]
fn finite_review_continuity_numeric_capture_profile() {
    // Goal: finite capture admission preserves canonical source continuity.
    // Method: use a real predecessor and a profile successor object before construction.
    finite_review_continuity(false, FiniteAdmission::Capture, SourceContinuity::Profile);
}

#[test]
fn finite_review_continuity_numeric_capture_timing() {
    // Goal: finite capture admission preserves canonical source continuity.
    // Method: use a real predecessor and a timing successor object before construction.
    finite_review_continuity(false, FiniteAdmission::Capture, SourceContinuity::Timing);
}

#[test]
fn finite_review_continuity_captured_direct_matching() {
    // Goal: finite direct admission preserves canonical source continuity.
    // Method: use a real predecessor and a matching successor object before construction.
    finite_review_continuity(true, FiniteAdmission::Direct, SourceContinuity::Matching);
}

#[test]
fn finite_review_continuity_captured_direct_profile() {
    // Goal: finite direct admission preserves canonical source continuity.
    // Method: use a real predecessor and a profile successor object before construction.
    finite_review_continuity(true, FiniteAdmission::Direct, SourceContinuity::Profile);
}

#[test]
fn finite_review_continuity_captured_direct_timing() {
    // Goal: finite direct admission preserves canonical source continuity.
    // Method: use a real predecessor and a timing successor object before construction.
    finite_review_continuity(true, FiniteAdmission::Direct, SourceContinuity::Timing);
}

#[test]
fn finite_review_continuity_captured_prepared_matching() {
    // Goal: finite prepared admission preserves canonical source continuity.
    // Method: use a real predecessor and a matching successor object before construction.
    finite_review_continuity(true, FiniteAdmission::Prepared, SourceContinuity::Matching);
}

#[test]
fn finite_review_continuity_captured_prepared_profile() {
    // Goal: finite prepared admission preserves canonical source continuity.
    // Method: use a real predecessor and a profile successor object before construction.
    finite_review_continuity(true, FiniteAdmission::Prepared, SourceContinuity::Profile);
}

#[test]
fn finite_review_continuity_captured_prepared_timing() {
    // Goal: finite prepared admission preserves canonical source continuity.
    // Method: use a real predecessor and a timing successor object before construction.
    finite_review_continuity(true, FiniteAdmission::Prepared, SourceContinuity::Timing);
}

#[test]
fn finite_review_continuity_captured_capture_matching() {
    // Goal: finite capture admission preserves canonical source continuity.
    // Method: use a real predecessor and a matching successor object before construction.
    finite_review_continuity(true, FiniteAdmission::Capture, SourceContinuity::Matching);
}

#[test]
fn finite_review_continuity_captured_capture_profile() {
    // Goal: finite capture admission preserves canonical source continuity.
    // Method: use a real predecessor and a profile successor object before construction.
    finite_review_continuity(true, FiniteAdmission::Capture, SourceContinuity::Profile);
}

#[test]
fn finite_review_continuity_captured_capture_timing() {
    // Goal: finite capture admission preserves canonical source continuity.
    // Method: use a real predecessor and a timing successor object before construction.
    finite_review_continuity(true, FiniteAdmission::Capture, SourceContinuity::Timing);
}

fn finite_review_continuity(
    captured: bool,
    admission: FiniteAdmission,
    continuity: SourceContinuity,
) {
    let fixture = finite_review_predecessor(captured, continuity);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let frontier = finite_frontier(&store, &fixture, &budget);
    assert_eq!(frontier.publication_end(), 2);
    assert_eq!(frontier.event_end(), 1);
    let tick = encode_payload_tick(&mut fixture.reader(), 2);
    assert_eq!(tick.events.start, 3);
    let slot = fixture.slot();

    let result = finite_review_admit(&store, &fixture, &budget, &ledger, admission);

    match continuity {
        SourceContinuity::Matching => result.unwrap(),
        SourceContinuity::Profile | SourceContinuity::Timing => {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(fixture.slot(), slot);
            assert_eq!(
                fs::read_dir(fixture.root.join("recovery-data"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}
fn finite_review_admit(
    store: &Store,
    fixture: &Fixture,
    budget: &Arc<crate::resources::ByteBudget>,
    ledger: &PublicationLedger,
    admission: FiniteAdmission,
) -> io::Result<()> {
    match admission {
        FiniteAdmission::Direct => {
            drop(store.begin_finite_events(fixture.reader(), budget, ledger)?);
        }
        FiniteAdmission::Capture => {
            drop(store.capture_finite_events(fixture.reader(), budget, ledger)?);
        }
        FiniteAdmission::Prepared => {
            let root = PrivateDirectory::open(&fixture.root)?;
            let mut scratch = [0; 256];
            let mut query = crate::canonical::SourceFrontierRead::begin(
                &root,
                fixture.run,
                &mut scratch,
                budget,
            )?;
            for _ in 0..1000 {
                if query.step()? == crate::canonical::FrontierStep::Complete {
                    break;
                }
            }
            assert!(query.result()?.is_some());
            drop(store.begin_finite_events_from_frontier(
                fixture.reader(),
                query,
                budget,
                ledger,
            )?);
        }
    }
    Ok(())
}
fn finite_review_predecessor(captured: bool, continuity: SourceContinuity) -> Fixture {
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
    drop(store);
    finite_review_successor(&mut fixture, captured, continuity);
    fixture
}
fn finite_review_successor(fixture: &mut Fixture, captured: bool, continuity: SourceContinuity) {
    let captured = if matches!(continuity, SourceContinuity::Profile) {
        !captured
    } else {
        captured
    };
    let origin = if matches!(continuity, SourceContinuity::Timing) {
        2
    } else {
        1
    };
    let place = collector::ring::Place {
        lock_path: fixture.base.join("continuity.lock"),
        name_prefix: format!(
            "/fdc{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let mut shape = fixture.reader().shape();
    shape.slot_count = 1;
    let writer = if captured {
        collector::ring::Writer::create_with_identity(
            &place,
            shape,
            collector::ring::DictionaryCapacity {
                entries: 4,
                arena_bytes: 16 * 1024,
            },
            &fixture.run,
            origin,
        )
        .unwrap()
    } else {
        collector::ring::Writer::create_recording(&place, shape, &fixture.run, origin).unwrap()
    };
    let old = fixture.writer.name().to_owned();
    fixture.writer = writer;
    collector::ring::unlink_generation(&old).unwrap();
    fixture.place = place;
    fixture.source = crate::transaction::SourceTiming::from_reader(&fixture.reader());
    fixture.binding = fixture.reader().object_binding().unwrap();
    if captured {
        publish_captured_pair_at(fixture, 1, origin);
    } else {
        publish_numeric_pair_at(fixture, 1, origin);
    }
    publish_at(&mut fixture.writer, 1);
}
