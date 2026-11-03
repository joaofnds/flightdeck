use super::*;
use crate::{canonical::ReadWork, resources::ByteBudget};

#[test]
fn original_census_loss_only_process_restart() {
    // Goal: exact zero-survivor cleanup uses the same durable identity/absence authority.
    // Method: terminate a real owner at a partial witness or fixed plan, then reacquire Store.
    if let Some(root) = std::env::var_os("FLIGHTDECK_LOSS_CENSUS_ROOT") {
        loss_census_child(Path::new(&root));
        return;
    }
    for captured in [false, true] {
        for boundary in ["partial", "plan"] {
            for replacement in [false, true] {
                loss_census_case(captured, boundary, replacement);
            }
        }
    }
}
fn loss_census_case(captured: bool, boundary: &str, replacement: bool) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let attempt = store
        .register_attempt(&exact_input(&fixture), &mut fixture.reader(), &budget)
        .unwrap();
    review_publish_pair(&mut fixture, 2);
    review_zero_index(&attempt, &fixture);
    attempt
        .create_event_batch(4095)
        .unwrap()
        .write_all(b"PAR1")
        .unwrap();
    overwrite_registered_source(&mut fixture);
    let object = fixture
        .root
        .join("data")
        .join(collector::recording::identity_text(attempt.transaction()).to_string());
    drop(attempt);
    drop(store);
    let mut child = OwnedChild(
        loss_census_command(&fixture, boundary, "stop")
            .spawn()
            .unwrap(),
    );
    assert!(child.wait().success());
    let slot = fixture.slot();
    if replacement {
        let path = fixture.base.join("torn-replacement");
        private::open_private_file(&path, true)
            .unwrap()
            .write_all(b"PAR1")
            .unwrap();
        fs::rename(path, object.join("pending/events/4095.parquet")).unwrap();
    }
    let mut child = OwnedChild(
        loss_census_command(
            &fixture,
            boundary,
            if replacement { "refuse" } else { "finish" },
        )
        .spawn()
        .unwrap(),
    );
    assert!(child.wait().success());
    if replacement {
        assert_eq!(fixture.slot(), slot);
        assert_eq!(
            fs::read(object.join("pending/events/4095.parquet")).unwrap(),
            b"PAR1"
        );
    } else {
        assert!(!object.join("original-census").exists());
        assert!(!object.join("pending").exists());
    }
}
fn loss_census_command(fixture: &Fixture, boundary: &str, mode: &str) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::loss_only_tests::",
                "census_tests::original_census_loss_only_process_restart"
            ),
            "--nocapture",
        ])
        .env("FLIGHTDECK_LOSS_CENSUS_ROOT", &fixture.root)
        .env("FLIGHTDECK_REGISTRATION_RING", fixture.writer.name())
        .env("FLIGHTDECK_CENSUS_BOUNDARY", boundary)
        .env("FLIGHTDECK_CENSUS_MODE", mode);
    command
}
fn loss_census_child(root: &Path) {
    let store = Store::open(root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let reader = || {
        RingReader::open_generation(&std::env::var("FLIGHTDECK_REGISTRATION_RING").unwrap())
            .unwrap()
    };
    let mode = std::env::var("FLIGHTDECK_CENSUS_MODE").unwrap();
    let boundary = std::env::var("FLIGHTDECK_CENSUS_BOUNDARY").unwrap();
    let object = fs::read_dir(root.join("data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state = crate::transaction::normal::NormalState::decode(
        &fs::read(root.join("foreground")).unwrap(),
        store.recording,
    )
    .unwrap();
    let mut owner = if state.phase == bootstrap::codec::Phase::Registered {
        store
            .resume_loss_proof_with_ledger(reader(), &budget, &ledger)
            .unwrap()
    } else {
        store
            .resume_loss_publication_with_ledger(&budget, &ledger)
            .unwrap()
    };
    for _ in 0..20000 {
        match owner.step() {
            Err(error) => {
                assert_eq!(mode, "refuse");
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                return;
            }
            Ok(PublicationStep::Idle) => {
                assert_eq!(mode, "finish");
                loss_census_oracle(&store, &reader(), &object, &budget);
                return;
            }
            Ok(PublicationStep::Continue) => {}
        }
        if mode == "stop" {
            let state = crate::transaction::normal::NormalState::decode(
                &fs::read(root.join("foreground")).unwrap(),
                store.recording,
            )
            .unwrap();
            let stopped = if boundary == "partial" {
                state.census.kind == 1
                    && fs::metadata(object.join("original-census"))
                        .is_ok_and(|file| file.len() == 12544)
            } else {
                state.phase == bootstrap::codec::Phase::SalvagePlanned
            };
            if stopped {
                eprintln!(
                    "loss-census-process-stop {boundary} work={:?}",
                    ledger.work()
                );
                std::process::exit(0);
            }
        }
    }
    panic!("bounded loss census continuations");
}
fn loss_census_oracle(store: &Store, reader: &RingReader, object: &Path, budget: &Arc<ByteBudget>) {
    let metadata = crate::canonical::CanonicalMetadata::read(
        &mut private::open_private_file(&object.join("loss/metadata"), false).unwrap(),
    )
    .unwrap();
    assert_eq!(metadata.kind, crate::transaction::CanonicalKind::LossOnly);
    assert_eq!(metadata.loss.publications_new, 3);
    assert_eq!(metadata.loss.events_new, 2);
    assert_eq!(metadata.loss.inherited.publications, 0);
    assert_eq!(metadata.loss.inherited.events, 0);
    assert_eq!(metadata.loss.publication_end, 4);
    assert_eq!(metadata.loss.event_end, 3);
    assert_eq!(metadata.lifecycle, crate::canonical::Lifecycle::Active);
    let frontier = store
        .source_frontier(
            &reader.identity().unwrap(),
            budget,
            &mut ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 4);
    assert_eq!(frontier.event_end(), 3);
    assert!(!frontier.complete());
}
