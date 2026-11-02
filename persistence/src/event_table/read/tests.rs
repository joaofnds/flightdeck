use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use collector::ring::{Identity, ProcessCreation, RunIdentity};

use super::*;
use crate::event_recovery::batch::{self, Row};
use crate::owned_identity::OwnedIdentity;
use crate::window::EventRecord;
use crate::{Begin, Losses, Manifest, Store, TableManifest, WindowIdentity};

const ROWS: usize = 16_384;
const BYTES: usize = collector::ring::PROGRAM_PATH_BYTES_MAX;
const ENVIRONMENT: &str = "FLIGHTDECK_P01_LEGACY_READ_ROOT";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

mod captured;
mod schema;

struct Fixture(PathBuf);
struct Probe(Child);

#[test]
fn legacy_profile_bounds_the_actual_multiple_group_chunk_layout() {
    // Goal: the scratch certificate's chunk multiplier matches the pinned native decoder.
    // Method: decode16real two-row groups with rechunk disabled, then trace normalization.
    let fixture = Fixture::with_frame(frame(32), Some(2));
    let recording = collector::recording::read_identity(&fixture.0).unwrap();
    let marker = crate::layout::WindowKey::from(&identity(recording)).marker(&fixture.0);
    let manifest = crate::read_committed(&fixture.0, &marker).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let source =
        crate::event_table::EventTable::open_budgeted(&fixture.0, &manifest, &budget).unwrap();
    let part = source.part(0).unwrap();
    let file = collector::recording::private::open_private_file(part.path(), false).unwrap();
    let table = AdmittedTable::from_file_budgeted_profiled(file, &budget).unwrap();
    let (_, chunks) = table.slice_work_shape(0, 32).unwrap();
    assert_eq!(chunks, 16);
    let bytes = super::super::workspace::normalization(32, chunks).unwrap();
    let scratch = budget.reserve(bytes).unwrap();
    let native = table
        .read_slice_metadata_budgeted(&slice_columns(true, false), 0, 32)
        .unwrap();
    for column in native.columns() {
        assert_eq!(column.as_materialized_series().n_chunks(), chunks);
    }

    let (normalized, requests) =
        crate::allocation_tests::observe(&budget, || normalize(native, true, false).unwrap());

    assert_eq!(normalized.height(), 32);
    assert!(requests.allocated_bytes <= bytes);
    assert!(requests.allocation_charge_min >= bytes);
    eprintln!(
        "multi_group chunks={chunks} requested={} reserved={bytes}",
        requests.allocated_bytes
    );
    drop(normalized);
    drop(scratch);
}

#[test]
fn legacy_query_preflight_captures_one_input_for_multiple_slices() {
    // Goal: preflight does not parse a whole legacy file once for every future slice.
    // Method: admit a short-name source larger than one slice and count actual captures.
    let rows = BATCH_ROWS_MAX + 1;
    let mut frame = frame(rows);
    frame
        .with_column(Column::new(
            "executable_name".into(),
            vec![b"x".as_slice(); rows],
        ))
        .unwrap();
    frame
        .with_column(Column::new(
            "app_path".into(),
            vec![b"/a.app".as_slice(); rows],
        ))
        .unwrap();
    let fixture = Fixture::with_frame(frame, None);
    let recording = collector::recording::read_identity(&fixture.0).unwrap();
    let marker = crate::layout::WindowKey::from(&identity(recording)).marker(&fixture.0);
    let manifest = crate::read_committed(&fixture.0, &marker).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        crate::event_table::EventTable::open_budgeted(&fixture.0, &manifest, &budget).unwrap();
    let trace = crate::work::ingestion::Trace::new(recording);
    let scope = trace.enter();

    let plan = table.into_lazy(&budget).unwrap();

    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == crate::work::ingestion::Kind::ContentCapture)
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == crate::work::ingestion::Kind::NativeCollect)
    );
    drop(scope);
    let counted = plan.select([len().alias("count")]).collect().unwrap();
    assert_eq!(
        counted.column("count").unwrap().u32().unwrap().get(0),
        Some(u32::try_from(rows).unwrap())
    );
    drop(counted);
    assert_eq!(budget.used(), 0);
}

