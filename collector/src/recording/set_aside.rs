//! Moves a recording this version's writers refuse out of the way of a new one.
use std::io;
use std::path::{Path, PathBuf};

use super::{format, identity_text, private};
use crate::ring::Identity;

/// Every process that may go on using a recording's path holds one of these for its lifetime.
const LIFETIME_LOCKS: [&str; 3] = ["recorder.lock", "collector.lock", "persister.lock"];

/// Renames a recording an older version wrote, which no writer of this version may append to,
/// so that the next recorder starts a new one at `root`. Returns where the older recording went.
/// The rename keeps the saved files, and the name carries the recording's identity, so only a
/// copy of this same recording can be in the way, which the rename refuses. A recording a process
/// still uses stays, since that process goes on using its path.
pub fn set_aside_older(root: &Path) -> io::Result<Option<PathBuf>> {
    let Some(identity) = older_identity(root)? else {
        return Ok(None);
    };

    let _registration = private::FileLock::wait_short(&root.join("registration.lock"))?;
    for lock in LIFETIME_LOCKS {
        if private::FileLock::held(&root.join(lock))? {
            return Ok(None);
        }
    }

    let destination = root.with_extension(format!("{}.recording", identity_text(identity)));
    std::fs::rename(root, &destination)?;
    private::sync_parent(&destination)?;
    Ok(Some(destination))
}

/// Whether `root` holds a recording an older version wrote, which no writer of this version may
/// append to.
pub fn is_older(root: &Path) -> io::Result<bool> {
    Ok(older_identity(root)?.is_some())
}

fn older_identity(root: &Path) -> io::Result<Option<Identity>> {
    match format::older_writer_identity(root) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::Unsupported
            ) =>
        {
            Ok(None)
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::recording::random_identity;
    use crate::ring::Identity;

    const OLDER_IDENTITY: &[u8] = b"00000000000000010000000000000002\n";
    const OLDER_DESTINATION: &str = "flightdeck.00000000000000010000000000000002.recording";

    struct Fixture {
        parent: PathBuf,
        root: PathBuf,
    }

    impl Fixture {
        fn empty() -> Self {
            let token = identity_text(random_identity().unwrap());
            let parent = std::env::temp_dir().join(format!("flightdeck-set-aside-{token}"));
            private::create_private_dir(&parent).unwrap();
            let root = parent.join("flightdeck.recording");
            Self { parent, root }
        }

        fn with_identity(bytes: &[u8]) -> Self {
            let fixture = Self::empty();
            private::create_private_dir(&fixture.root).unwrap();
            private::atomic_write(&fixture.root, "identity", bytes).unwrap();
            fixture
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.parent).unwrap();
        }
    }

    #[test]
    fn moves_an_older_recording_beside_itself_with_its_files() {
        // Goal: a recording no writer here accepts leaves room for a new one and loses nothing.
        // Method: set aside a root holding the 33-byte older identity and one saved file.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        private::atomic_write(&fixture.root, "runs", b"saved").unwrap();

        let moved = set_aside_older(&fixture.root).unwrap().unwrap();

        assert_eq!(moved, fixture.parent.join(OLDER_DESTINATION));
        assert!(!fixture.root.exists());
        assert_eq!(std::fs::read(moved.join("runs")).unwrap(), b"saved");
    }

    #[test]
    fn keeps_a_recording_this_version_writes() {
        // Goal: an ordinary launch never moves the recording it is about to continue.
        // Method: create the current format, then ask for it to be set aside.
        let fixture = Fixture::empty();
        private::create_private_dir(&fixture.root).unwrap();
        format::create(&fixture.root, Identity([1, 2])).unwrap();

        let moved = set_aside_older(&fixture.root).unwrap();

        assert_eq!(moved, None);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_a_recording_only_a_newer_writer_accepts() {
        // Goal: going back to an older app leaves the newer app's recording where it finds it.
        // Method: require writer protocol 2, which this version refuses without being older.
        let fixture = Fixture::with_identity(
            b"flightdeck-recording-1\n00000001\n00000002\n00000000000000010000000000000002\n",
        );

        let moved = set_aside_older(&fixture.root).unwrap();

        assert_eq!(moved, None);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_a_recording_only_a_newer_reader_opens() {
        // Goal: a recording this version cannot even read stays where it finds it.
        // Method: require reader protocol 2.
        let fixture = Fixture::with_identity(
            b"flightdeck-recording-1\n00000002\n00000001\n00000000000000010000000000000002\n",
        );

        let moved = set_aside_older(&fixture.root).unwrap();

        assert_eq!(moved, None);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn finds_nothing_to_move_without_a_recording() {
        // Goal: the first launch on this Mac moves nothing and reports no failure.
        // Method: ask for a root that does not exist.
        let fixture = Fixture::empty();

        assert_eq!(set_aside_older(&fixture.root).unwrap(), None);
    }

    #[test]
    fn moves_an_older_recording_whose_parent_is_a_link() {
        // Goal: a linked application support directory still reports where the recording went.
        // Method: reach the older root through a symbolic link to its parent.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let link = fixture.parent.join("link");
        std::os::unix::fs::symlink(&fixture.parent, &link).unwrap();

        let moved = set_aside_older(&link.join("flightdeck.recording"))
            .unwrap()
            .unwrap();

        assert!(moved.join("identity").exists());
    }

    #[test]
    fn reports_a_recording_as_older_only_when_a_writer_here_refuses_it_for_its_age() {
        // Goal: a launch tells the recording it must set aside from every other it may find.
        // Method: ask of an older root, a current one, and a root that does not exist.
        let older = Fixture::with_identity(OLDER_IDENTITY);
        let current = Fixture::empty();
        private::create_private_dir(&current.root).unwrap();
        format::create(&current.root, Identity([1, 2])).unwrap();
        let missing = Fixture::empty();

        assert!(is_older(&older.root).unwrap());
        assert!(!is_older(&current.root).unwrap());
        assert!(!is_older(&missing.root).unwrap());
    }

    #[test]
    fn keeps_an_older_recording_its_controller_still_holds() {
        // Goal: a controller between two worker starts goes on using the path it was given.
        // Method: hold the controller's lifetime lock while asking for the root to be set aside.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let _recorder = private::FileLock::try_lock(&fixture.root.join("recorder.lock")).unwrap();

        let moved = set_aside_older(&fixture.root).unwrap();

        assert_eq!(moved, None);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_an_older_recording_its_collector_still_holds() {
        // Goal: a collector an older version left running keeps the path it writes under.
        // Method: hold the collector's lifetime lock while asking for the root to be set aside.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let _collector = private::FileLock::try_lock(&fixture.root.join("collector.lock")).unwrap();

        let moved = set_aside_older(&fixture.root).unwrap();

        assert_eq!(moved, None);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_an_older_recording_its_persister_still_holds() {
        // Goal: a persister still saving an older run keeps the path it writes under.
        // Method: hold the persister's lifetime lock while asking for the root to be set aside.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let _persister = private::FileLock::try_lock(&fixture.root.join("persister.lock")).unwrap();

        let moved = set_aside_older(&fixture.root).unwrap();

        assert_eq!(moved, None);
        assert!(fixture.root.join("identity").exists());
    }
}
