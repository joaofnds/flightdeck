#[cfg(test)]
#[path = "encoder_fixture/mod.rs"]
mod engine_fixture;

#[cfg(test)]
mod tests {
    use super::engine_fixture;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

    use collector::recording::RegisteredRun;
    use collector::ring::{ObservationTimes, Place, Readings, Writer};
    use persistence::canonical::ReadWork;
    use persistence::canonical::catalog::Catalog;
    use persistence::resources::ByteBudget;
    use persistence::worker::{Derivation, Derive, HeldWindows};
    use persistence::{Manifest, Store, TableManifest};

    static NEXT: AtomicU64 = AtomicU64::new(1);
    /// Held by each test for as long as its recording lives, shared with the other tests or,
    /// by a test that starts a child process, alone. A child holds a copy of every descriptor
    /// of this process until it runs its program, the recordings' locks included, so a test
    /// that releases a lock while another test starts a child can find the lock still held.
    static TURNS: RwLock<()> = RwLock::new(());

    struct Recording {
        path: PathBuf,
        _turn: Turn,
    }

    enum Turn {
        Shared {
            _guard: RwLockReadGuard<'static, ()>,
        },
        Alone {
            _guard: RwLockWriteGuard<'static, ()>,
        },
    }

    impl Recording {
        fn new() -> Self {
            let guard = TURNS.read().unwrap_or_else(PoisonError::into_inner);
            Self::taking(Turn::Shared { _guard: guard })
        }

        /// The recording of a test that starts a child process, which no other test of this
        /// binary runs beside.
        fn alone() -> Self {
            let guard = TURNS.write().unwrap_or_else(PoisonError::into_inner);
            Self::taking(Turn::Alone { _guard: guard })
        }

