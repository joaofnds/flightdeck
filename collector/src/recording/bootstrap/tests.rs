use super::codec::*;
use crate::ring::Identity;

#[test]
fn bootstrap_matches_the_registered_wire_fixture() {
    // Goal: encoder and decoder cannot agree on an accidental format change.
    // Method: compare both directions with the independent registered 64-byte encoding.
    let expected = [
        0x46, 0x44, 0x42, 0x4f, 0x4f, 0x54, 0x30, 0x32, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0,
        0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let identity = Identity([1, 2]);
    let mut bytes = [0; 64];
    encode_bootstrap(identity, &mut bytes).unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(decode_bootstrap(&expected).unwrap(), identity);
}

#[test]
fn bootstrap_rejects_reserved_bytes_and_wrong_lengths() {
    // Goal: bootstrap authority has one exact encoding. Method: corrupt its padding and size.
    let identity = Identity([1, 2]);
    let mut bytes = [0; 64];
    encode_bootstrap(identity, &mut bytes).unwrap();
    assert_eq!(decode_bootstrap(&bytes).unwrap(), identity);
    bytes[63] = 1;
    assert!(decode_bootstrap(&bytes).is_err());
    assert!(decode_bootstrap(&bytes[..63]).is_err());
}

use super::{Classification, StateObservation, classify, initialize};
use crate::recording::private::{self, FileLock, PrivateDirectory};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "fd-bootstrap-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        private::create_private_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn a_recording_set_up_in_part_finishes_with_the_identity_it_began() {
    // Goal: a process that ended while setting up a recording leaves a root the next one
    // finishes, under the same identity.
    // Method: seed the first step of a setup, then ask for the recording's identity.
    let fixture = Root::new();
    let root = PrivateDirectory::open(&fixture.0).unwrap();
    let registration = FileLock::try_lock(&fixture.0.join("registration.lock")).unwrap();
    let identity = Identity([1, 2]);
    let mut bootstrap = [0; 64];
    encode_bootstrap(identity, &mut bootstrap).unwrap();
    private::atomic_write_in(&root, "bootstrap", &bootstrap).unwrap();

    let finished = crate::recording::recording_identity(&fixture.0, &registration).unwrap();

    assert_eq!(finished, identity);
    assert_eq!(
        classify(&root, &mut [0; 4097]).unwrap(),
        Classification::Initialized(identity)
    );
}

#[test]
fn initializer_adopts_partial_scaffold_and_reader_detects_replaced_quiet_inode() {
    // Goal: restart preserves registration identity and independent reopen catches ABA.
    // Method: seed an interrupted scaffold, initialize, then replace identical state bytes.
    let fixture = Root::new();
    let root = PrivateDirectory::open(&fixture.0).unwrap();
    let registration = FileLock::try_lock(&fixture.0.join("registration.lock")).unwrap();
    let identity = Identity([1, 2]);
    let mut bootstrap = [0; 64];
    encode_bootstrap(identity, &mut bootstrap).unwrap();
    private::atomic_write_in(&root, "bootstrap", &bootstrap).unwrap();
    root.create_directory("ordinary").unwrap();
    private::atomic_write_in(&root, "foreground.pending", b"FDT").unwrap();
    let mut scratch = [0; 4097];
    assert_eq!(
        classify(&root, &mut scratch).unwrap(),
        Classification::Initializing(identity)
    );
    assert_eq!(
        initialize(&root, &registration, &mut scratch).unwrap(),
        identity
    );
    assert_eq!(
        classify(&root, &mut scratch).unwrap(),
        Classification::Initialized(identity)
    );
    assert_eq!(
        initialize(&root, &registration, &mut scratch).unwrap(),
        identity
    );

    let mut state = [0; 256];
    let observation = StateObservation::begin(&root, identity, &mut state)
        .unwrap()
        .unwrap();
    let mut second = [0; 256];
    assert!(observation.unchanged(&mut second).unwrap());
    encode_state(
        identity,
        &NamespaceState::Quiet { sequence: 0 },
        &mut second,
    )
    .unwrap();
    private::atomic_write_in(&root, "namespace-state", &second).unwrap();
    assert!(!observation.unchanged(&mut second).unwrap());
}