#[test]
fn reads_one_full_width_legacy_row_within_the_existing_small_caller_budget() {
    // Goal: an existing64MiB caller can read a tiny legacy seal without a fixed64MiB floor.
    // Method: commit a real full-width native identity and verify the budgeted result bytes.
    let fixture = Fixture::with_rows(1);
    let recording = collector::recording::read_identity(&fixture.0).unwrap();
    let marker = crate::layout::WindowKey::from(&identity(recording)).marker(&fixture.0);
    let manifest = crate::read_committed(&fixture.0, &marker).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        crate::event_table::EventTable::open_budgeted(&fixture.0, &manifest, &budget).unwrap();
    let part = table.part(0).unwrap();

    let result = part
        .read_slice(0, 1, &budget)
        .unwrap()
        .associated(&budget)
        .unwrap();

    observe_rows(&result, 1);
    eprintln!(
        "legacy_one_row ledger_peak={} retained={}",
        budget.used_peak(),
        budget.used()
    );
    drop(result);
    drop(part);
    drop(table);
    assert_eq!(budget.used(), 0);
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
    let recording = collector::recording::read_identity(&fixture.0).unwrap();
    let marker = crate::layout::WindowKey::from(&identity(recording)).marker(&fixture.0);
    let manifest = crate::read_committed(&fixture.0, &marker).unwrap();
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

#[test]
fn legacy_query_refuses_intrinsically_wide_groups_before_native_collection() {
    // Goal: a legacy query cannot enter native collection with an unfundable decode profile.
    // Method: preflight6,144full-width rows under80MiBthrough the production lazy entry point.
    let fixture = Fixture::with_rows(6144);
    let recording = collector::recording::read_identity(&fixture.0).unwrap();
    let marker = crate::layout::WindowKey::from(&identity(recording)).marker(&fixture.0);
    let manifest = crate::read_committed(&fixture.0, &marker).unwrap();
    let budget = Arc::new(ByteBudget::new(80 * 1024 * 1024));
    let table =
        crate::event_table::EventTable::open_budgeted(&fixture.0, &manifest, &budget).unwrap();
    let trace = crate::work::ingestion::Trace::new(recording);
    let scope = trace.enter();

    let result = table.into_lazy(&budget);

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
    ));
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        crate::work::ingestion::Kind::NativeCollect
            | crate::work::ingestion::Kind::ProjectionDecode
    )));
    drop(scope);
    drop(result);
    assert_eq!(budget.used(), 0);
}

#[test]
#[ignore = "opt-in native legacy full-width decode accounting observation"]
fn observes_legacy_full_width_decode() {
    // Goal: distinguish the legacy decode envelope from the new8MiBphysical-part contract.
    // Method: decode16,384full-width plain encoded rows in a fresh child under its real ledger.
    observe_native(
        ROWS,
        "event_table::read::tests::observes_legacy_full_width_decode",
    );
}

#[test]
#[ignore = "opt-in native one-row legacy decode accounting observation"]
fn observes_one_legacy_row_with_the_profiled_workspace() {
    // Goal: the real tiny legacy native decode fits the existing64MiB caller.
    // Method: decode one full-width row with the profiled production workspace in a child.
    observe_native(
        1,
        "event_table::read::tests::observes_one_legacy_row_with_the_profiled_workspace",
    );
}

