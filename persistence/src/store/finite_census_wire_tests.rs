use super::*;

#[test]
fn finite_review_census_numeric_literal() {
    // Goal: finite census bytes conform independently to the persistent header contract.
    // Method: compare the actual witness with a literal after a nonzero-floor predecessor.
    finite_review_census_literal(false);
}

#[test]
fn finite_review_census_captured_literal() {
    // Goal: the captured header uses the same finite grammar and preserves its profile.
    // Method: compare all 256 witness bytes and reject reserved and cross-operation changes.
    finite_review_census_literal(true);
}

fn finite_review_census_literal(captured: bool) {
    let mut fixture = proof_fixture(captured, 1);
    let store = Store::open(&fixture.root).unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    assert_eq!(
        finite_floor_loss(&mut fixture, &store, &budget).order_floor(),
        1
    );
    for tick in 121..=122 {
        if captured {
            publish_captured_pair_at(&mut fixture, tick, tick * 1_000_000_000);
        } else {
            publish_numeric_pair_at(&mut fixture, tick, tick * 1_000_000_000);
        }
    }
    publish_at(&mut fixture.writer, 3);
    let mut owner = store
        .begin_finite_events(fixture.reader(), &budget, &ledger)
        .unwrap();
    let state = finite_state(&fixture);
    let object = fixture
        .root
        .join("recovery-data")
        .join(collector::recording::identity_text(state.transaction).to_string());
    let path = object.join("original-census");
    for _ in 0..20000 {
        assert_eq!(owner.step().unwrap(), RowPublicationStep::Continue);
        if path.exists() && fs::metadata(&path).unwrap().len() == 394_720 {
            break;
        }
    }
    drop(owner);
    let bytes = fs::read(path).unwrap();
    assert_eq!(bytes.len(), 394_720);
    let literal = finite_review_census_bytes(&state.input, state.transaction, &object);
    assert_eq!(&bytes[..256], literal);
    finite_review_census_refusals(&literal, &state.input, state.transaction, &object);
}

fn finite_review_census_bytes(
    input: &AttemptInput,
    transaction: Identity,
    object: &Path,
) -> [u8; 256] {
    let mut literal = [0; 256];
    literal[..8].copy_from_slice(b"FDCENS02");
    for (offset, value) in [
        (8, 1u32),
        (12, 96),
        (16, 4109),
        (20, u32::from(input.captured)),
        (24, 3),
        (28, 1),
        (144, 1),
        (152, 1),
        (208, 512),
        (216, 1),
    ] {
        literal[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    for (offset, identity) in [
        (32, input.run.recording),
        (48, input.run.run),
        (64, input.run.boot),
        (80, transaction),
        (160, input.binding.identity),
    ] {
        literal[offset..offset + 8].copy_from_slice(&identity.0[0].to_be_bytes());
        literal[offset + 8..offset + 16].copy_from_slice(&identity.0[1].to_be_bytes());
    }
    for (offset, path) in [
        (96, object.to_owned()),
        (112, object.join("pending")),
        (128, object.join("pending/events")),
    ] {
        let (device, inode) = PrivateDirectory::open(&path)
            .unwrap()
            .device_inode()
            .unwrap();
        literal[offset..offset + 8].copy_from_slice(&device.to_be_bytes());
        literal[offset + 8..offset + 16].copy_from_slice(&inode.to_be_bytes());
    }
    for (offset, value) in [
        (176, input.binding.generation),
        (184, input.binding.publication_first),
        (192, input.binding.event_first),
        (200, input.source.layout),
        (224, input.source.origin_continuous_ns.unwrap()),
        (232, 1),
    ] {
        literal[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }
    literal
}

fn finite_review_census_refusals(
    literal: &[u8; 256],
    input: &AttemptInput,
    transaction: Identity,
    object: &Path,
) {
    let object = PrivateDirectory::open(object)
        .unwrap()
        .device_inode()
        .unwrap();
    let decode = |bytes: &[u8; 256], input: &AttemptInput| {
        inventory::census::CensusHeader::decode(bytes, input, transaction, object)
    };
    assert!(decode(literal, input).is_ok());
    for offset in 0..256 {
        if matches!(offset, 148..=151 | 156..=159 | 212..=215 | 220..=223 | 240..=255) {
            let mut changed = *literal;
            changed[offset] = 1;
            assert!(decode(&changed, input).is_err(), "reserved offset {offset}");
        }
    }
    let mut ordinary = input.clone();
    ordinary.operation = AttemptOperation::Ordinary {
        window: 1,
        attempted_origin: 3,
    };
    ordinary.validate().unwrap();
    assert!(decode(literal, &ordinary).is_err());
}