#[test]
fn unsupported_identity_precedes_bootstrap_and_preserves_root() {
    // Goal: unsupported roots refuse without mutation. Method: compare contents after initialize.
    let fixture = Root::new();
    let root = PrivateDirectory::open(&fixture.0).unwrap();
    let registration = FileLock::try_lock(&fixture.0.join("registration.lock")).unwrap();
    let bytes = b"flightdeck-recording-1\n00000001\n00000001\n00000000000000010000000000000002\n";
    private::atomic_write_in(&root, "identity", bytes).unwrap();
    private::atomic_write_in(&root, "bootstrap", b"invalid").unwrap();
    let mut scratch = [0; 4097];
    assert_eq!(
        classify(&root, &mut scratch).unwrap(),
        Classification::Unsupported
    );
    assert_eq!(
        initialize(&root, &registration, &mut scratch)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(fixture.0.join("identity")).unwrap(), bytes);
    assert_eq!(
        std::fs::read(fixture.0.join("bootstrap")).unwrap(),
        b"invalid"
    );
}

#[test]
fn fresh_root_refuses_unknown_names_and_contradictory_scratch() {
    // Goal: only exact fresh inventory is repairable. Method: inject unknown lock and bad prefix.
    let fixture = Root::new();
    let root = PrivateDirectory::open(&fixture.0).unwrap();
    let mut scratch = [0; 4097];
    assert_eq!(
        classify(&root, &mut scratch).unwrap(),
        Classification::Fresh
    );
    private::atomic_write_in(&root, "persister.lock", b"").unwrap();
    assert_eq!(
        classify(&root, &mut scratch).unwrap(),
        Classification::Corrupt
    );
    root.remove_file("persister.lock").unwrap();
    private::atomic_write_in(&root, "bootstrap.pending", b"FD").unwrap();
    assert_eq!(
        classify(&root, &mut scratch).unwrap(),
        Classification::Fresh
    );
    private::atomic_write_in(&root, "bootstrap.pending", b"FX").unwrap();
    assert_eq!(
        classify(&root, &mut scratch).unwrap(),
        Classification::Corrupt
    );
}

#[test]
fn initialized_root_refuses_a_signed_reader_protocol_without_mutation() {
    // Goal: malformed reader requirements cannot grant authority to an initialized root.
    // Method: replace its fixed-width reader field and compare every root artifact on refusal.
    verify_signed_protocol_refusal(23);
}

#[test]
fn initialized_root_refuses_a_signed_writer_protocol_without_mutation() {
    // Goal: malformed writer requirements cannot grant authority to an initialized root.
    // Method: replace its fixed-width writer field and compare every root artifact on refusal.
    verify_signed_protocol_refusal(32);
}

