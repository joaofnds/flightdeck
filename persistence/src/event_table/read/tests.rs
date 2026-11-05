use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use collector::ring::{Identity, ProcessCreation, RunIdentity};

use super::*;
use crate::canonical::ready_fixture::ReadyWindow;
use crate::event_recovery::batch::{self, Row};
use crate::owned_identity::OwnedIdentity;
use crate::window::EventRecord;
use crate::{Losses, Manifest, Store, TableManifest, WindowIdentity};

const BYTES: usize = collector::ring::PROGRAM_PATH_BYTES_MAX;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

mod captured;

struct Fixture(PathBuf);

#[test]
fn a_part_without_the_provenance_a_part_holds_is_refused() {
    // Goal: a part missing the columns every part holds never reaches a query as such a row.
    // Method: read a part whose provenance columns were left out, under the part's own budget.
    let mut input = frame(1);
    for (name, _) in crate::event_observation::EVENT_OBSERVATION_FIELDS {
        input.drop_in_place(name).unwrap();
    }
    let fixture = Fixture::with_frame(input, None);
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let manifest = fixture.manifest();
    let table =
        crate::event_table::EventTable::open_budgeted(&fixture.0, &manifest, &budget).unwrap();

    let refused = table.part(0).unwrap().read_slice(0, 1, &budget);

    assert!(matches!(
        refused,
        Err(AdmittedReadRefusal::Decode(Error::Bound(
            "event physical schema"
        )))
    ));
}

