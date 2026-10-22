#[cfg(test)]
#[path = "encoder_fixture/mod.rs"]
mod engine_fixture;

#[cfg(test)]
mod tests {
    use super::engine_fixture;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use collector::ring::{Identity, RunIdentity};
    use persistence::{Begin, Losses, Manifest, Store, TableManifest, TimeBounds, WindowIdentity};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    struct Recording(PathBuf);

    impl Recording {
        fn new() -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            Self(PathBuf::from(format!(
                "/private/tmp/flightdeck-store-{}-{serial}",
                std::process::id()
            )))
        }
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            if self.0.exists() {
                fs::remove_dir_all(&self.0).unwrap();
            }
        }
    }

    fn identity(store: &Store) -> WindowIdentity {
        WindowIdentity {
            run: RunIdentity {
                recording: store.recording(),
                run: Identity([3, 4]),
                boot: Identity([5, 6]),
            },
            window: 17,
        }
    }

    fn manifest(identity: WindowIdentity) -> Manifest {
        Manifest {
            identity,
            publication_first: 20,
            publication_end: 23,
            events_first: 90,
            events_end: 94,
            wall_ns: Some(TimeBounds { min: 100, max: 200 }),
            continuous_ns: None,
            awake_ns: None,
            losses: Losses::default(),
            data_directory: String::new(),
            tables: [
                "ticks",
                "memory",
                "cpus",
                "interfaces",
                "gpu_states",
                "core_states",
                "fans",
                "processes",
                "coalitions",
                "events",
            ]
            .into_iter()
            .map(|name| TableManifest { name, rows: 3 })
            .collect(),
        }
    }

    fn stage(store: &Store, manifest: &Manifest) -> persistence::PendingWindow {
        let Begin::Pending(pending) = store.begin(&manifest.identity).unwrap() else {
            panic!("new window was already committed");
        };
        write_tables(&pending, manifest);
        pending
    }

    fn write_tables(pending: &persistence::PendingWindow, manifest: &Manifest) {
        for table in &manifest.tables {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .read(false)
                .mode(0o600)
                .open(pending.path().join(format!("{}.parquet", table.name)))
                .unwrap();
            file.write_all(b"actual closed table fixture").unwrap();
        }
    }

    fn submit_manifest(store: &Store, candidate: Manifest) -> Manifest {
        match store.begin(&candidate.identity).unwrap() {
            Begin::Committed(committed) => committed,
            Begin::Pending(pending) => {
                write_tables(&pending, &candidate);
                store.commit(pending, candidate).unwrap()
            }
        }
    }

    #[test]
    fn a_complete_window_reopens_by_its_fixed_key_after_wall_bounds_change() {
        // Goal: replay uses the committed identity and ranges despite a changed surviving tail.
        // Method: commit files, reopen and present the same key with different time bounds.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let recording_identity = store.recording();
        drop(store);

        let store = Store::open(&recording.0).unwrap();
        let mut changed_tail = committed.clone();
        changed_tail.wall_ns = Some(TimeBounds { min: 151, max: 199 });
        changed_tail.publication_first = 21;
        let replayed = submit_manifest(&store, changed_tail);

        assert_eq!(recording_identity, store.recording());
        assert_eq!(20, replayed.publication_first);
        assert_eq!(Some(TimeBounds { min: 100, max: 200 }), replayed.wall_ns);
        assert_eq!(committed.data_directory, replayed.data_directory);
    }

    #[test]
    fn a_ready_orphan_preserves_original_bounds_when_a_changed_tail_is_submitted() {
        // Goal: an immutable ready window survives a replay with different observed bounds.
        // Method: interrupt after rename, then submit a whole replacement manifest and files.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let result = store.commit_observed(pending, original.clone(), &mut |boundary| {
            if boundary == persistence::CommitBoundary::ReadyRenamed {
                Err(std::io::ErrorKind::Interrupted.into())
            } else {
                Ok(())
            }
        });
        assert_eq!(std::io::ErrorKind::Interrupted, result.unwrap_err().kind());
        assert!(!store.marker_path(&original.identity).try_exists().unwrap());
        drop(store);

        let store = Store::open(&recording.0).unwrap();
        let mut changed_tail = original.clone();
        changed_tail.wall_ns = Some(TimeBounds { min: 151, max: 199 });
        changed_tail.publication_first = 21;
        let replayed = submit_manifest(&store, changed_tail);

        assert_eq!(original.wall_ns, replayed.wall_ns);
        assert_eq!(original.publication_first, replayed.publication_first);
        let persisted =
            persistence::read_committed(&recording.0, &store.marker_path(&original.identity))
                .unwrap();
        assert_eq!(original.wall_ns, persisted.wall_ns);
        assert_eq!(original.publication_first, persisted.publication_first);
        assert_complete_fixture(&recording.0, &store, &persisted);
    }

    #[test]
    fn a_second_owner_is_refused_until_the_first_owner_exits() {
        // Goal: a surviving persister owns disk commits without blocking the startup lock.
        // Method: contend for lifetime ownership and separately take the registration lock.
        let recording = Recording::new();
        let owner = Store::open(&recording.0).unwrap();

        let refused = Store::open(&recording.0);
        let registration = owner.registration_lock().unwrap();

        let error = refused.err().unwrap();
        assert_eq!(std::io::ErrorKind::WouldBlock, error.kind());
        assert!(persistence::ownership_contended(&error));
        drop(registration);
        drop(owner);
        assert!(Store::open(&recording.0).is_ok());
    }

    #[test]
    fn registration_contention_is_distinct_from_a_surviving_persister() {
        // Goal: startup coordination cannot be mistaken for existing persistence ownership.
        // Method: refuse opening while holding only the short registration lock.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let lock = store.registration_lock().unwrap();
        drop(store);

        let error = Store::open(&recording.0).err().unwrap();

        assert_eq!(std::io::ErrorKind::WouldBlock, error.kind());
        assert!(!persistence::ownership_contended(&error));
        drop(lock);
    }

    #[test]
    fn opening_a_store_waits_for_a_short_registration_transaction() {
        // Goal: a live registry transaction cannot make a newly launched persister disappear.
        // Method: hold the native registration lock for 100 ms while opening a fresh owner.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let recording_identity = store.recording();
        let lock = store.registration_lock().unwrap();
        drop(store);
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(lock);
        });

        let reopened = Store::open(&recording.0);

        release.join().unwrap();
        assert_eq!(recording_identity, reopened.unwrap().recording());
    }

    #[test]
    fn a_corrupt_canonical_marker_is_an_error_instead_of_an_absent_window() {
        // Goal: corrupt recovery metadata never permits a replacement commit.
        // Method: damage the fixed marker after publication and ask for the same key.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let marker = store.marker_path(&committed.identity);
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(marker)
            .unwrap();
        file.write_all(b"broken metadata").unwrap();
        drop(file);

        let result = store.begin(&committed.identity);

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            result.err().unwrap().kind()
        );
    }

    #[test]
    fn all_recording_artifacts_are_private_before_and_after_commit() {
        // Goal: pending and committed artifact modes never grant another user access.
        // Method: inspect actual temporary/table/metadata paths before and after commit.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        inspect_private_directories(&recording.0, pending.path());
        inspect_private_tables(pending.path(), &original);

        let committed = store.commit(pending, original).unwrap();
        let data = recording.0.join(&committed.data_directory);
        let marker = store.marker_path(&committed.identity);
        inspect_private_directories(&recording.0, &data);
        inspect_private_directories(&recording.0, marker.parent().unwrap());
        inspect_private_tables(&data, &committed);
        for path in [
            recording.0.join("identity"),
            recording.0.join("registration.lock"),
            recording.0.join("persister.lock"),
            data.join("metadata"),
            marker,
        ] {
            collector::recording::private::verify_file(&path).unwrap();
        }
    }

    #[test]
    fn an_existing_unsafe_recording_is_refused_without_changing_its_permissions() {
        // Goal: setup refuses another user's accessible destination instead of repairing it.
        // Method: open an existing world-readable directory and inspect its unchanged mode.
        let recording = Recording::new();
        fs::create_dir(&recording.0).unwrap();
        fs::set_permissions(&recording.0, fs::Permissions::from_mode(0o755)).unwrap();

        let result = Store::open(&recording.0);

        assert_eq!(
            std::io::ErrorKind::PermissionDenied,
            result.err().unwrap().kind()
        );
        assert_eq!(0o755, fs::metadata(&recording.0).unwrap().mode() & 0o777);
    }

    #[test]
    fn a_symbolic_link_destination_is_refused_without_following_it() {
        // Goal: a supplied link never grants the recorder authority over its target.
        // Method: use a link to an otherwise safe private recording directory.
        let target = Recording::new();
        let store = Store::open(&target.0).unwrap();
        drop(store);
        let link = Recording::new();
        std::os::unix::fs::symlink(&target.0, &link.0).unwrap();

        let result = Store::open(&link.0);

        assert!(result.is_err());
        fs::remove_file(&link.0).unwrap();
    }

    #[test]
    fn a_nonempty_acl_on_an_existing_recording_is_refused() {
        // Goal: mode bits cannot hide a grant to another user on an existing recording.
        // Method: attach an everyone-read ACL, refuse setup, then inspect the surviving ACL.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        drop(store);
        let added = std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,readattr,readextattr,readsecurity",
            ])
            .arg(&recording.0)
            .status()
            .unwrap();
        assert!(added.success());

        let result = Store::open(&recording.0);

        assert_eq!(
            std::io::ErrorKind::PermissionDenied,
            result.err().unwrap().kind()
        );
        let removed = std::process::Command::new("/bin/chmod")
            .arg("-N")
            .arg(&recording.0)
            .status()
            .unwrap();
        assert!(removed.success());
    }

    fn interrupted_commit(boundary: persistence::CommitBoundary) {
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let result = store.commit_observed(pending, original.clone(), &mut |observed| {
            if observed == boundary {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        });
        assert_eq!(std::io::ErrorKind::Interrupted, result.unwrap_err().kind());
        drop(store);

        let store = Store::open(&recording.0).unwrap();
        let recovered = store.begin(&original.identity).unwrap();
        assert_recovery_boundary(&recovered, boundary);
        let committed = match recovered {
            Begin::Committed(committed) => committed,
            Begin::Pending(pending) => {
                write_tables(&pending, &original);
                store.commit(pending, original).unwrap()
            }
        };

        assert_eq!(20, committed.publication_first);
        assert_eq!(23, committed.publication_end);
        assert_eq!(
            committed,
            store.existing(&committed.identity).unwrap().unwrap()
        );
    }

    fn assert_recovery_boundary(recovered: &Begin, boundary: persistence::CommitBoundary) {
        use persistence::CommitBoundary;
        let committed = match boundary {
            CommitBoundary::ReadyRenamed
            | CommitBoundary::BeforeReadySync
            | CommitBoundary::ReadySynced
            | CommitBoundary::BeforeMarkerLink
            | CommitBoundary::MarkerLinked
            | CommitBoundary::BeforeMarkerSync
            | CommitBoundary::MarkerSynced => true,
            CommitBoundary::BeforeFilesSync
            | CommitBoundary::FilesSynced
            | CommitBoundary::BeforeMetadataWrite
            | CommitBoundary::MetadataSynced
            | CommitBoundary::BeforePendingSync
            | CommitBoundary::PendingSynced
            | CommitBoundary::BeforeReadyRename => false,
        };
        assert_eq!(
            committed,
            matches!(recovered, Begin::Committed(_)),
            "wrong recovery variant after {boundary:?}"
        );
    }

    macro_rules! interruption_test {
        ($name:ident, $boundary:ident) => {
            #[test]
            fn $name() {
                // Goal: interrupted publication converges to one complete immutable window.
                // Method: refuse the named durability boundary and recover with a fresh owner.
                interrupted_commit(persistence::CommitBoundary::$boundary);
            }
        };
    }

    interruption_test!(interruption_after_table_sync_recovers_once, FilesSynced);
    interruption_test!(
        interruption_after_metadata_sync_recovers_once,
        MetadataSynced
    );
    interruption_test!(interruption_after_pending_sync_recovers_once, PendingSynced);
    interruption_test!(interruption_after_ready_rename_recovers_once, ReadyRenamed);
    interruption_test!(interruption_after_ready_sync_recovers_once, ReadySynced);
    interruption_test!(interruption_after_marker_link_recovers_once, MarkerLinked);
    interruption_test!(interruption_after_marker_sync_recovers_once, MarkerSynced);

    #[test]
    fn an_overfull_partition_is_incomplete_instead_of_proving_orphan_absence() {
        // Goal: hitting the bound cannot authorize a second commit for an unobserved orphan.
        // Method: put too many entries in a partition, then ask to begin a new window there.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let partition = pending.path().parent().unwrap().to_owned();
        drop(pending);
        for index in 0..1443_u64 {
            fs::write(
                partition.join(format!("extra{index}")),
                b"incomplete fixture",
            )
            .unwrap();
        }

        let result = store.begin(&original.identity);

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            result.err().unwrap().kind()
        );
    }

    #[test]
    fn a_second_staging_window_is_refused_while_one_is_live() {
        // Goal: the store cannot accumulate an unbounded queue of pending window directories.
        // Method: keep one staging token live and try to begin another window.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let first = manifest(identity(&store));
        let pending = stage(&store, &first);
        let mut second = first.identity;
        second.window += 1;

        let result = store.begin(&second);

        assert_eq!(std::io::ErrorKind::WouldBlock, result.err().unwrap().kind());
        drop(pending);
        assert!(matches!(store.begin(&second).unwrap(), Begin::Pending(_)));
    }

    #[test]
    fn a_new_recording_strips_an_inherited_acl_before_any_readings_are_written() {
        // Goal: inherited grants cannot expose a newly created recording.
        // Method: inherit an everyone ACL from a private parent and verify the new child.
        let parent = Recording::new();
        fs::create_dir(&parent.0).unwrap();
        fs::set_permissions(&parent.0, fs::Permissions::from_mode(0o700)).unwrap();
        let added = std::process::Command::new("/bin/chmod")
        .args([
            "+a",
            "everyone allow read,readattr,readextattr,readsecurity,file_inherit,directory_inherit",
        ])
        .arg(&parent.0)
        .status()
        .unwrap();
        assert!(added.success());
        let recording = parent.0.join("recording");

        let store = Store::open(&recording).unwrap();

        collector::recording::private::verify_directory(&recording).unwrap();
        collector::recording::private::verify_file(&recording.join("identity")).unwrap();
        assert!(store.existing(&identity(&store)).unwrap().is_none());
        drop(store);
        assert!(
            std::process::Command::new("/bin/chmod")
                .arg("-RN")
                .arg(&parent.0)
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
        let store = Store::open(&recording.0).unwrap();
        let destination = recording.0.join("not_a_directory");
        fs::write(&destination, b"keep this file").unwrap();
        let window = engine_fixture::window(false);

        let result = persistence::Encoder.encode(&window, &destination);

        assert!(matches!(result, Err(persistence::Error::Io(error))
            if error.raw_os_error() == Some(20)));
        assert_eq!(b"keep this file", fs::read(destination).unwrap().as_slice());
        drop(store);
    }

    #[test]
    fn invalid_manifest_bounds_are_refused_before_a_ready_name_is_formatted() {
        // Goal: an invalid boundary value remains an operating error rather than a panic.
        // Method: stage files, then submit reversed measured wall bounds.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let mut original = manifest(identity(&store));
        let pending = stage(&store, &original);
        original.wall_ns = Some(TimeBounds { min: 200, max: 100 });

        let result = store.commit(pending, original);

        assert_eq!(std::io::ErrorKind::InvalidData, result.unwrap_err().kind());
    }

    #[test]
    fn a_window_reopens_with_the_app_ranks_it_derived_when_it_sealed() {
        // Goal: a window seals the table it derives beside its own, and a reader finds it.
        // Method: commit a window that lists app ranks after its tables, then read its marker.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let mut original = manifest(identity(&store));
        original.tables.push(app_ranks());
        let pending = stage(&store, &original);

        let committed = store.commit(pending, original).unwrap();

        assert_complete_fixture(&recording.0, &store, &committed);
        assert_eq!(Some(&app_ranks()), committed.tables.last());
    }

    #[test]
    fn a_window_listing_app_ranks_in_place_of_one_of_its_tables_is_refused() {
        // Goal: a derived table never stands in for a table every window holds.
        // Method: list app ranks in place of the events table, so the count of tables holds.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let mut original = manifest(identity(&store));
        assert_eq!("events", original.tables.pop().unwrap().name);
        original.tables.push(app_ranks());
        let pending = stage(&store, &original);

        let result = store.commit(pending, original);

        assert_eq!(std::io::ErrorKind::InvalidData, result.unwrap_err().kind());
    }

    #[test]
    fn a_window_listing_app_ranks_twice_is_refused() {
        // Goal: a window holds each derived table at most once.
        // Method: stage app ranks once, then list them twice in the manifest committed.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let mut original = manifest(identity(&store));
        original.tables.push(app_ranks());
        let pending = stage(&store, &original);
        original.tables.push(app_ranks());

        let result = store.commit(pending, original);

        assert_eq!(std::io::ErrorKind::InvalidData, result.unwrap_err().kind());
    }

    #[test]
    fn a_window_reopens_with_the_app_parts_it_derived_when_it_sealed() {
        // Goal: a window seals every App's part of its observations beside its ranks, and a
        // reader finds both.
        // Method: commit a window that lists app ranks and app parts after its tables, then read
        // its marker.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let mut original = manifest(identity(&store));
        original.tables.push(app_ranks());
        original.tables.push(app_parts());
        let pending = stage(&store, &original);

        let committed = store.commit(pending, original).unwrap();

        assert_complete_fixture(&recording.0, &store, &committed);
        assert_eq!(Some(&app_parts()), committed.tables.last());
    }

    fn app_ranks() -> TableManifest {
        TableManifest {
            name: "app_ranks",
            rows: 40,
        }
    }

    fn app_parts() -> TableManifest {
        TableManifest {
            name: "app_parts",
            rows: 9,
        }
    }

    fn parquet_privacy(other_user: bool, test: &str) {
        const CHILD_ROOT: &str = "FLIGHTDECK_STORE_TEST_PARQUET_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            parquet_privacy_child(&PathBuf::from(root), other_user);
            return;
        }
        let recording = Recording::new();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                test,
                "--include-ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ROOT, &recording.0)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            b"verified",
            fs::read(recording.0.join("verified")).unwrap().as_slice()
        );
    }

    fn parquet_privacy_child(parent: &std::path::Path, other_user: bool) {
        parquet_privacy_inherited_parent(parent, other_user);
        let root = parent.join("recording");
        let store = Store::open(&root).unwrap();
        let mut window = engine_fixture::window(false);
        window.identity.run.recording = store.recording();
        let Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
            panic!("new encoder fixture was committed");
        };
        let mut manifest = manifest(window.identity);
        manifest.publication_first = window.publication_range().start;
        manifest.publication_end = window.publication_range().end;
        manifest.events_first = window.event_range().start;
        manifest.events_end = window.event_range().end;
        manifest.wall_ns = Some(TimeBounds {
            min: 1_000_000_000_000,
            max: 1_060_000_000_000,
        });
        manifest.tables = persistence::Encoder
            .encode(&window, pending.path())
            .unwrap();
        inspect_private_directories(&root, pending.path());
        inspect_parquet_files(pending.path(), &manifest, other_user);
        let committed = store.commit(pending, manifest).unwrap();
        inspect_private_directories(&root, &root.join(&committed.data_directory));
        inspect_private_directories(
            &root,
            store.marker_path(&committed.identity).parent().unwrap(),
        );
        inspect_parquet_files(
            &root.join(&committed.data_directory),
            &committed,
            other_user,
        );
        for path in [
            root.join("identity"),
            store.marker_path(&committed.identity),
            root.join(&committed.data_directory).join("metadata"),
        ] {
            collector::recording::private::verify_file(&path).unwrap();
            if other_user {
                refused_for_nobody(&path);
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

    fn inspect_parquet_files(directory: &std::path::Path, manifest: &Manifest, other_user: bool) {
        collector::recording::private::verify_directory(directory).unwrap();
        assert_eq!(10, manifest.tables.len());
        for table in &manifest.tables {
            let path = directory.join(format!("{}.parquet", table.name));
            collector::recording::private::verify_file(&path).unwrap();
            let bytes = fs::read(&path).unwrap();
            assert_eq!(b"PAR1", &bytes[..4]);
            assert_eq!(b"PAR1", &bytes[bytes.len() - 4..]);
            if other_user {
                refused_for_nobody(&path);
            }
        }
    }

    fn inspect_private_directories(root: &std::path::Path, leaf: &std::path::Path) {
        for directory in leaf.ancestors().take(8) {
            collector::recording::private::verify_directory(directory).unwrap();
            if directory == root {
                return;
            }
        }
        panic!("artifact directory was outside the bounded recording hierarchy");
    }

    fn inspect_private_tables(directory: &std::path::Path, manifest: &Manifest) {
        assert_eq!(10, manifest.tables.len());
        for table in &manifest.tables {
            let path = directory.join(format!("{}.parquet", table.name));
            collector::recording::private::verify_file(&path).unwrap();
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

    #[test]
    fn committed_readers_do_not_need_the_persisters_lifetime_lock() {
        // Goal: live queries can inspect committed files while the persister still owns storage.
        // Method: read the canonical marker through the read-only entry point with its owner live.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();

        let read =
            persistence::read_committed(&recording.0, &store.marker_path(&committed.identity))
                .unwrap();

        assert_eq!(committed, read);
        assert!(Store::open(&recording.0).is_err());
    }

    interruption_test!(
        interruption_before_table_sync_recovers_once,
        BeforeFilesSync
    );
    interruption_test!(
        interruption_before_metadata_write_recovers_once,
        BeforeMetadataWrite
    );
    interruption_test!(
        interruption_before_pending_sync_recovers_once,
        BeforePendingSync
    );
    interruption_test!(
        interruption_before_ready_rename_recovers_once,
        BeforeReadyRename
    );
    interruption_test!(
        interruption_before_ready_sync_recovers_once,
        BeforeReadySync
    );
    interruption_test!(
        interruption_before_marker_link_recovers_once,
        BeforeMarkerLink
    );
    interruption_test!(
        interruption_before_marker_sync_recovers_once,
        BeforeMarkerSync
    );

    #[test]
    fn unsafe_surviving_pending_files_are_preserved_instead_of_repaired() {
        // Goal: recovery cannot silently change an unsafe pre-existing destination.
        // Method: leave a pending file with a public mode and verify refusal preserves it.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let table = pending.path().join("ticks.parquet");
        drop(pending);
        fs::set_permissions(&table, fs::Permissions::from_mode(0o644)).unwrap();

        let result = store.begin(&original.identity);

        assert_eq!(
            std::io::ErrorKind::PermissionDenied,
            result.err().unwrap().kind()
        );
        assert_eq!(0o644, fs::metadata(&table).unwrap().mode() & 0o777);
        assert!(store.existing(&original.identity).unwrap().is_none());
    }

    #[test]
    fn rereading_a_commitment_returns_the_manifest_the_full_read_accepted() {
        // Goal: a reader holding a window open sees the same commitment without a full walk.
        // Method: commit a window, read its marker in full, then reread it.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let marker = store.marker_path(&committed.identity);
        let read = persistence::read_committed(&recording.0, &marker).unwrap();

        let reread = persistence::reread_committed(&recording.0, &marker).unwrap();

        assert_eq!(read, reread);
    }

    #[test]
    fn rereading_a_removed_commitment_reports_it_missing() {
        // Goal: a window evicted while a reader holds it open stops being readable.
        // Method: commit a window, remove its marker, and reread it.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let marker = store.marker_path(&committed.identity);
        fs::remove_file(&marker).unwrap();

        let result = persistence::reread_committed(&recording.0, &marker);

        assert_eq!(std::io::ErrorKind::NotFound, result.err().unwrap().kind());
    }

    #[test]
    fn rereading_a_copied_marker_refuses_it() {
        // Goal: a matching payload alone cannot keep a reread commitment standing.
        // Method: replace the marker with a byte-identical independent file and reread it.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let marker = store.marker_path(&committed.identity);
        let bytes = fs::read(&marker).unwrap();
        fs::remove_file(&marker).unwrap();
        let mut replacement = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(false)
            .mode(0o600)
            .open(&marker)
            .unwrap();
        replacement.write_all(&bytes).unwrap();
        drop(replacement);

        let result = persistence::reread_committed(&recording.0, &marker);

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            result.err().unwrap().kind()
        );
    }

    #[test]
    fn rereading_a_window_whose_metadata_is_a_symbolic_link_refuses_it() {
        // Goal: a link to the marker cannot stand in for the window's own metadata hard link.
        // Method: replace the window's metadata with a symbolic link to its marker and reread.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let marker = store.marker_path(&committed.identity);
        let metadata = recording.0.join(&committed.data_directory).join("metadata");
        fs::remove_file(&metadata).unwrap();
        std::os::unix::fs::symlink(&marker, &metadata).unwrap();

        let result = persistence::reread_committed(&recording.0, &marker);

        assert!(result.is_err());
    }

    #[test]
    fn rereading_a_marker_refuses_its_data_directory_before_following_it() {
        // Goal: a reread follows a marker's data directory only where the window's identity
        // puts it, since a reader also rereads markers no full read has accepted.
        // Method: rewrite the linked metadata to name the same directory by its absolute path,
        // which still reaches the hard link, and reread the marker.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let marker = store.marker_path(&committed.identity);
        let relative = committed.data_directory.as_bytes();
        let absolute = recording.0.join(&committed.data_directory);
        let absolute = absolute.to_str().unwrap().as_bytes();
        let mut encoded = u64::try_from(relative.len())
            .unwrap()
            .to_le_bytes()
            .to_vec();
        encoded.extend_from_slice(relative);
        let bytes = fs::read(&marker).unwrap();
        let at = bytes
            .windows(encoded.len())
            .position(|window| window == encoded.as_slice())
            .unwrap();
        let mut rewritten = bytes[..at].to_vec();
        rewritten.extend_from_slice(&u64::try_from(absolute.len()).unwrap().to_le_bytes());
        rewritten.extend_from_slice(absolute);
        rewritten.extend_from_slice(&bytes[at + encoded.len()..]);
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&marker)
            .unwrap();
        file.write_all(&rewritten).unwrap();
        drop(file);

        let result = persistence::reread_committed(&recording.0, &marker);

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            result.err().unwrap().kind()
        );
    }

    #[test]
    fn a_copied_marker_cannot_replace_the_metadata_hard_link() {
        // Goal: a matching metadata payload alone cannot claim the canonical committed set.
        // Method: replace the marker with a byte-identical independent file and query it.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let marker = store.marker_path(&committed.identity);
        let bytes = fs::read(&marker).unwrap();
        fs::remove_file(&marker).unwrap();
        let mut replacement = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(false)
            .mode(0o600)
            .open(&marker)
            .unwrap();
        replacement.write_all(&bytes).unwrap();
        drop(replacement);

        let result = persistence::read_committed(&recording.0, &marker);

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            result.err().unwrap().kind()
        );
    }

    macro_rules! crash_test {
        ($name:ident, $boundary:ident) => {
            #[test]
            fn $name() {
                // Goal: a killed persister leaves a complete once-only publication after recovery.
                // Method: abort a separate process at the named boundary, then reopen its files.
                crash_and_recover(
                    persistence::CommitBoundary::$boundary,
                    concat!("tests::", stringify!($name)),
                );
            }
        };
    }

    crash_test!(a_crash_before_table_sync_recovers_once, BeforeFilesSync);
    crash_test!(a_crash_after_table_sync_recovers_once, FilesSynced);
    crash_test!(
        a_crash_before_metadata_write_recovers_once,
        BeforeMetadataWrite
    );
    crash_test!(a_crash_after_metadata_sync_recovers_once, MetadataSynced);
    crash_test!(a_crash_before_pending_sync_recovers_once, BeforePendingSync);
    crash_test!(a_crash_after_pending_sync_recovers_once, PendingSynced);
    crash_test!(a_crash_before_ready_rename_recovers_once, BeforeReadyRename);
    crash_test!(a_crash_after_ready_rename_recovers_once, ReadyRenamed);
    crash_test!(a_crash_before_ready_sync_recovers_once, BeforeReadySync);
    crash_test!(a_crash_after_ready_sync_recovers_once, ReadySynced);
    crash_test!(a_crash_before_marker_link_recovers_once, BeforeMarkerLink);
    crash_test!(a_crash_after_marker_link_recovers_once, MarkerLinked);
    crash_test!(a_crash_before_marker_sync_recovers_once, BeforeMarkerSync);
    crash_test!(a_crash_after_marker_sync_recovers_once, MarkerSynced);

    fn crash_and_recover(boundary: persistence::CommitBoundary, test_name: &str) {
        use std::os::unix::process::ExitStatusExt;
        const CHILD_ROOT: &str = "FLIGHTDECK_STORE_TEST_CRASH_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            crash_child(&PathBuf::from(root), boundary);
        }
        let recording = Recording::new();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .env(CHILD_ROOT, &recording.0)
            .output()
            .unwrap();
        assert_eq!(
            Some(6),
            result.status.signal(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));

        let recovered = store.begin(&original.identity).unwrap();
        assert_recovery_boundary(&recovered, boundary);
        let committed = match recovered {
            Begin::Committed(committed) => committed,
            Begin::Pending(pending) => {
                write_tables(&pending, &original);
                store.commit(pending, original).unwrap()
            }
        };

        assert_eq!(20, committed.publication_first);
        assert_eq!(23, committed.publication_end);
        let read =
            persistence::read_committed(&recording.0, &store.marker_path(&committed.identity))
                .unwrap();
        assert_eq!(committed, read);
        let ready = recording.0.join(&committed.data_directory);
        for table in &committed.tables {
            assert_eq!(
                b"actual closed table fixture",
                fs::read(ready.join(format!("{}.parquet", table.name)))
                    .unwrap()
                    .as_slice()
            );
        }
        assert_eq!(1, fs::read_dir(ready.parent().unwrap()).unwrap().count());
    }

    fn crash_child(root: &std::path::Path, boundary: persistence::CommitBoundary) -> ! {
        let store = Store::open(root).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let result = store.commit_observed(pending, original, &mut |observed| {
            if observed == boundary {
                std::process::abort();
            }
            Ok(())
        });
        panic!("crash boundary was not reached: {result:?}");
    }

    #[test]
    fn recovery_progress_comes_from_valid_commits_across_elapsed_day_partitions() {
        // Goal: recovery seeks the last durable publication without a mutable checkpoint.
        // Method: commit windows across eight days and inspect the maximum committed end.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let mut first = manifest(identity(&store));
        let pending = stage(&store, &first);
        store.commit(pending, first.clone()).unwrap();
        first.identity.window = 10_081;
        first.publication_first = 800;
        first.publication_end = 803;
        let pending = stage(&store, &first);
        let latest = store.commit(pending, first).unwrap();
        let mut middle = manifest(identity(&store));
        middle.identity.window = 1441;
        middle.publication_first = 400;
        middle.publication_end = 403;
        let pending = stage(&store, &middle);
        store.commit(pending, middle).unwrap();

        let progress = store.progress(&latest.identity.run).unwrap();

        assert_eq!(Some(latest), progress);
    }

    #[test]
    fn corrupt_commits_prevent_a_recovery_progress_claim() {
        // Goal: unreadable history cannot turn into an apparently complete checkpoint.
        // Method: damage a canonical marker and then derive progress for its run.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let mut marker = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(store.marker_path(&committed.identity))
            .unwrap();
        marker.write_all(b"bad").unwrap();
        drop(marker);

        let progress = store.progress(&committed.identity.run);

        assert_eq!(
            std::io::ErrorKind::InvalidData,
            progress.unwrap_err().kind()
        );
    }

    struct PartialCrash {
        table: &'static str,
        after: bool,
        test: &'static str,
    }

    macro_rules! partial_crash_test {
        ($name:ident, $table:literal, $after:literal) => {
            #[test]
            fn $name() {
                // Goal: a table-write crash never makes a partial set visible as committed.
                // Method: abort a child around one closed opaque file, then recover the full set.
                partial_crash_and_recover(&PartialCrash {
                    table: $table,
                    after: $after,
                    test: concat!("tests::", stringify!($name)),
                });
            }
        };
    }

    partial_crash_test!(a_crash_before_ticks_write_recovers_once, "ticks", false);
    partial_crash_test!(a_crash_after_ticks_write_recovers_once, "ticks", true);
    partial_crash_test!(a_crash_before_memory_write_recovers_once, "memory", false);
    partial_crash_test!(a_crash_after_memory_write_recovers_once, "memory", true);
    partial_crash_test!(a_crash_before_cpus_write_recovers_once, "cpus", false);
    partial_crash_test!(a_crash_after_cpus_write_recovers_once, "cpus", true);
    partial_crash_test!(
        a_crash_before_interfaces_write_recovers_once,
        "interfaces",
        false
    );
    partial_crash_test!(
        a_crash_after_interfaces_write_recovers_once,
        "interfaces",
        true
    );
    partial_crash_test!(
        a_crash_before_gpu_states_write_recovers_once,
        "gpu_states",
        false
    );
    partial_crash_test!(
        a_crash_after_gpu_states_write_recovers_once,
        "gpu_states",
        true
    );
    partial_crash_test!(
        a_crash_before_core_states_write_recovers_once,
        "core_states",
        false
    );
    partial_crash_test!(
        a_crash_after_core_states_write_recovers_once,
        "core_states",
        true
    );
    partial_crash_test!(a_crash_before_fans_write_recovers_once, "fans", false);
    partial_crash_test!(a_crash_after_fans_write_recovers_once, "fans", true);
    partial_crash_test!(
        a_crash_before_processes_write_recovers_once,
        "processes",
        false
    );
    partial_crash_test!(
        a_crash_after_processes_write_recovers_once,
        "processes",
        true
    );
    partial_crash_test!(
        a_crash_before_coalitions_write_recovers_once,
        "coalitions",
        false
    );
    partial_crash_test!(
        a_crash_after_coalitions_write_recovers_once,
        "coalitions",
        true
    );
    partial_crash_test!(a_crash_before_events_write_recovers_once, "events", false);
    partial_crash_test!(a_crash_after_events_write_recovers_once, "events", true);

    fn partial_crash_and_recover(case: &PartialCrash) {
        use std::os::unix::process::ExitStatusExt;
        const CHILD_ROOT: &str = "FLIGHTDECK_STORE_TEST_PARTIAL_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            partial_crash_child(&PathBuf::from(root), case);
        }
        let recording = Recording::new();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", case.test, "--nocapture", "--test-threads=1"])
            .env(CHILD_ROOT, &recording.0)
            .output()
            .unwrap();
        assert_eq!(
            Some(6),
            result.status.signal(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        assert!(store.existing(&original.identity).unwrap().is_none());

        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();

        assert_complete_fixture(&recording.0, &store, &committed);
    }

    fn partial_crash_child(root: &std::path::Path, case: &PartialCrash) -> ! {
        let store = Store::open(root).unwrap();
        let original = manifest(identity(&store));
        let Begin::Pending(pending) = store.begin(&original.identity).unwrap() else {
            panic!("fixture window was committed");
        };
        for table in &original.tables {
            if table.name == case.table {
                if !case.after {
                    std::process::abort();
                }
            }
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .read(false)
                .mode(0o600)
                .open(pending.path().join(format!("{}.parquet", table.name)))
                .unwrap();
            file.write_all(b"actual closed table fixture").unwrap();
            drop(file);
            if table.name == case.table {
                if case.after {
                    std::process::abort();
                }
            }
        }
        panic!("partial table crash boundary was not reached");
    }

    #[test]
    fn a_window_in_the_wrong_elapsed_day_is_an_error_instead_of_complete_empty() {
        // Goal: malformed partition placement cannot hide a recorded second as absent.
        // Method: move a committed ready set into a different well-formed day directory.
        let recording = Recording::new();
        let store = Store::open(&recording.0).unwrap();
        let original = manifest(identity(&store));
        let pending = stage(&store, &original);
        let committed = store.commit(pending, original).unwrap();
        let ready = recording.0.join(&committed.data_directory);
        let wrong = ready
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("00000000000000000001");
        collector::recording::private::create_private_dir(&wrong).unwrap();
        fs::rename(&ready, wrong.join(ready.file_name().unwrap())).unwrap();

        let answer = persistence::query::processes_at_second(&recording.0, 0);

        assert!(
            answer.is_err(),
            "wrong partition returned a complete empty answer"
        );
    }

    fn assert_complete_fixture(root: &std::path::Path, store: &Store, committed: &Manifest) {
        let read =
            persistence::read_committed(root, &store.marker_path(&committed.identity)).unwrap();
        assert_eq!(*committed, read);
        let ready = root.join(&committed.data_directory);
        for table in &committed.tables {
            assert_eq!(
                b"actual closed table fixture",
                fs::read(ready.join(format!("{}.parquet", table.name)))
                    .unwrap()
                    .as_slice()
            );
        }
        assert_eq!(1, fs::read_dir(ready.parent().unwrap()).unwrap().count());
    }
}
