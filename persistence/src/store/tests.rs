use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use collector::recording::discovery::WindowName;

use super::{Store, partition_root, read_committed, ready_relative, visit_names};
use crate::layout::Kind;
use crate::{Losses, Manifest, TableManifest, TimeBounds, WindowIdentity};

const WINDOWS: u64 = 10_081;
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

struct History {
    store: Store,
    root: PathBuf,
    identity: WindowIdentity,
}

#[test]
fn a_history_longer_than_a_week_is_complete_for_recovery_and_read_only_discovery() {
    // Goal: complete enumeration survives more than a week of valid keys across partitions.
    // Method: create filesystem-valid immutable sets with shared opaque payloads, then read all.
    let history = History::new();

    let started = std::time::Instant::now();
    let progress = history
        .store
        .progress(&history.identity.run)
        .unwrap()
        .unwrap();
    let progress_ns = started.elapsed().as_nanos();
    let started = std::time::Instant::now();
    let discovered = history.discover();
    eprintln!(
        "history windows={WINDOWS} progress_ns={progress_ns} discovery_ns={}",
        started.elapsed().as_nanos()
    );

    assert_eq!(WINDOWS - 1, progress.identity.window);
    assert_eq!((WINDOWS - 1) * 60 + 2, progress.publication_end);
    assert_eq!(usize::try_from(WINDOWS).unwrap(), discovered.len());
    assert!(discovered.into_iter().all(|present| present));
}

impl History {
    fn new() -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!(
            "/private/tmp/flightdeck-history-{}-{serial}",
            std::process::id()
        ));
        let store = Store::open(&root).unwrap();
        let identity = WindowIdentity {
            run: collector::ring::RunIdentity {
                recording: store.recording(),
                run: collector::ring::Identity([8, 9]),
                boot: collector::ring::Identity([10, 11]),
            },
            window: 0,
        };
        let history = Self {
            store,
            root,
            identity,
        };
        history.populate();
        history
    }

    fn populate(&self) {
        let payloads = self.root.join("opaque_fixture");
        DirBuilder::new().mode(0o700).create(&payloads).unwrap();
        for table in crate::schema::TABLES {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .read(false)
                .mode(0o600)
                .open(payloads.join(table.name()))
                .unwrap();
            file.write_all(b"closed opaque filesystem fixture, not Parquet fidelity")
                .unwrap();
        }
        for window in 0..WINDOWS {
            history_window(&self.store, &self.identity, &payloads, window);
        }
    }

    fn discover(&self) -> Vec<bool> {
        let mut discovered = vec![false; usize::try_from(WINDOWS).unwrap()];
        let run = partition_root(&self.root, Kind::Data, &self.identity)
            .parent()
            .unwrap()
            .to_owned();
        visit_names(&run, |day| {
            visit_names(&run.join(day), |name| {
                let window = WindowName::parse(name)?.window;
                let identity = WindowIdentity {
                    window,
                    ..self.identity
                };
                let manifest = read_committed(&self.root, &self.store.marker_path(&identity))?;
                assert_eq!(window, manifest.identity.window);
                assert_eq!(window * 60 + 2, manifest.publication_end);
                let position = usize::try_from(window).unwrap();
                assert!(!discovered[position], "duplicate key {window}");
                discovered[position] = true;
                Ok(())
            })
        })
        .unwrap();
        discovered
    }
}

impl Drop for History {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn history_window(store: &Store, identity: &WindowIdentity, payload: &Path, window: u64) {
    let identity = WindowIdentity {
        window,
        ..*identity
    };
    history_directories(store, &identity);
    let wall_ns = window * 60_000_000_000;
    let mut manifest = Manifest {
        event_batches: None,
        identity,
        publication_first: window * 60 + 1,
        publication_end: window * 60 + 2,
        events_first: 0,
        events_end: 0,
        wall_ns: Some(TimeBounds {
            min: wall_ns,
            max: wall_ns,
        }),
        continuous_ns: None,
        awake_ns: None,
        losses: Losses::default(),
        data_directory: String::new(),
        tables: crate::schema::TABLES
            .iter()
            .map(|table| TableManifest {
                name: table.name(),
                rows: 1,
            })
            .collect(),
    };
    manifest.data_directory = ready_relative(&manifest).to_str().unwrap().to_owned();
    let directory = store.root.join(&manifest.data_directory);
    DirBuilder::new().mode(0o700).create(&directory).unwrap();
    for table in &manifest.tables {
        fs::hard_link(
            payload.join(table.name),
            directory.join(format!("{}.parquet", table.name)),
        )
        .unwrap();
    }
    let metadata = directory.join("metadata");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(false)
        .mode(0o600)
        .open(&metadata)
        .unwrap();
    manifest.write(&mut file).unwrap();
    drop(file);
    fs::hard_link(metadata, store.marker_path(&manifest.identity)).unwrap();
}

fn history_directories(store: &Store, identity: &WindowIdentity) {
    if identity.window == 0 {
        for kind in [Kind::Data, Kind::Commits] {
            let parent = partition_root(&store.root, kind, identity)
                .parent()
                .unwrap()
                .to_owned();
            DirBuilder::new().mode(0o700).create(parent).unwrap();
        }
    }
    if identity.window.is_multiple_of(1440) {
        for kind in [Kind::Data, Kind::Commits] {
            DirBuilder::new()
                .mode(0o700)
                .create(partition_root(&store.root, kind, identity))
                .unwrap();
        }
    }
}