fn verify_signed_protocol_refusal(offset: usize) {
    use std::os::unix::fs::MetadataExt;

    let fixture = Root::new();
    let root = PrivateDirectory::open(&fixture.0).unwrap();
    let lock = FileLock::try_lock_in(&root, "registration.lock").unwrap();
    let mut scratch = [0; 4097];
    initialize(&root, &lock, &mut scratch).unwrap();
    let mut identity = std::fs::read(fixture.0.join("identity")).unwrap();
    identity[offset] = b'+';
    private::atomic_write_in(&root, "identity", &identity).unwrap();
    let snapshot = || {
        let mut entries = std::fs::read_dir(&fixture.0)
            .unwrap()
            .take(21)
            .map(|entry| {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                let bytes = if metadata.is_file() {
                    std::fs::read(entry.path()).unwrap()
                } else {
                    Vec::new()
                };
                (
                    entry.file_name(),
                    metadata.dev(),
                    metadata.ino(),
                    metadata.mode(),
                    metadata.uid(),
                    bytes,
                )
            })
            .collect::<Vec<_>>();
        assert!(entries.len() <= 20);
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    };
    let before = snapshot();

    assert_eq!(
        classify(&root, &mut scratch).unwrap(),
        Classification::Corrupt
    );
    assert_eq!(
        initialize(&root, &lock, &mut scratch).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(before, snapshot());
}

#[test]
fn slot_envelope_refuses_unknown_enums_and_maintenance_authority() {
    // Goal: opaque occupied bytes never become Idle or executable maintenance.
    // Method: alter the exact idle envelope into bounded occupied and invalid forms.
    let identity = Identity([1, 2]);
    let mut bytes = [0; 4096];
    encode_idle(identity, SlotKind::Foreground, &mut bytes).unwrap();
    assert!(matches!(
        decode_slot(&bytes, identity, SlotKind::Foreground).unwrap(),
        Slot::Idle
    ));
    bytes[47] = 1;
    bytes[51] = 1;
    bytes[55] = 1;
    bytes[56..60].copy_from_slice(&2048_u32.to_be_bytes());
    bytes[64] = 7;
    assert!(matches!(
        decode_slot(&bytes, identity, SlotKind::Foreground).unwrap(),
        Slot::Occupied { payload, .. } if payload.len() == 2048 && payload[0] == 7
    ));
    bytes[51] = 9;
    assert!(decode_slot(&bytes, identity, SlotKind::Foreground).is_err());
    bytes[51] = 1;
    bytes[55] = 4;
    assert!(decode_slot(&bytes, identity, SlotKind::Foreground).is_err());
    bytes[55] = 1;
    bytes[15] = 2;
    assert_eq!(
        decode_slot(&bytes, identity, SlotKind::Maintenance)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
}

#[test]
fn slot_envelope_requires_the_registered_payload_length() {
    // Goal: a short occupied envelope cannot masquerade as a registered operation.
    // Method: decode the fixed common header with invalid and exact payload lengths.
    let identity = Identity([1, 2]);
    let mut bytes = [0; 4096];
    encode_idle(identity, SlotKind::Foreground, &mut bytes).unwrap();
    bytes[47] = 1;
    bytes[51] = 1;
    bytes[55] = 1;

    for length in [0_u32, 1, 2047, 2049, 4032] {
        bytes[56..60].copy_from_slice(&length.to_be_bytes());
        assert!(
            decode_slot(&bytes, identity, SlotKind::Foreground).is_err(),
            "accepted occupied payload length {length}"
        );
    }

    bytes[56..60].copy_from_slice(&2048_u32.to_be_bytes());
    assert!(matches!(
        decode_slot(&bytes, identity, SlotKind::Foreground).unwrap(),
        Slot::Occupied { payload, .. } if payload.len() == 2048
    ));
}

#[test]
fn slot_envelope_keeps_unresolved_extent_distinct_from_commitment() {
    // Goal: unknown endpoints must never decode as completion authority.
    // Method: decode the pinned wire phases with the same occupied envelope.
    let identity = Identity([1, 2]);
    let mut bytes = [0; 4096];
    encode_idle(identity, SlotKind::Foreground, &mut bytes).unwrap();
    bytes[47] = 1;
    bytes[55] = 1;
    bytes[56..60].copy_from_slice(&2048_u32.to_be_bytes());

    for (wire, expected) in [
        (1_u32, Phase::Registered),
        (2, Phase::Ready),
        (3, Phase::Publishing),
        (4, Phase::Published),
        (5, Phase::Cleanup),
        (6, Phase::SalvagePlanned),
        (7, Phase::SalvageCommitted),
        (8, Phase::UnresolvedExtent),
    ] {
        bytes[48..52].copy_from_slice(&wire.to_be_bytes());
        let Slot::Occupied { phase, .. } =
            decode_slot(&bytes, identity, SlotKind::Foreground).unwrap()
        else {
            panic!("occupied wire phase decoded as idle");
        };
        assert_eq!(phase, expected);
    }
}

#[test]
fn retained_root_survives_substitution_at_longest_admitted_path() {
    // Goal: initializer never reconstructs descendant absolute paths.
    // Method: initialize at 1023 bytes, rename its ancestor and substitute a fresh root name.
    let fixture = Root::new();
    let mut path = fixture.0.canonicalize().unwrap();
    for _ in 0..8 {
        let remaining = 1023 - path.as_os_str().len();
        if remaining == 0 {
            break;
        }
        path.push("x".repeat((remaining - 1).min(200)));
        private::create_private_dir(&path).unwrap();
    }
    assert_eq!(path.as_os_str().len(), 1023);
    let root = PrivateDirectory::open(&path).unwrap();
    let lock = FileLock::try_lock_in(&root, "registration.lock").unwrap();
    let mut scratch = [0; 4097];
    let identity = initialize(&root, &lock, &mut scratch).unwrap();
    let old = path.with_file_name("old");
    std::fs::rename(&path, &old).unwrap();
    private::create_private_dir(&path).unwrap();
    assert_eq!(initialize(&root, &lock, &mut scratch).unwrap(), identity);
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
    let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
        assert_eq!(
            classify(&root, &mut scratch).unwrap(),
            Classification::Initialized(identity)
        );
    });
    assert_eq!(calls, 0);
}

