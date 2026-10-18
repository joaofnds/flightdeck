use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use collector::ring::{Identity, RunIdentity};
use persistence::engine::Engine;
use persistence::{Error, Losses, Manifest, TableManifest, WindowIdentity};
use polars::prelude::{Column, DataFrame, ParquetWriter};

use super::super::{ComparisonTable, Reference, discover_order};
use super::Cursor;

#[test]
fn reads_ordered_windows_incrementally_without_losing_boundary_rows() {
    // Goal: table comparison needs one window at a time and keeps global publication order.
    // Method: supply reversed manifests, then create the later file only after consuming a row.
    let directory = Directory::new();
    let reference = reference();
    let mut manifests = [manifest("later", 4..6), manifest("earlier", 1..4)];
    discover_order(&mut manifests, &reference.identity).unwrap();
    directory.write("earlier", &[(3, 1), (1, 2), (2, 2), (2, 0)]);
    let connection = Engine::new();
    let mut actual = Cursor::new(
        &connection,
        &directory.0,
        &manifests,
        &reference,
        &ComparisonTable {
            label: "cpus",
            order: "publication,cpu",
        },
    )
    .unwrap();

    let first = actual.next().unwrap().unwrap();
    let mut rows = vec![(
        first.0.get::<u64>(0).unwrap(),
        first.0.get::<u32>(1).unwrap(),
    )];
    directory.write("later", &[(5, 7), (4, 9)]);
    let mut complete = false;
    for _row in 0..8 {
        let Some((row, columns)) = actual.next().unwrap() else {
            complete = true;
            break;
        };
        assert_eq!(columns, 6);
        assert_eq!(row.get::<u32>(2).unwrap(), u32::MAX);
        rows.push((row.get::<u64>(0).unwrap(), row.get::<u32>(1).unwrap()));
    }

    assert!(complete);
    assert_eq!(rows, [(2, 0), (2, 2), (3, 1), (4, 9)]);
}

#[test]
fn refuses_overlapping_window_publication_ranges() {
    // Goal: concatenating separately sorted windows cannot accept ambiguous global ordering.
    // Method: admit two manifests whose publication ranges overlap by one publication.
    let reference = reference();
    let mut manifests = [manifest("earlier", 1..4), manifest("later", 3..6)];

    let result = discover_order(&mut manifests, &reference.identity);

    assert!(matches!(
        result,
        Err(Error::Bound("reference overlapping publication ranges"))
    ));
}

#[test]
fn refuses_rows_outside_the_window_publication_range() {
    // Goal: a false manifest cannot invalidate the cursor's global ordering guarantee.
    // Method: place an otherwise selected row before its declared window range.
    let directory = Directory::new();
    directory.write("later", &[(2, 1)]);
    let reference = reference();
    let manifests = [manifest("later", 3..6)];
    let connection = Engine::new();
    let mut actual = Cursor::new(
        &connection,
        &directory.0,
        &manifests,
        &reference,
        &ComparisonTable {
            label: "cpus",
            order: "publication,cpu",
        },
    )
    .unwrap();

    let result = actual.next();

    assert!(matches!(
        result,
        Err(Error::Bound("reference table publication range"))
    ));
}

fn reference() -> Reference {
    Reference {
        identity: RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        },
        identity_capable: false,
        deadline: Instant::now() + Duration::from_secs(30),
        seconds: vec![1, 2],
        first: 2,
        newest: 4,
        publications: 3,
        tick_seen: true,
        wall_ns: 4_000_000_000,
    }
}

fn manifest(directory: &str, publications: std::ops::Range<u64>) -> Manifest {
    Manifest {
        identity: WindowIdentity {
            run: reference().identity,
            window: publications.start,
        },
        publication_first: publications.start,
        publication_end: publications.end,
        events_first: 0,
        events_end: 0,
        wall_ns: None,
        continuous_ns: None,
        awake_ns: None,
        losses: Losses::default(),
        data_directory: directory.to_owned(),
        tables: vec![TableManifest {
            name: "cpus",
            rows: 4,
        }],
    }
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "recording-check-cursor-{}-{number}",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();

        Self(path)
    }

    fn write(&self, directory: &str, values: &[(u64, u32)]) {
        let directory = self.0.join(directory);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let mut frame = DataFrame::new(
            values.len(),
            vec![
                Column::new(
                    "publication".into(),
                    values.iter().map(|row| row.0).collect::<Vec<_>>(),
                ),
                Column::new(
                    "cpu".into(),
                    values.iter().map(|row| row.1).collect::<Vec<_>>(),
                ),
                Column::new("user_ticks".into(), vec![u32::MAX; values.len()]),
                Column::new("system_ticks".into(), vec![0_u32; values.len()]),
                Column::new("nice_ticks".into(), vec![0_u32; values.len()]),
                Column::new("idle_ticks".into(), vec![0_u32; values.len()]),
            ],
        )
        .unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("cpus.parquet"))
            .unwrap();
        let bytes = ParquetWriter::new(file).finish(&mut frame).unwrap();
        assert!(bytes > 0);
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
