//! Deletes a recording this version's writers refuse, so that a new one can start in its place.
use std::io;
use std::path::Path;

use super::{Registry, format, private};
use crate::diagnostics;
use crate::ring::unlink_generation;

/// Every process that may go on using a recording's path holds one of these for its lifetime.
const LIFETIME_LOCKS: [&str; 3] = ["recorder.lock", "collector.lock", "persister.lock"];

/// Deletes a recording an older version wrote, which no writer of this version may append to, so
/// that the next recorder starts a new one at `root`, and returns whether it did. A recording a
/// process still uses stays, since that process goes on using its path. The recording takes a name
/// beside `root` in one step before any file goes, so an interruption leaves the whole recording
/// or a directory whose deletion the next call finishes, and never a part of one at `root`, which
/// a recorder would refuse as corrupt.
pub fn delete_older(root: &Path) -> io::Result<bool> {
    let deletion = root.with_extension("deleting");
    delete_older_finish(&deletion)?;
    if !is_older(root)? {
        return Ok(false);
    }

    {
        let _registration = private::FileLock::wait_short(&root.join("registration.lock"))?;
        // Another launch may have deleted it and started a new recording here during the wait.
        if delete_older_held(root)? || !is_older(root)? {
            return Ok(false);
        }
        std::fs::rename(root, &deletion)?;
        private::sync_parent(&deletion)?;
    }

    delete_older_finish(&deletion)?;
    Ok(true)
}