#[test]
fn direct_normalization_allocations_fit_the_reserved_fixed_schema_workspace() {
    // Goal: fixed-schema normalization owns no payload-sized copy or uncharged query plan.
    // Method: observe requested heap layouts at empty, tiny and maximum bounded row counts.
    for rows in [0, 1, BATCH_ROWS_MAX] {
        for identities in [false, true] {
            let mut input = frame(rows);
            if !identities {
                for (name, _) in crate::schema::IDENTITY_FIELDS {
                    input.drop_in_place(name).unwrap();
                }
            }
            let bytes = super::super::workspace::normalization(rows, 1).unwrap();
            let budget = Arc::new(ByteBudget::new(bytes));
            let reservation = budget.reserve(bytes).unwrap();

            let (normalized, allocations) = crate::allocation_tests::observe(&budget, || {
                normalize(input, identities, false).unwrap()
            });

            eprintln!(
                "normalization rows={rows} identities={identities} requested={} \
                reserved={bytes} allocations={}",
                allocations.allocated_bytes, allocations.allocations
            );
            assert!(allocations.allocated_bytes <= bytes);
            assert_eq!(allocations.allocation_charge_min, bytes);
            assert_eq!(normalized.width(), model::catalog::EVENTS.column_count());
            let ((), destruction) = crate::allocation_tests::observe(&budget, || drop(normalized));
            assert!(destruction.deallocations > 0);
            assert_eq!(destruction.deallocation_charge_min, bytes);
            drop(reservation);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn associated_filter_allocations_fit_the_reserved_numeric_and_view_workspace() {
    // Goal: association filtering copies views and scalars without copying program payload.
    // Method: measure a maximum batch's mixed association filter under its derived charge.
    let rows = BATCH_ROWS_MAX;
    let mut input = normalize(frame(rows), true, false).unwrap();
    input
        .with_column(Column::new(
            "publication".into(),
            (0..rows)
                .map(|index| (index % 2 == 0).then_some(1_u64))
                .collect::<Vec<_>>(),
        ))
        .unwrap();
    input
        .with_column(Column::new(
            "event_observation_availability".into(),
            (0..rows)
                .map(|index| u8::try_from(index % 2).unwrap())
                .collect::<Vec<_>>(),
        ))
        .unwrap();
    let bytes = super::super::workspace::association(rows).unwrap();
    let budget = Arc::new(ByteBudget::new(bytes));
    let reservation = budget.reserve(bytes).unwrap();

    let (filtered, allocations) = crate::allocation_tests::observe(&budget, || {
        crate::event_observation::associated_rows(&input).unwrap()
    });

    eprintln!(
        "associated rows={rows} requested={} reserved={bytes} allocations={}",
        allocations.allocated_bytes, allocations.allocations
    );
    assert_eq!(filtered.height(), rows / 2);
    assert!(allocations.allocated_bytes <= bytes);
    assert_eq!(allocations.allocation_charge_min, bytes);
    let ((), destruction) = crate::allocation_tests::observe(&budget, || drop(filtered));
    assert!(destruction.deallocations > 0);
    assert_eq!(destruction.deallocation_charge_min, bytes);
    drop(reservation);
    assert_eq!(budget.used(), 0);
}

#[test]
fn refused_legacy_transform_workspace_allocates_nothing() {
    // Goal: an unfunded transform never begins normalization or native value decoding.
    // Method: retain all but one caller byte after admission and trace the real workspace gate.
    let fixture = Fixture::with_rows(2);
    let manifest = fixture.manifest();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let source =
        crate::event_table::EventTable::open_budgeted(&fixture.0, &manifest, &budget).unwrap();
    let part = source.part(0).unwrap();
    let file = collector::recording::private::open_private_file(part.path(), false).unwrap();
    let table = AdmittedTable::from_file_budgeted_profiled(file, &budget).unwrap();
    let occupied = budget
        .reserve(budget.maximum() - budget.used() - 1)
        .unwrap();

    let (result, requests) =
        crate::allocation_tests::observe(&budget, || slice_workspace(&table, &part, 0, 2, &budget));

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
    ));
    assert_eq!(requests.allocations, 0);
    assert_eq!(requests.deallocations, 0);
    drop(occupied);
}

#[test]
fn associated_output_clones_keep_only_the_selected_program_storage_charged() {
    // Goal: the input/work guard cannot escape through a tiny retained association.
    // Method: retain one selected binary buffer after dropping input, output and projection.
    let budget = Arc::new(ByteBudget::new(4 * 1024 * 1024));
    let mut input = normalize(frame(8), true, false).unwrap();
    input
        .with_column(Column::new(
            "publication".into(),
            [Some(1_u64), None, None, None, None, None, None, None],
        ))
        .unwrap();
    input
        .with_column(Column::new(
            "event_observation_availability".into(),
            [0_u8, 1, 1, 1, 1, 1, 1, 1],
        ))
        .unwrap();
    let reservation = budget
        .reserve(super::super::workspace::association(8).unwrap())
        .unwrap();
    let rows = crate::engine::frame_lease::copy_event_frame(&input, &budget).unwrap();
    let frame = EventFrame { rows, reservation };

    let associated = frame.associated(&budget).unwrap();
    let projected = associated.select(["executable_name"]).unwrap();
    let retained = projected
        .column("executable_name")
        .unwrap()
        .binary()
        .unwrap()
        .downcast_iter()
        .next()
        .unwrap()
        .data_buffers()[0]
        .clone();
    drop(associated);
    drop(projected);

    assert_eq!(retained.len(), BYTES);
    assert_eq!(retained.as_slice(), vec![0xff; BYTES]);
    assert!(budget.used() < 64 * 1024);
    assert!(budget.used() >= BYTES);
    drop(retained);
    assert_eq!(budget.used(), 0);
}

fn observe_rows(frame: &DataFrame, rows: usize) {
    assert_eq!(frame.height(), rows);
    let name = vec![0xff; BYTES];
    let app = app();
    for row in 0..rows {
        assert_eq!(
            frame
                .column("executable_name")
                .unwrap()
                .binary()
                .unwrap()
                .get(row),
            Some(name.as_slice())
        );
        assert_eq!(
            frame.column("app_path").unwrap().binary().unwrap().get(row),
            Some(app.as_slice())
        );
    }
}

/// The manifest of the one window under `root`, as a reader lists it.
fn fixture_manifest(root: &Path) -> Manifest {
    let attempt = std::fs::read_dir(root.join("data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    crate::read_committed(root, &attempt.path().join("ready").join("metadata")).unwrap()
}

impl Fixture {
    /// The manifest of the fixture's window, as a reader lists it.
    fn manifest(&self) -> Manifest {
        fixture_manifest(&self.0)
    }

    fn with_rows(rows: usize) -> Self {
        Self::with_frame(frame(rows), None)
    }

    fn with_frame(mut frame: DataFrame, row_group: Option<usize>) -> Self {
        let rows = frame.height();
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "fd-legacy-decode-{}-{sequence}-{rows}",
            std::process::id()
        ));
        assert!(!root.exists());
        let fixture = Self(root);
        let store = Store::open(&fixture.0).unwrap();
        let identity = identity(store.recording());
        let ready = ReadyWindow::create(&fixture.0).unwrap();
        let events = ready.path().join("events");
        collector::recording::private::create_private_dir(&events).unwrap();
        // The real encoder uses plain pages: repeated values still carry every native byte.
        let file = crate::encoder::encode_open(&events.join(crate::event_recovery::batch_name(0)))
            .unwrap();
        if let Some(row_group) = row_group {
            ParquetWriter::new(file)
                .with_row_group_size(Some(row_group))
                .finish(&mut frame)
                .unwrap();
        } else {
            crate::encoder::encode_write(&mut frame, file).unwrap();
        }
        let count = collector::u64_from_usize(rows);
        crate::event_table::index::Index {
            identity,
            first: 1,
            end: count + 1,
            rows: count,
            identities: true,
            batches: vec![crate::event_recovery::Batch {
                first: 1,
                end: count + 1,
                rows: count,
            }],
        }
        .write(&events.join("index"))
        .unwrap();
        let tables = Self::with_frame_tables(ready.path(), count);
        drop(
            ready
                .prove(Manifest {
                    identity,
                    publication_first: 1,
                    publication_end: 2,
                    events_first: 1,
                    events_end: count + 1,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                    losses: Losses::default(),
                    data_directory: String::new(),
                    tables,
                    event_batches: Some(1),
                })
                .unwrap(),
        );
        drop(store);
        fixture
    }

    fn with_frame_tables(ready: &Path, events: u64) -> Vec<TableManifest> {
        crate::schema::TABLES
            .iter()
            .map(|table| {
                if table.name() != "events" {
                    collector::recording::private::open_private_file(
                        &ready.join(format!("{}.parquet", table.name())),
                        true,
                    )
                    .unwrap();
                }
                TableManifest {
                    name: table.name(),
                    rows: match table.name() {
                        "ticks" => 1,
                        "events" => events,
                        _ => 0,
                    },
                }
            })
            .collect()
    }
}

fn identity(recording: Identity) -> WindowIdentity {
    WindowIdentity {
        run: RunIdentity {
            recording,
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        },
        window: 0,
    }
}

fn frame(rows: usize) -> DataFrame {
    let row = Row {
        publication: Some(1),
        event: EventRecord {
            number: 1,
            tick: 1,
            pid: 41,
            kind: crate::schema::event_kind::EXEC,
            coalition: None,
            usage: None,
            availability: crate::schema::availability::NOT_APPLICABLE,
            noticed: None,
        },
        identity: Some(OwnedIdentity {
            pid: 41,
            creation: Some(ProcessCreation {
                unique_id: u64::MAX,
                pid_version: i32::MIN,
            }),
            availability: 0,
            usage: crate::schema::availability::NOT_APPLICABLE,
            program: Some(0),
        }),
        name: Some(vec![0xff; BYTES]),
        app: Some(app()),
    };
    let mut frame = batch::frame(&[row], true)
        .unwrap()
        .take(&IdxCa::from_vec("index".into(), vec![0; rows]))
        .unwrap();
    frame
        .with_column(Column::new(
            "event".into(),
            (1..=collector::u64_from_usize(rows)).collect::<Vec<_>>(),
        ))
        .unwrap();
    frame
}

fn app() -> Vec<u8> {
    let mut app = vec![0xfe; BYTES];
    app[0] = b'/';
    app[BYTES - 4..].copy_from_slice(b".app");
    app
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