fn observe_native(rows: usize, test: &str) {
    if let Some(root) = std::env::var_os(ENVIRONMENT) {
        observe(&PathBuf::from(root), rows);
        return;
    }
    let fixture = Fixture::with_rows(rows);
    let log_path = fixture.0.join("native-read.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut probe = Probe(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                test,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ENVIRONMENT, &fixture.0)
            .env("POLARS_MAX_THREADS", "1")
            .env("RAYON_NUM_THREADS", "1")
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let status = probe.wait();
    let mut output = String::new();
    std::fs::File::open(log_path)
        .unwrap()
        .take(1024 * 1024 + 1)
        .read_to_string(&mut output)
        .unwrap();
    assert!(output.len() <= 1024 * 1024);
    eprintln!("{output}");
    assert!(status.success());
}

fn observe(root: &Path, rows: usize) {
    let recording = collector::recording::read_identity(root).unwrap();
    let identity = identity(recording);
    let marker = crate::layout::WindowKey::from(&identity).marker(root);
    let manifest = crate::read_committed(root, &marker).unwrap();
    let before = crate::resources::Footprint::read().unwrap();
    if rows > 1 {
        observe_refusal(root, &manifest, rows);
    }
    let maximum = if rows == 1 {
        64 * 1024 * 1024
    } else {
        1024 * 1024 * 1024
    };
    let budget = Arc::new(ByteBudget::new(maximum));
    let table = crate::event_table::EventTable::open_budgeted(root, &manifest, &budget).unwrap();
    let part = table.part(0).unwrap();
    let result = part.read_slice(0, u32::try_from(rows).unwrap(), &budget);
    let after = crate::resources::Footprint::read().unwrap();
    eprintln!(
        "legacy_native budget={maximum} rows={rows} payload_bytes={} before_current={} \
        before_peak={} \
        after_current={} after_peak={} ledger_peak={} result={}",
        rows * BYTES * 2,
        before.current_bytes,
        before.lifetime_bytes_max,
        after.current_bytes,
        after.lifetime_bytes_max,
        budget.used_peak(),
        if result.is_ok() { "decoded" } else { "refused" }
    );
    let frame = result.unwrap();
    observe_rows(frame.rows(), rows);
    drop(frame);
    drop(part);
    drop(table);
    assert_eq!(budget.used(), 0);
}

fn observe_refusal(root: &Path, manifest: &Manifest, rows: usize) {
    let maximum = 256 * 1024 * 1024;
    let budget = Arc::new(ByteBudget::new(maximum));
    let table = crate::event_table::EventTable::open_budgeted(root, manifest, &budget).unwrap();
    let part = table.part(0).unwrap();
    let work = crate::work::observe();
    let trace = crate::work::ingestion::Trace::new(manifest.identity.run.recording);
    let scope = trace.enter();
    let refused = part.read_slice(0, u32::try_from(rows).unwrap(), &budget);
    assert!(matches!(
        refused,
        Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
    ));
    assert_eq!(work.counts().collections, 0);
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        crate::work::ingestion::Kind::NativeCollect
            | crate::work::ingestion::Kind::ProjectionDecode
    )));
    eprintln!(
        "legacy_native budget={maximum} result=Budget native_collects=0 projection_decodes=0"
    );
    drop(scope);
    drop(trace);
    drop(work);
    drop(part);
    drop(table);
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

impl Fixture {
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
        let Begin::Pending(pending) = store.begin(&identity).unwrap() else {
            panic!("new fixture")
        };
        // The real encoder uses plain pages: repeated values still carry every native byte.
        let file = crate::encoder::encode_open(&pending.path().join("events.parquet")).unwrap();
        if let Some(row_group) = row_group {
            ParquetWriter::new(file)
                .with_row_group_size(Some(row_group))
                .finish(&mut frame)
                .unwrap();
        } else {
            crate::encoder::encode_write(&mut frame, file).unwrap();
        }
        let tables = crate::schema::TABLES
            .iter()
            .map(|table| {
                if table.name() != "events" {
                    collector::recording::private::open_private_file(
                        &pending.path().join(format!("{}.parquet", table.name())),
                        true,
                    )
                    .unwrap();
                }
                TableManifest {
                    name: table.name(),
                    rows: if table.name() == "events" {
                        collector::u64_from_usize(rows)
                    } else {
                        0
                    },
                }
            })
            .collect();
        store
            .commit(
                pending,
                Manifest {
                    identity,
                    publication_first: 1,
                    publication_end: 2,
                    events_first: 1,
                    events_end: collector::u64_from_usize(rows) + 1,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                    losses: Losses::default(),
                    data_directory: String::new(),
                    tables,
                    event_batches: None,
                },
            )
            .unwrap();
        fixture
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
    for (name, _) in crate::event_observation::EVENT_OBSERVATION_FIELDS {
        frame.drop_in_place(name).unwrap();
    }
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

impl Probe {
    fn wait(&mut self) -> ExitStatus {
        for _step in 0..6_000 {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("owned native legacy read exceeded60second observation deadline");
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_some() {
            return;
        }
        self.0.kill().unwrap();
        for _step in 0..1000 {
            if self.0.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("owned native legacy read did not exit afterSIGKILL");
    }
}