        fn taking(turn: Turn) -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            Self {
                path: PathBuf::from(format!(
                    "/private/tmp/flightdeck-store-{}-{serial}",
                    std::process::id()
                )),
                _turn: turn,
            }
        }
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            if self.path.exists() {
                fs::remove_dir_all(&self.path).unwrap();
            }
        }
    }

    /// A window of the encoder's fixture, saved where a reader of the recording finds it, with the
    /// tables `derive` derives.
    fn saved_window(recording: &Recording, derive: &dyn Derive) -> Manifest {
        let store = Store::open(&recording.path).unwrap();
        let mut window = engine_fixture::window(false);
        window.identity.run.recording = store.recording();
        drop(store);

        persistence::canonical::ready_fixture::publish_window_derived(
            &recording.path,
            &window,
            derive,
        )
        .unwrap()
    }

    /// Derives the named tables, each a file of no meaning that a reader of the manifest never
    /// opens.
    struct NamedTables(&'static [&'static str]);

    impl Derive for NamedTables {
        fn derive(&self, derivation: &Derivation<'_>) -> persistence::Result<Vec<TableManifest>> {
            let mut tables = Vec::with_capacity(self.0.len());
            for name in self.0 {
                derivation
                    .pending
                    .create_file(&format!("{name}.parquet"))
                    .unwrap();
                tables.push(TableManifest { name, rows: 3 });
            }

            Ok(tables)
        }
    }

    /// Saves one minute of a real run with the persister's own worker.
    fn saved_minute(root: &std::path::Path, lock: &std::path::Path) {
        let store = Store::open(root).unwrap();
        let identity = collector::recording::new_run_identity(store.recording()).unwrap();
        let mut writer = Writer::create_recording(
            &Place {
                lock_path: lock.to_path_buf(),
                name_prefix: format!("/fdstore{}", std::process::id()),
            },
            engine_fixture::profile(false),
            &identity,
            0,
        )
        .unwrap();
        writer.publish_observed(
            &Readings {
                run_tick: 0,
                cpus: &[],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: collector::ring::Dropped::default(),
            },
            &ObservationTimes {
                wall_ns: 1_000_000_000_000,
                continuous_ns: 1_000_000_000,
                awake_ns: 1_000_000_000,
            },
        );
        let run = RegisteredRun {
            identity,
            ring_name: writer.name().to_owned(),
            collector_pid: 0,
        };
        persistence::worker::seal_held_windows(
            &store,
            &run,
            None,
            &mut HeldWindows::default(),
            &NamedTables(&[]),
        )
        .unwrap();
        collector::ring::unlink_generation(writer.name()).unwrap();
    }

    /// Checks every directory and file under `root` is private, and gives the files.
    fn private_files(root: &std::path::Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut walking = vec![root.to_path_buf()];
        for _directory in 0..1 << 12 {
            let Some(directory) = walking.pop() else {
                return files;
            };
            collector::recording::private::verify_directory(&directory).unwrap();
            for entry in fs::read_dir(&directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    walking.push(entry.path());
                } else {
                    collector::recording::private::verify_file(&entry.path()).unwrap();
                    files.push(entry.path());
                }
            }
        }
        panic!("more directories than one minute's recording holds");
    }

    #[test]
    fn everything_a_saved_minute_leaves_in_a_recording_is_private() {
        // Goal: the modes of a recording's files and directories never grant another user access.
        // Method: save a minute through the real worker, then inspect everything under the root.
        let recording = Recording::new();
        let lock = recording.path.with_extension("ring.lock");

        saved_minute(&recording.path, &lock);

        let files = private_files(&recording.path);
        fs::remove_file(lock).unwrap();
        for name in ["identity", "registration.lock", "persister.lock"] {
            assert!(files.contains(&recording.path.join(name)), "{name}");
        }
        assert!(files.iter().any(|file| {
            file.extension()
                .is_some_and(|extension| extension == "parquet")
        }));
    }

    #[test]
    fn a_window_reopens_with_the_app_ranks_it_derived_when_it_sealed() {
        // Goal: a window seals the table it derives beside its own, and a reader finds it.
        // Method: save a window that lists app ranks after its tables, then read its proof.
        let recording = Recording::new();

        let saved = saved_window(&recording, &NamedTables(&["app_ranks"]));

        let read = persistence::read_committed(
            &recording.path,
            &persistence::commitment_path(&recording.path, &saved),
        )
        .unwrap();
        assert_eq!(read, saved);
        assert_eq!("app_ranks", read.tables.last().unwrap().name);
    }

    #[test]
    fn a_window_reopens_with_the_app_parts_it_derived_when_it_sealed() {
        // Goal: a window seals every App's part of its observations beside its ranks, and a reader
        // finds both.
        // Method: save a window that lists app ranks and app parts after its tables, then read its
        // proof.
        let recording = Recording::new();

        let saved = saved_window(&recording, &NamedTables(&["app_ranks", "app_parts"]));

        let read = persistence::read_committed(
            &recording.path,
            &persistence::commitment_path(&recording.path, &saved),
        )
        .unwrap();
        assert_eq!(read, saved);
        assert_eq!("app_parts", read.tables.last().unwrap().name);
    }

    #[test]
    fn committed_readers_do_not_need_the_persisters_lifetime_lock() {
        // Goal: live queries can inspect saved files while the persister still owns the recording.
        // Method: read a window's proof and list the window while a store holds the recording.
        let recording = Recording::new();
        let saved = saved_window(&recording, &NamedTables(&[]));
        let store = Store::open(&recording.path).unwrap();

        let read = persistence::read_committed(
            &recording.path,
            &persistence::commitment_path(&recording.path, &saved),
        )
        .unwrap();
        let budget = Arc::new(ByteBudget::new(1 << 28));
        let listed = Catalog::open(&recording.path, &budget)
            .unwrap()
            .windows_of(&saved.identity.run, &mut ReadWork::default())
            .unwrap();

        assert_eq!(read, saved);
        assert_eq!(listed, [saved]);
        assert!(Store::open(&recording.path).is_err());
        drop(store);
    }

    #[test]
    fn rereading_a_commitment_returns_the_manifest_the_full_read_accepted() {
        // Goal: a reader holding a window open sees the same commitment without a full walk.
        // Method: save a window, read its proof in full, then reread it.
        let recording = Recording::new();
        let saved = saved_window(&recording, &NamedTables(&[]));
        let proof = persistence::commitment_path(&recording.path, &saved);
        let read = persistence::read_committed(&recording.path, &proof).unwrap();

        let reread = persistence::reread_committed(&recording.path, &proof).unwrap();

        assert_eq!(read, reread);
    }

    #[test]
    fn rereading_a_removed_commitment_reports_it_missing() {
        // Goal: a window evicted while a reader holds it open stops being readable.
        // Method: save a window, remove its proof, and reread it.
        let recording = Recording::new();
        let saved = saved_window(&recording, &NamedTables(&[]));
        let proof = persistence::commitment_path(&recording.path, &saved);
        fs::remove_file(&proof).unwrap();

        let result = persistence::reread_committed(&recording.path, &proof);

        assert_eq!(std::io::ErrorKind::NotFound, result.err().unwrap().kind());
    }

    #[test]
    fn rereading_a_proof_that_is_a_symbolic_link_refuses_it() {
        // Goal: a link cannot stand in for the window's own proof.
        // Method: replace a window's proof with a symbolic link to a copy of it, and reread it.
        let recording = Recording::new();
        let saved = saved_window(&recording, &NamedTables(&[]));
        let proof = persistence::commitment_path(&recording.path, &saved);
        let copy = recording.path.with_extension("proof-copy");
        fs::rename(&proof, &copy).unwrap();
        std::os::unix::fs::symlink(&copy, &proof).unwrap();

        let result = persistence::reread_committed(&recording.path, &proof);

        fs::remove_file(copy).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn a_second_owner_is_refused_until_the_first_owner_exits() {
        // Goal: a surviving persister owns disk commits without blocking the startup lock.
        // Method: contend for lifetime ownership and separately take the registration lock.
        let recording = Recording::new();
        let owner = Store::open(&recording.path).unwrap();

        let refused = Store::open(&recording.path);
        let registration = owner.registration_lock().unwrap();

        let error = refused.err().unwrap();
        assert_eq!(std::io::ErrorKind::WouldBlock, error.kind());
        assert!(persistence::ownership_contended(&error));
        drop(registration);
        drop(owner);
        Store::open(&recording.path).unwrap();
    }

    #[test]
    fn registration_contention_is_distinct_from_a_surviving_persister() {
        // Goal: startup coordination cannot be mistaken for existing persistence ownership.
        // Method: refuse opening while holding only the short registration lock.
        let recording = Recording::new();
        let store = Store::open(&recording.path).unwrap();
        let lock = store.registration_lock().unwrap();
        drop(store);

        let error = Store::open(&recording.path).err().unwrap();

        assert_eq!(std::io::ErrorKind::WouldBlock, error.kind());
        assert!(!persistence::ownership_contended(&error));
        drop(lock);
    }

    #[test]
    fn opening_a_store_waits_for_a_short_registration_transaction() {
        // Goal: a live registry transaction cannot make a newly launched persister disappear.
        // Method: hold the native registration lock for 100 ms while opening a fresh owner.
        let recording = Recording::new();
        let store = Store::open(&recording.path).unwrap();
        let recording_identity = store.recording();
        let lock = store.registration_lock().unwrap();
        drop(store);
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(lock);
        });

        let reopened = Store::open(&recording.path);

        release.join().unwrap();
        assert_eq!(recording_identity, reopened.unwrap().recording());
    }

    #[test]
    fn an_existing_unsafe_recording_is_refused_without_changing_its_permissions() {
        // Goal: setup refuses another user's accessible destination instead of repairing it.
        // Method: open an existing world-readable directory and inspect its unchanged mode.
        let recording = Recording::new();
        fs::create_dir(&recording.path).unwrap();
        fs::set_permissions(&recording.path, fs::Permissions::from_mode(0o755)).unwrap();

        let result = Store::open(&recording.path);

        assert_eq!(
            std::io::ErrorKind::PermissionDenied,
            result.err().unwrap().kind()
        );
        assert_eq!(0o755, fs::metadata(&recording.path).unwrap().mode() & 0o777);
    }

    #[test]
    fn a_symbolic_link_destination_is_refused_without_following_it() {
        // Goal: a supplied link never grants the recorder authority over its target.
        // Method: use a link to an otherwise safe private recording directory.
        let target = Recording::new();
        let store = Store::open(&target.path).unwrap();
        drop(store);
        // A second recording would wait on this test's own turn.
        let link = target.path.with_extension("link");
        std::os::unix::fs::symlink(&target.path, &link).unwrap();

        let result = Store::open(&link);

        fs::remove_file(&link).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn a_nonempty_acl_on_an_existing_recording_is_refused() {
        // Goal: mode bits cannot hide a grant to another user on an existing recording.
        // Method: attach an everyone-read ACL, refuse setup, then inspect the surviving ACL.
        let recording = Recording::alone();
        let store = Store::open(&recording.path).unwrap();
        drop(store);
        let added = std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,readattr,readextattr,readsecurity",
            ])
            .arg(&recording.path)
            .status()
            .unwrap();
        assert!(added.success());

        let result = Store::open(&recording.path);

        assert_eq!(
            std::io::ErrorKind::PermissionDenied,
            result.err().unwrap().kind()
        );
        let removed = std::process::Command::new("/bin/chmod")
            .arg("-N")
            .arg(&recording.path)
            .status()
            .unwrap();
        assert!(removed.success());
    }

    #[test]
    fn a_new_recording_strips_an_inherited_acl_before_any_readings_are_written() {
        // Goal: inherited grants cannot expose a newly created recording.
        // Method: inherit an everyone ACL from a private parent and verify the new child.
        let parent = Recording::alone();
        fs::create_dir(&parent.path).unwrap();
        fs::set_permissions(&parent.path, fs::Permissions::from_mode(0o700)).unwrap();
        let added = std::process::Command::new("/bin/chmod")
        .args([
            "+a",
            "everyone allow read,readattr,readextattr,readsecurity,file_inherit,directory_inherit",
        ])
        .arg(&parent.path)
        .status()
        .unwrap();
        assert!(added.success());
        let recording = parent.path.join("recording");

        let store = Store::open(&recording).unwrap();

        collector::recording::private::verify_directory(&recording).unwrap();
        collector::recording::private::verify_file(&recording.join("identity")).unwrap();
        drop(store);
        assert!(
            std::process::Command::new("/bin/chmod")
                .arg("-RN")
                .arg(&parent.path)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    #[ignore = "uses passwordless sudo to exercise actual second-user refusals on CI"]
    fn another_user_cannot_read_pending_or_committed_recording_artifacts() {
        // Goal: another unprivileged user cannot open any recording artifact.
        // Method: attempt actual nobody reads of pending files and then committed metadata/data.
        parquet_privacy(
            true,
            "tests::another_user_cannot_read_pending_or_committed_recording_artifacts",
        );
    }

    #[test]
    fn real_parquet_artifacts_are_private_below_an_inheriting_acl() {
        // Goal: native encoder output stays private even under an otherwise permissive ACL.
        // Method: encode in an isolated worker with umask077 and inspect pending and sealed files.
        parquet_privacy(
            false,
            "tests::real_parquet_artifacts_are_private_below_an_inheriting_acl",
        );
    }

    #[test]
    fn a_table_path_inspection_error_is_reported_before_native_file_writing() {
        // Goal: failed metadata inspection cannot be treated as permission to create a file.
        // Method: provide a regular file where the table directory must be.
        let recording = Recording::new();
        let store = Store::open(&recording.path).unwrap();
        let destination = recording.path.join("not_a_directory");
        fs::write(&destination, b"keep this file").unwrap();
        let window = engine_fixture::window(false);

        let result = persistence::Encoder.encode(&window, &destination);

        assert!(matches!(result, Err(persistence::Error::Io(error))
            if error.raw_os_error() == Some(20)));
        assert_eq!(b"keep this file", fs::read(destination).unwrap().as_slice());
        drop(store);
    }

    fn parquet_privacy(other_user: bool, test: &str) {
        const CHILD_ROOT: &str = "FLIGHTDECK_STORE_TEST_PARQUET_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            parquet_privacy_child(&PathBuf::from(root), other_user);
            return;
        }
        let recording = Recording::alone();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                test,
                "--include-ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ROOT, &recording.path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            b"verified",
            fs::read(recording.path.join("verified"))
                .unwrap()
                .as_slice()
        );
    }

    fn parquet_privacy_child(parent: &std::path::Path, other_user: bool) {
        parquet_privacy_inherited_parent(parent, other_user);
        let root = parent.join("recording");
        let lock = std::env::temp_dir().join(format!("fd-privacy-{}.lock", std::process::id()));

        saved_minute(&root, &lock);

        fs::remove_file(lock).unwrap();
        let files = private_files(&root);
        for path in &files {
            let is_table = path
                .extension()
                .is_some_and(|extension| extension == "parquet");
            if is_table {
                let bytes = fs::read(path).unwrap();
                assert_eq!(b"PAR1", &bytes[..4]);
                assert_eq!(b"PAR1", &bytes[bytes.len() - 4..]);
            }
            let has_bytes = fs::metadata(path).unwrap().len() > 0;
            if other_user && has_bytes {
                refused_for_nobody(path);
            }
        }
        fs::write(parent.join("verified"), b"verified").unwrap();
    }

    fn parquet_privacy_inherited_parent(parent: &std::path::Path, other_user: bool) {
        unsafe extern "C" {
            fn umask(mask: u16) -> u16;
        }
        let previous = unsafe { umask(0o077) };
        assert!(previous <= 0o777);
        fs::create_dir(parent).unwrap();
        let added = std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                concat!(
                    "everyone allow read,execute,readattr,readextattr,",
                    "readsecurity,file_inherit,directory_inherit"
                ),
            ])
            .arg(parent)
            .status()
            .unwrap();
        assert!(added.success());
        let control = parent.join("inherited_control");
        fs::write(&control, b"inherited ACL is active").unwrap();
        assert!(collector::recording::private::verify_file(&control).is_err());
        if other_user {
            let allowed = std::process::Command::new("sudo")
                .args(["-n", "-u", "nobody", "/bin/cat"])
                .arg(&control)
                .output()
                .unwrap();
            assert!(
                allowed.status.success(),
                "{}",
                String::from_utf8_lossy(&allowed.stderr)
            );
        }
    }

    fn refused_for_nobody(path: &std::path::Path) {
        let accessible = fs::read(path).unwrap();
        assert!(!accessible.is_empty());
        let result = std::process::Command::new("sudo")
            .args(["-n", "-u", "nobody", "/bin/cat"])
            .arg(path)
            .output()
            .unwrap();
        assert_eq!(Some(1), result.status.code());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains("Permission denied"), "{error}");
        assert!(result.stdout.is_empty());
    }
}