fn delete_older_held(root: &Path) -> io::Result<bool> {
    for lock in LIFETIME_LOCKS {
        if private::FileLock::held(&root.join(lock))? {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Removes what the deletion name holds. A launch may name any path as its recording, so this name
/// may hold one in use, which stays, or a link, whose target's rings are not this call's to remove.
fn delete_older_finish(deletion: &Path) -> io::Result<()> {
    match private::PrivateDirectory::open(deletion) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    if delete_older_held(deletion)? {
        return Ok(());
    }

    delete_older_finish_rings(deletion);
    std::fs::remove_dir_all(deletion)
}

/// Removes the rings the recording's run list names, since each holds its memory until the Mac
/// restarts and no later collector reclaims a ring that names a run. A list or a ring that cannot
/// be read is reported and stops nothing, so it can never keep a new recording from starting.
fn delete_older_finish_rings(deletion: &Path) {
    let registry = match Registry::read(deletion) {
        Ok(registry) => registry,
        Err(error) => {
            diagnostics::error(format_args!(
                "Could not read the rings of the older recording at {}: {error}",
                deletion.display()
            ));
            return;
        }
    };

    for run in &registry.runs {
        match unlink_generation(&run.ring_name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            // A ring's name may hold a token, which other users must not learn from the log.
            Err(error) => diagnostics::error(format_args!(
                "Could not remove a ring of the older recording: {error}"
            )),
        }
    }
}

/// Whether `root` holds a recording an older version wrote, which no writer of this version may
/// append to.
pub fn is_older(root: &Path) -> io::Result<bool> {
    match format::requires_older_writer(root) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::Unsupported
            ) =>
        {
            Ok(false)
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;
    use crate::recording::{RegisteredRun, identity_text, random_identity};
    use crate::ring::{Identity, Place, RingReader, RunIdentity, Shape, Writer};

    const OLDER_IDENTITY: &[u8] = b"00000000000000010000000000000002\n";

    struct Fixture {
        parent: PathBuf,
        root: PathBuf,
    }

    impl Fixture {
        fn empty() -> Self {
            let token = identity_text(random_identity().unwrap());
            let parent = std::env::temp_dir().join(format!("flightdeck-delete-older-{token}"));
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

        fn deletion(&self) -> PathBuf {
            self.parent.join("flightdeck.deleting")
        }

        /// A directory of `root` whose file `remove_dir_all` cannot remove while its mode holds.
        fn with_unremovable_directory(&self) {
            let directory = self.root.join("data");
            private::create_private_dir(&directory).unwrap();
            private::atomic_write(&directory, "saved", b"saved").unwrap();
            set_mode(&directory, 0o500);
        }

        /// Names a retained ring in the run list, as a collector an older version ran leaves it.
        fn with_retained_ring(&self) -> String {
            let place = Place {
                lock_path: self.parent.join("ring.lock"),
                name_prefix: format!("/fdd{}", std::process::id()),
            };
            let shape = Shape {
                slot_count: 2,
                cpu_capacity: 1,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: 1,
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity: 2,
            };
            let writer = Writer::create_recording(&place, shape, &run_identity(), 0).unwrap();
            let ring_name = writer.name().to_owned();
            self.with_run_list_naming(&ring_name);
            ring_name
        }

        fn with_run_list_naming(&self, ring_name: &str) {
            let registry = Registry {
                runs: vec![RegisteredRun {
                    identity: run_identity(),
                    ring_name: ring_name.to_owned(),
                    collector_pid: 1,
                }],
                evictions: 0,
            };
            registry.write(&self.root).unwrap();
        }

        fn entries(&self) -> Vec<std::ffi::OsString> {
            std::fs::read_dir(&self.parent)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for directory in [self.root.join("data"), self.deletion().join("data")] {
                if directory.exists() {
                    set_mode(&directory, 0o700);
                }
            }
            std::fs::remove_dir_all(&self.parent).unwrap();
        }
    }

    fn run_identity() -> RunIdentity {
        RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        }
    }

    fn identity_requiring(reader: u32, writer: u32) -> Vec<u8> {
        format!(
            "flightdeck-recording-1\n{reader:08x}\n{writer:08x}\n\
             00000000000000010000000000000002\n"
        )
        .into_bytes()
    }

    fn set_mode(directory: &Path, mode: u32) {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn deletes_an_older_recording_with_its_files() {
        // Goal: a recording no writer here accepts is gone, and nothing of it stays beside it.
        // Method: delete a root holding the 33-byte older identity and a saved file in a directory,
        // and list what its parent holds afterwards.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let directory = fixture.root.join("data");
        private::create_private_dir(&directory).unwrap();
        private::atomic_write(&directory, "saved", b"saved").unwrap();

        let deleted = delete_older(&fixture.root).unwrap();

        assert!(deleted);
        assert_eq!(fixture.entries(), Vec::<std::ffi::OsString>::new());
    }

    #[test]
    fn deletes_a_recording_that_requires_the_previous_writer() {
        // Goal: the root the previous writer protocol left, which a real Mac holds, is older too.
        // Method: delete a root in the full envelope that names the previous protocol.
        let fixture = Fixture::with_identity(&identity_requiring(
            format::WRITER_PROTOCOL - 1,
            format::WRITER_PROTOCOL - 1,
        ));

        assert!(delete_older(&fixture.root).unwrap());
        assert!(!fixture.root.exists());
    }

    #[test]
    fn keeps_a_recording_this_version_writes() {
        // Goal: an ordinary launch never deletes the recording it is about to continue.
        // Method: create the current format, then ask for it to be deleted.
        let fixture = Fixture::empty();
        private::create_private_dir(&fixture.root).unwrap();
        format::create(&fixture.root, Identity([1, 2])).unwrap();

        let deleted = delete_older(&fixture.root).unwrap();

        assert!(!deleted);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_a_recording_only_a_newer_writer_accepts() {
        // Goal: going back to an older app leaves the newer app's recording where it finds it.
        // Method: require a writer protocol past this one, which this version refuses for being
        // too new and not too old.
        let fixture = Fixture::with_identity(&identity_requiring(
            format::READER_PROTOCOL,
            format::WRITER_PROTOCOL + 1,
        ));

        let deleted = delete_older(&fixture.root).unwrap();

        assert!(!deleted);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_a_recording_only_a_newer_reader_opens() {
        // Goal: a recording this version cannot even read stays where it finds it.
        // Method: require a reader protocol past the newest this version reads.
        let fixture = Fixture::with_identity(&identity_requiring(
            format::READER_PROTOCOL_MAX + 1,
            format::WRITER_PROTOCOL,
        ));

        let deleted = delete_older(&fixture.root).unwrap();

        assert!(!deleted);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_a_root_it_cannot_read_as_a_recording() {
        // Goal: an identity this version cannot parse is never taken for an older recording, since
        // the deletion cannot be undone.
        // Method: ask for a root whose identity file holds other text to be deleted.
        let fixture = Fixture::with_identity(b"not a recording\n");

        let refusal = delete_older(&fixture.root);

        assert_eq!(
            refusal.map_err(|error| error.kind()),
            Err(io::ErrorKind::InvalidData)
        );
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_a_root_in_the_previous_envelope_that_names_no_recording() {
        // Goal: an identity no writer ever chose is damage and not an older recording, since the
        // deletion cannot be undone.
        // Method: ask for a root in the previous protocol's envelope whose identity is all zero.
        let fixture = Fixture::with_identity(
            b"flightdeck-recording-1\n00000001\n00000001\n\
              00000000000000000000000000000000\n",
        );

        let refusal = delete_older(&fixture.root);

        assert_eq!(
            refusal.map_err(|error| error.kind()),
            Err(io::ErrorKind::InvalidData)
        );
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_a_root_in_the_oldest_form_that_names_no_recording() {
        // Goal: the bare identity of the oldest recordings is held to the same rule.
        // Method: ask for a root whose 33-byte identity is all zero.
        let fixture = Fixture::with_identity(b"00000000000000000000000000000000\n");

        let refusal = delete_older(&fixture.root);

        assert_eq!(
            refusal.map_err(|error| error.kind()),
            Err(io::ErrorKind::InvalidData)
        );
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn finds_nothing_to_delete_without_a_recording() {
        // Goal: the first launch on this Mac deletes nothing and reports no failure.
        // Method: ask for a root that does not exist.
        let fixture = Fixture::empty();

        assert!(!delete_older(&fixture.root).unwrap());
    }

    #[test]
    fn deletes_an_older_recording_whose_parent_is_a_link() {
        // Goal: a linked application support directory does not stop the deletion.
        // Method: reach the older root through a symbolic link to its parent.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let link = fixture.parent.join("link");
        std::os::unix::fs::symlink(&fixture.parent, &link).unwrap();

        let deleted = delete_older(&link.join("flightdeck.recording")).unwrap();

        assert!(deleted);
        assert!(!fixture.root.exists());
    }

    #[test]
    fn reports_a_recording_as_older_only_when_a_writer_here_refuses_it_for_its_age() {
        // Goal: a launch tells the recording it must delete from every other it may find.
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
        // Method: hold the controller's lifetime lock while asking for the root to be deleted.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let _recorder = private::FileLock::try_lock(&fixture.root.join("recorder.lock")).unwrap();

        let deleted = delete_older(&fixture.root).unwrap();

        assert!(!deleted);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_an_older_recording_its_collector_still_holds() {
        // Goal: a collector an older version left running keeps the path it writes under.
        // Method: hold the collector's lifetime lock while asking for the root to be deleted.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let _collector = private::FileLock::try_lock(&fixture.root.join("collector.lock")).unwrap();

        let deleted = delete_older(&fixture.root).unwrap();

        assert!(!deleted);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_an_older_recording_its_persister_still_holds() {
        // Goal: a persister still saving an older run keeps the path it writes under.
        // Method: hold the persister's lifetime lock while asking for the root to be deleted.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let _persister = private::FileLock::try_lock(&fixture.root.join("persister.lock")).unwrap();

        let deleted = delete_older(&fixture.root).unwrap();

        assert!(!deleted);
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn keeps_an_older_recording_a_process_is_registering_in() {
        // Goal: a process inside a registration change never has its recording deleted under it.
        // Method: hold the registration lock while asking for the root to be deleted, and expect
        // the wait for it to end in a refusal.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let _registration =
            private::FileLock::try_lock(&fixture.root.join("registration.lock")).unwrap();

        let refusal = delete_older(&fixture.root);

        assert_eq!(
            refusal.map_err(|error| error.kind()),
            Err(io::ErrorKind::WouldBlock)
        );
        assert!(fixture.root.join("identity").exists());
    }

    #[test]
    fn a_deletion_stopped_partway_leaves_no_part_of_the_recording_at_its_path() {
        // Goal: a recorder finds a whole older recording or none, and never a part it would refuse
        // as corrupt, so a new recording can always start.
        // Method: make one directory unremovable so the deletion fails after it began, then look
        // at the recording's path.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        fixture.with_unremovable_directory();

        let stopped = delete_older(&fixture.root);

        assert!(stopped.is_err());
        assert!(!fixture.root.exists());
        assert!(fixture.deletion().join("data/saved").exists());
    }

    #[test]
    fn finishes_a_deletion_an_earlier_launch_left() {
        // Goal: an interrupted deletion does not hold its files on disk for good, whatever the
        // recording at the path is by the next launch.
        // Method: leave a part of a recording under the deletion name, with no recording at the
        // path, and ask again.
        let fixture = Fixture::empty();
        private::create_private_dir(&fixture.deletion()).unwrap();
        private::atomic_write(&fixture.deletion(), "saved", b"saved").unwrap();

        let deleted = delete_older(&fixture.root).unwrap();

        assert!(!deleted);
        assert!(!fixture.deletion().exists());
    }

    #[test]
    fn keeps_what_a_process_holds_under_the_deleting_name() {
        // Goal: a launch may name any path, so a recording in use at the deletion name stays.
        // Method: hold a controller's lifetime lock in a directory of that name and ask again.
        let fixture = Fixture::empty();
        private::create_private_dir(&fixture.deletion()).unwrap();
        private::atomic_write(&fixture.deletion(), "saved", b"saved").unwrap();
        let _recorder =
            private::FileLock::try_lock(&fixture.deletion().join("recorder.lock")).unwrap();

        delete_older(&fixture.root).unwrap();

        assert!(fixture.deletion().join("saved").exists());
    }

    #[test]
    fn leaves_the_rings_of_what_a_link_at_the_deleting_name_points_to() {
        // Goal: a link at the deletion name costs the directory it points to nothing.
        // Method: link that name to a recording naming a real ring, ask again, and open the ring.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let ring_name = fixture.with_retained_ring();
        std::os::unix::fs::symlink(&fixture.root, fixture.deletion()).unwrap();

        let refusal = delete_older(&fixture.root);

        assert!(refusal.is_err());
        assert!(RingReader::open_generation(&ring_name).is_ok());
        assert!(fixture.root.join("identity").exists());
        unlink_generation(&ring_name).unwrap();
    }

    #[test]
    fn releases_the_rings_an_older_recording_kept() {
        // Goal: a deleted recording leaves none of its rings holding memory until the Mac restarts.
        // Method: name a real retained ring in an older root's run list, delete the root, and try
        // to open the ring.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        let ring_name = fixture.with_retained_ring();

        assert!(delete_older(&fixture.root).unwrap());

        let opened = RingReader::open_generation(&ring_name);
        assert_eq!(
            opened.err().map(|error| error.kind()),
            Some(io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn deletes_an_older_recording_whose_rings_are_gone() {
        // Goal: a restart of the Mac, which removes every ring, does not stop the deletion.
        // Method: name a ring that does not exist in the run list.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        fixture.with_run_list_naming(&format!("/fdg{}", std::process::id()));

        assert!(delete_older(&fixture.root).unwrap());
        assert!(!fixture.root.exists());
    }

    #[test]
    fn deletes_an_older_recording_whose_run_list_cannot_be_read() {
        // Goal: a damaged run list never keeps a new recording from starting.
        // Method: put other text in the run list of an older root.
        let fixture = Fixture::with_identity(OLDER_IDENTITY);
        private::atomic_write(&fixture.root, "runs", b"not a run list").unwrap();

        assert!(delete_older(&fixture.root).unwrap());
        assert!(!fixture.root.exists());
    }
}
