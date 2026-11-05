//! Moves a recording this version's writers refuse out of the way of a new one.
use std::io;
use std::path::{Path, PathBuf};

use super::{format, identity_text, private};

/// Renames a recording an older version wrote, which no writer of this version may append to,
/// so that the next recorder starts a new one at `root`. Returns where the older recording went.
/// A rename keeps every file as it was, and the name carries the recording's identity, so no
/// other recording set aside here can be in the way. A recording a process still writes stays,
/// since that process goes on using its path.
pub fn set_aside_older(root: &Path) -> io::Result<Option<PathBuf>> {
    let identity = match format::older_writer_identity(root) {
        Ok(Some(identity)) => identity,
        Ok(None) => return Ok(None),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::Unsupported
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };

    let _registration = private::FileLock::wait_short(&root.join("registration.lock"))?;
    if private::FileLock::held(&root.join("collector.lock"))? {
        return Ok(None);
    }
    if private::FileLock::held(&root.join("persister.lock"))? {
        return Ok(None);
    }

    let destination = root.with_extension(format!("{}.recording", identity_text(identity)));
    std::fs::rename(root, &destination)?;
    if let Some(parent) = destination.parent() {
        private::sync_directory(parent)?;
    }
    Ok(Some(destination))
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
