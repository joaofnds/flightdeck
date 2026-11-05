use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::{AtomicU64, Ordering};

use super::Store;

static NEXT: AtomicU64 = AtomicU64::new(1);

#[test]
fn a_legacy_root_is_readable_but_a_new_writer_refuses_without_changing_it() {
    // Goal: format transition never converts an active or dormant legacy recording in place.
    // Method: keep only legacy identity and an announcement, then compare the root on refusal.
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("fd-legacy-refusal-{}-{serial}", std::process::id()));
    collector::recording::private::create_private_dir(&root).unwrap();
    let identity = b"00000000000000010000000000000002\n";
    collector::recording::private::atomic_write(&root, "identity", identity).unwrap();
    collector::recording::private::atomic_write(&root, "collector.announcement", b"sentinel\n")
        .unwrap();
    assert_eq!(
        collector::recording::read_identity(&root).unwrap(),
        collector::ring::Identity([1, 2])
    );
    let refused = Store::open(&root)
        .err()
        .expect("new writer accepted legacy root");
    assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(fs::read(root.join("identity")).unwrap(), identity);
    assert_eq!(
        fs::read(root.join("collector.announcement")).unwrap(),
        b"sentinel\n"
    );
    assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_previous_writers_identity_refuses_a_store_without_changing_the_root() {
    // Goal: a recording of the format before this one stays as its writer left it.
    // Method: lay out a root of the previous identity format, open a store, and compare the root.
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "fd-previous-writer-{}-{serial}",
        std::process::id()
    ));
    collector::recording::private::create_private_dir(&root).unwrap();
    let identity =
        b"flightdeck-recording-1\n00000001\n00000001\n00000000000000010000000000000002\n";
    collector::recording::private::atomic_write(&root, "identity", identity).unwrap();
    collector::recording::private::create_private_dir(&root.join("commits")).unwrap();

    let refused = Store::open(&root).err().unwrap();

    assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(fs::read(root.join("identity")).unwrap(), identity);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_reuses_private_children_and_refuses_permission_repair() {
    // Goal: Store startup consumes create-or-validate and preserves foreign permissions.
    // Method: reopen a fresh Store, then loosen data and inspect it after refusal.
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "fd-startup-private-{}-{serial}",
        std::process::id()
    ));
    drop(Store::open(&root).unwrap());
    let identity = fs::metadata(root.join("data")).unwrap();

    drop(Store::open(&root).unwrap());

    assert_eq!(
        identity.ino(),
        fs::metadata(root.join("data")).unwrap().ino()
    );
    fs::set_permissions(root.join("data"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        std::io::ErrorKind::PermissionDenied,
        Store::open(&root).err().unwrap().kind()
    );
    assert_eq!(
        0o755,
        fs::metadata(root.join("data")).unwrap().mode() & 0o7777
    );
    fs::remove_dir_all(root).unwrap();
}
