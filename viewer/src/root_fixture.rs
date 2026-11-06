//! Recording roots no reader can read yet or any more.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::recording::{bootstrap, private};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A root as the previous format left it, with a directory of its committed minutes.
pub(crate) fn older_root() -> PathBuf {
    let root = fixture_root("older");
    let identity =
        b"flightdeck-recording-1\n00000001\n00000001\n00000000000000010000000000000002\n";
    private::atomic_write(&root, "identity", identity).unwrap();
    private::create_private_dir(&root.join("commits")).unwrap();
    root
}

/// A root whose writer stopped between writing its identity and finishing its setup, the last
/// moment of a first launch.
pub(crate) fn unfinished_root() -> PathBuf {
    let root = fixture_root("unfinished");
    let directory = private::PrivateDirectory::open(&root).unwrap();
    let lock = private::FileLock::try_lock_in(&directory, "registration.lock").unwrap();
    let recording = bootstrap::initialize(&directory, &lock, &mut [0; 4097]).unwrap();
    let mut bytes = [0; 64];
    bootstrap::codec::encode_bootstrap(recording, &mut bytes).unwrap();
    private::atomic_write_in(&directory, "bootstrap", &bytes).unwrap();
    root
}

fn fixture_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "flightdeck-root-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    private::create_private_dir(&root).unwrap();
    root
}