#[test]
fn state_codec_rejects_identity_padding_and_cross_field_changes() {
    // Goal: Quiet cannot hide transaction authority. Method: mutate each fixed boundary.
    let identity = Identity([1, 2]);
    let mut bytes = [0; 256];
    encode_state(identity, &NamespaceState::Quiet { sequence: 0 }, &mut bytes).unwrap();
    assert_eq!(
        decode_state(&bytes, identity).unwrap(),
        NamespaceState::Quiet { sequence: 0 }
    );
    bytes[55] = 1;
    assert!(decode_state(&bytes, identity).is_err());
    bytes[55] = 0;
    bytes[255] = 1;
    assert!(decode_state(&bytes, identity).is_err());
    bytes[255] = 0;
    assert!(decode_state(&bytes, Identity([2, 1])).is_err());
    assert!(decode_state(&bytes[..255], identity).is_err());
}

#[test]
fn visible_identity_with_bootstrap_stays_pending_until_initializer_finishes() {
    // Goal: identity publication cannot authorize readers ahead of bootstrap removal.
    // Method: recreate the interrupted final boundary and observe Pending then adoption.
    let fixture = Root::new();
    let root = PrivateDirectory::open(&fixture.0).unwrap();
    let lock = FileLock::try_lock_in(&root, "registration.lock").unwrap();
    let mut scratch = [0; 4097];
    let identity = initialize(&root, &lock, &mut scratch).unwrap();
    let mut bootstrap = [0; 64];
    encode_bootstrap(identity, &mut bootstrap).unwrap();
    private::atomic_write_in(&root, "bootstrap", &bootstrap).unwrap();
    let mut state = [0; 256];
    assert!(
        StateObservation::begin(&root, identity, &mut state)
            .unwrap()
            .is_none()
    );
    assert_eq!(initialize(&root, &lock, &mut scratch).unwrap(), identity);
    assert!(
        StateObservation::begin(&root, identity, &mut state)
            .unwrap()
            .is_some()
    );
}

#[test]
fn initializer_refuses_symlinks_and_public_modes_without_repair() {
    // Goal: malformed scaffold admission precedes repair. Method: substitute a child and lock.
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = Root::new();
    let root = PrivateDirectory::open(&fixture.0).unwrap();
    let lock = FileLock::try_lock_in(&root, "registration.lock").unwrap();
    let identity = Identity([1, 2]);
    let mut bootstrap = [0; 64];
    encode_bootstrap(identity, &mut bootstrap).unwrap();
    private::atomic_write_in(&root, "bootstrap", &bootstrap).unwrap();
    symlink(&fixture.0, fixture.0.join("data")).unwrap();
    let mut scratch = [0; 4097];
    assert!(initialize(&root, &lock, &mut scratch).is_err());
    assert_eq!(
        std::fs::read_link(fixture.0.join("data")).unwrap(),
        fixture.0
    );
    std::fs::remove_file(fixture.0.join("data")).unwrap();
    std::fs::set_permissions(
        fixture.0.join("registration.lock"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(initialize(&root, &lock, &mut scratch).is_err());
    assert_eq!(
        std::fs::metadata(fixture.0.join("registration.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
}
