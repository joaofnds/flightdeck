//! Ordinary recovery across loss of volatile metadata.
use super::*;

#[test]
fn ordinary_metadata_probe_missing_numeric() {
    // Goal: recover complete ordinary originals after losing only the volatile manifest.
    // Method: reopen Store before metadata and require publication to reach Idle.
    ordinary_metadata_probe(false, false);
}

#[test]
fn ordinary_metadata_probe_missing_captured() {
    // Goal: recover native ordinary originals after losing only the volatile manifest.
    // Method: reopen Store before metadata and require publication to reach Idle.
    ordinary_metadata_probe(true, false);
}

#[test]
fn ordinary_metadata_probe_completed_numeric() {
    // Goal: isolate the missing metadata boundary from ordinary source and row validity.
    // Method: complete the existing proof before the same Store reopen sequence.
    ordinary_metadata_probe(false, true);
}

#[test]
fn ordinary_metadata_probe_completed_captured() {
    // Goal: isolate the missing metadata boundary with native event bytes.
    // Method: complete the existing proof before the same Store reopen sequence.
    ordinary_metadata_probe(true, true);
}

fn ordinary_metadata_probe(captured: bool, completed_metadata: bool) {
    let mut fixture = proof_fixture(captured, 1);
    let base = fixture.base.clone();
    eprintln!("ORDINARY-METADATA-PROBE root={}", base.display());
    if captured {
        publish_captured_pair_at(&mut fixture, 2, 2);
    } else {
        publish_numeric_pair_at(&mut fixture, 2, 2);
    }
    fixture.writer.finish();
    let budget = registration_budget();
    let idle = fixture.slot();
    let (object, raw) = ordinary_metadata_probe_stage(&fixture, &budget, completed_metadata);
    let occupied = fixture.slot();
    let failure;
    let mut settled;
    {
        let store = Store::open(&fixture.root).unwrap();
        (settled, failure) =
            ordinary_metadata_probe_resume(&store, &budget, captured, completed_metadata);
        let directory = if settled { "ready" } else { "pending" };
        ordinary_metadata_probe_compare(&object, &raw, directory);
        if settled {
            assert_eq!(fixture.slot(), idle);
            let frontier = store
                .source_frontier(
                    &fixture.run,
                    &budget,
                    &mut crate::canonical::ReadWork::default(),
                )
                .unwrap()
                .unwrap();
            assert_eq!(frontier.publication_end(), 3);
            assert_eq!(frontier.event_end(), 3);
        } else {
            assert_eq!(fixture.slot(), occupied);
            assert!(
                store
                    .source_frontier(
                        &fixture.run,
                        &budget,
                        &mut crate::canonical::ReadWork::default()
                    )
                    .unwrap()
                    .is_none()
            );
            let ledger = PublicationLedger::new(&budget).unwrap();
            let mut recovery = store
                .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
                .unwrap();
            for _ in 0..10000 {
                if recovery.step().unwrap() == PublicationStep::Idle {
                    settled = true;
                    break;
                }
            }
            drop(recovery);
            assert!(settled);
            assert_eq!(fixture.slot(), idle);
            ordinary_metadata_probe_compare(&object, &raw, "ready");
        }
    }
    drop(fixture);
    assert!(!base.exists());
    assert!(
        settled,
        "complete ordinary originals must recover; profile={captured}, failure={failure:?}"
    );
}

fn ordinary_metadata_probe_compare(object: &Path, raw: &[(String, Vec<u8>)], directory: &str) {
    for (name, bytes) in raw {
        assert_eq!(&fs::read(object.join(directory).join(name)).unwrap(), bytes);
    }
}

fn ordinary_metadata_probe_stage(
    fixture: &Fixture,
    budget: &Arc<crate::resources::ByteBudget>,
    completed_metadata: bool,
) -> (PathBuf, Vec<(String, Vec<u8>)>) {
    let object;
    let mut raw = Vec::new();
    {
        let store = Store::open(&fixture.root).unwrap();
        let mut input = fixture.input(false);
        input.losses.unknown = None;
        input.operation = AttemptOperation::Ordinary {
            window: 0,
            attempted_origin: 2,
        };
        input.losses.publication_inherited_end = 2;
        input.losses.publications = 1;
        let attempt = store
            .register_attempt(&input, &mut fixture.reader(), budget)
            .unwrap();
        object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        let manifest = encode_payload(&attempt, fixture, 2, false);
        for table in crate::schema::TABLES {
            if !matches!(table, crate::schema::Table::Events) {
                let name = format!("{}.parquet", table.name());
                raw.push((
                    name.clone(),
                    fs::read(object.join("pending").join(name)).unwrap(),
                ));
            }
        }
        for name in ["events/index", "events/0000.parquet"] {
            raw.push((
                name.to_string(),
                fs::read(object.join("pending").join(name)).unwrap(),
            ));
        }
        assert!(!object.join("pending/metadata").exists());
        if completed_metadata {
            drop(issue(attempt, &manifest, fixture.reader(), budget).unwrap());
            assert!(object.join("pending/metadata").exists());
        } else {
            drop(attempt);
        }
    }
    (object, raw)
}
fn ordinary_metadata_probe_resume(
    store: &Store,
    budget: &Arc<crate::resources::ByteBudget>,
    captured: bool,
    completed_metadata: bool,
) -> (bool, Option<io::Error>) {
    let mut settled = false;
    let mut failure = None;
    let mut publication = store.resume_publication(budget).unwrap();
    for _ in 0..10000 {
        match publication.step() {
            Ok(PublicationStep::Idle) => {
                settled = true;
                break;
            }
            Ok(PublicationStep::Continue) => {}
            Err(error) => {
                eprintln!(
                    "ORDINARY-METADATA-PROBE profile={captured} \
            completed_metadata={completed_metadata} normal_error={error:?}"
                );
                failure = Some(error);
                break;
            }
        }
    }
    drop(publication);
    (settled, failure)
}

#[path = "ordinary_adapter_tests.rs"]
mod ordinary_adapter_tests;

#[path = "ordinary_complete_tests.rs"]
mod ordinary_complete_tests;
