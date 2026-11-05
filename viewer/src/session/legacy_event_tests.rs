use std::sync::Arc;

use collector::recording::private;
use persistence::engine::{AdmissionRefusal, AdmittedTable};
use persistence::resources::ByteBudget;
use polars::prelude::*;

use super::Session;
use crate::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::recording::{Refused, ScopedSealedMinute, SealedSource, processes};

const MEBIBYTE_BYTES: usize = 1024 * 1024;
const WIDE_BYTES: usize = collector::ring::PROGRAM_PATH_BYTES_MAX;
const LEGACY_REFUSAL_ROWS: usize = 6_144;

#[test]
fn legacy_event_routes_reject_disguised_identity_fields() {
    // Goal: legacy projection cannot erase a malformed reserved identity extension.
    // Method: remove the canonical group and query a mixed-case replacement through both callers.
    let (fixture, manifest) = fixture(1, 16);
    rewrite_events(&fixture, &manifest, |frame| {
        for (name, _) in persistence::schema::IDENTITY_FIELDS {
            frame.drop_in_place(name).unwrap();
        }
        frame
            .with_column(Column::new("Identity_Version".into(), [9_u8]))
            .unwrap();
    });
    check_routes_refused(&fixture, &manifest);
}

#[test]
fn legacy_event_routes_reject_disguised_observation_fields() {
    // Goal: malformed explicit provenance must not become synthesized associated provenance.
    // Method: query invalid uppercase provenance fields through SQL and the typed App loader.
    let (fixture, manifest) = fixture(1, 16);
    rewrite_events(&fixture, &manifest, |frame| {
        for (name, _) in persistence::event_observation::EVENT_OBSERVATION_FIELDS {
            frame
                .with_column(Column::new(name.to_ascii_uppercase().into(), [9_u8]))
                .unwrap();
        }
    });
    check_routes_refused(&fixture, &manifest);
}

#[test]
fn legacy_event_source_rejects_wrong_identity_type_before_query_planning() {
    // Goal: a caller's empty projection cannot skip reserved schema validation.
    // Method: admit a zero-row physical file whose canonical identity version has the wrong type.
    let (fixture, manifest) = fixture(0, 16);
    rewrite_events(&fixture, &manifest, |frame| {
        frame
            .with_column(Column::new("identity_version".into(), Vec::<i64>::new()))
            .unwrap();
    });
    check_source_refused(&fixture, &manifest);
}

#[test]
fn legacy_event_source_rejects_missing_base_fields_before_query_planning() {
    // Goal: an empty physical source cannot publish catalog fields it does not contain.
    // Method: remove required publication and PID fields separately before Session admission.
    for name in ["publication", "pid"] {
        let (fixture, manifest) = fixture(0, 16);
        rewrite_events(&fixture, &manifest, |frame| {
            frame.drop_in_place(name).unwrap();
        });

        check_source_refused(&fixture, &manifest);
    }
}

#[test]
fn legacy_event_source_rejects_wrong_publication_type_before_query_planning() {
    // Goal: LIMIT0 cannot make an invalid physical publication type into a valid source.
    // Method: replace UInt64 publication with UInt8 in a real empty legacy file.
    let (fixture, manifest) = fixture(0, 16);
    rewrite_events(&fixture, &manifest, |frame| {
        frame
            .with_column(Column::new("publication".into(), Vec::<u8>::new()))
            .unwrap();
    });

    check_source_refused(&fixture, &manifest);
}

fn check_source_refused(fixture: &Fixture, manifest: &persistence::Manifest) {
    let session = Session::open(&fixture.root).unwrap();
    let window = session
        .windows
        .iter()
        .position(|window| window.identity == manifest.identity)
        .unwrap();
    assert!(session.reader().source(window, "events").is_err());
}

#[test]
fn legacy_event_routes_accept_a_valid_empty_physical_table() {
    // Goal: strict physical validation retains legitimate empty legacy recordings.
    // Method: read a real zero-row Parquet table through both production callers and count.
    let (fixture, manifest) = fixture(0, 16);
    assert_eq!(typed_query(&fixture, &manifest).unwrap().height(), 0);
    assert_eq!(session_query(&fixture, &manifest).unwrap().height(), 0);
    assert_eq!(count_query(&fixture, &manifest).unwrap(), 0);
}

#[test]
fn legacy_event_routes_reject_corrupt_empty_physical_tables() {
    // Goal: a zero manifest count cannot bypass file admission.
    // Method: corrupt only the owned empty event file before opening SQL and typed readers.
    let (fixture, manifest) = fixture(0, 16);
    std::fs::write(events_path(&fixture, &manifest), b"invalid parquet").unwrap();
    check_routes_refused(&fixture, &manifest);
}

#[test]
fn legacy_event_routes_reject_rows_hidden_by_an_empty_manifest() {
    // Goal: logical emptiness cannot hide nonempty physical event data.
    // Method: replace an owned empty table with a valid one-row table before opening the readers.
    let (empty, empty_manifest) = fixture(0, 16);
    let (nonempty, nonempty_manifest) = fixture(1, 16);
    std::fs::copy(
        events_path(&nonempty, &nonempty_manifest),
        events_path(&empty, &empty_manifest),
    )
    .unwrap();
    check_routes_refused(&empty, &empty_manifest);
}

#[test]
fn legacy_event_count_rejects_corrupt_empty_physical_tables() {
    // Goal: the direct count route validates an empty legacy file independently of SQL.
    // Method: count an owned zero-row manifest after corrupting its physical file.
    let (fixture, manifest) = fixture(0, 16);
    std::fs::write(events_path(&fixture, &manifest), b"invalid parquet").unwrap();
    assert!(count_query(&fixture, &manifest).is_err());
}

#[test]
fn legacy_event_count_rejects_rows_hidden_by_an_empty_manifest() {
    // Goal: direct count compares physical metadata with a zero manifest count.
    // Method: copy one valid physical row into an owned zero-row recording before counting.
    let (empty, empty_manifest) = fixture(0, 16);
    let (nonempty, nonempty_manifest) = fixture(1, 16);
    std::fs::copy(
        events_path(&nonempty, &nonempty_manifest),
        events_path(&empty, &empty_manifest),
    )
    .unwrap();
    assert!(count_query(&empty, &empty_manifest).is_err());
}

fn check_routes_refused(fixture: &Fixture, manifest: &persistence::Manifest) {
    let typed = typed_query(fixture, manifest);
    let session = session_query(fixture, manifest);
    assert!(
        typed.is_err(),
        "typed reader accepted invalid physical events"
    );
    assert!(
        session.is_err(),
        "SQL reader accepted invalid physical events"
    );
}

fn typed_query(fixture: &Fixture, manifest: &persistence::Manifest) -> Result<DataFrame, Refused> {
    let marker = persistence::layout::WindowKey::from(&manifest.identity).marker(&fixture.root);
    let budget = Arc::new(ByteBudget::new(256 * MEBIBYTE_BYTES));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let minute = ScopedSealedMinute::open_processes_using(
        &SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        manifest,
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    )?;
    let rows = processes::read_processes_batch(&minute)?.rows;
    Ok(rows
        .lazy()
        .filter(col("app_origin").eq(lit(1_u8)))
        .collect()
        .map_err(persistence::Error::from)?)
}

fn session_query(
    fixture: &Fixture,
    manifest: &persistence::Manifest,
) -> persistence::Result<DataFrame> {
    let session = Session::open(&fixture.root)?;
    let window = session
        .windows
        .iter()
        .position(|window| window.identity == manifest.identity)
        .unwrap();
    let reader = session.reader();
    let source = reader.source(window, "events")?;
    session
        .connection
        .query(&format!("SELECT * FROM {source} ORDER BY event"))
}

fn count_query(
    fixture: &Fixture,
    manifest: &persistence::Manifest,
) -> Result<u64, persistence::engine::AdmittedReadRefusal> {
    let budget = Arc::new(ByteBudget::new(256 * MEBIBYTE_BYTES));
    persistence::event_table::EventTable::open_budgeted(&fixture.root, manifest, &budget)?
        .count_budgeted(None, &budget)
}

fn events_path(fixture: &Fixture, manifest: &persistence::Manifest) -> std::path::PathBuf {
    fixture
        .root
        .join(&manifest.data_directory)
        .join("events.parquet")
}

fn rewrite_events(
    fixture: &Fixture,
    manifest: &persistence::Manifest,
    change: impl FnOnce(&mut DataFrame),
) {
    let path = events_path(fixture, manifest);
    let mut frame = persistence::engine::scan(&path).unwrap().collect().unwrap();
    change(&mut frame);
    let output = private::open_private_file_writable(&path).unwrap();
    output.set_len(0).unwrap();
    ParquetWriter::new(output)
        .set_parallel(false)
        .finish(&mut frame)
        .unwrap();
}

#[test]
fn typed_legacy_events_reserve_expanded_groups_before_decode() {
    // Goal: the production App input path refuses expanded legacy pages before decoding.
    // Method: read unique full-width byte keys with a small shared allowance, then retry larger.
    let (fixture, manifest) = fixture(LEGACY_REFUSAL_ROWS, WIDE_BYTES);
    assert!(typed_read(&fixture, &manifest, 80 * MEBIBYTE_BYTES, true).is_none());
    let rows = typed_read(&fixture, &manifest, 256 * MEBIBYTE_BYTES, false).unwrap();
    check_rows(&rows, LEGACY_REFUSAL_ROWS, WIDE_BYTES, "app_event");
}

#[test]
fn session_legacy_events_reserve_expanded_groups_before_decode() {
    // Goal: saved SQL uses the same legacy decode admission as the typed App input path.
    // Method: query the Session reader under a small allowance, then verify every byte larger.
    let (fixture, manifest) = fixture(LEGACY_REFUSAL_ROWS, WIDE_BYTES);
    assert!(session_read(&fixture, &manifest, 80 * MEBIBYTE_BYTES, true).is_none());
    let rows = session_read(&fixture, &manifest, 256 * MEBIBYTE_BYTES, false).unwrap();
    check_rows(&rows, LEGACY_REFUSAL_ROWS, WIDE_BYTES, "event");
}

#[test]
fn typed_legacy_events_read_multiple_slices_in_event_order() {
    // Goal: a legacy file larger than one physical slice keeps its complete event order.
    // Method: save reversed event numbers across two slices and inspect the real App batch.
    let count = persistence::event_observation::BATCH_ROWS_MAX + 1;
    let (fixture, manifest) = fixture(count, 16);
    let rows = typed_read(&fixture, &manifest, 256 * MEBIBYTE_BYTES, false).unwrap();
    check_rows(&rows, count, 16, "app_event");
}

#[test]
#[ignore = "opt-in full-width legacy viewer admission at the production 256 MiB ceiling"]
fn full_width_legacy_viewer_routes_refuse_before_decode() {
    // Goal: neither production viewer route bypasses the full-width legacy admission bound.
    // Method: use a 16,384-row group at the normal ceiling and compare all bytes at 1 GiB.
    let count = persistence::event_observation::BATCH_ROWS_MAX;
    let (fixture, manifest) = fixture(count, WIDE_BYTES);
    assert!(typed_read(&fixture, &manifest, 256 * MEBIBYTE_BYTES, true).is_none());
    assert!(session_read(&fixture, &manifest, 256 * MEBIBYTE_BYTES, true).is_none());
    let rows = typed_read(&fixture, &manifest, 1024 * MEBIBYTE_BYTES, false).unwrap();
    check_rows(&rows, count, WIDE_BYTES, "app_event");
    drop(rows);
    let rows = session_read(&fixture, &manifest, 1024 * MEBIBYTE_BYTES, false).unwrap();
    check_rows(&rows, count, WIDE_BYTES, "event");
}

fn typed_read(
    fixture: &Fixture,
    manifest: &persistence::Manifest,
    bytes: usize,
    refused: bool,
) -> Option<DataFrame> {
    let marker = persistence::layout::WindowKey::from(&manifest.identity).marker(&fixture.root);
    let budget = Arc::new(ByteBudget::new(bytes));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let minute = ScopedSealedMinute::open_processes_using(
        &SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        manifest,
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    )
    .unwrap();
    let trace = persistence::work::ingestion::Trace::new(manifest.identity.run.recording);
    let scope = trace.enter();
    let result = processes::read_processes_batch(&minute);
    drop(scope);
    if refused {
        assert!(matches!(
            result,
            Err(Refused::Input(AdmissionRefusal::Budget))
        ));
        check_no_event_decode(&trace);
        assert_eq!(budget.used(), 0);
        None
    } else {
        let rows = result.unwrap().rows;
        Some(
            rows.lazy()
                .filter(col("app_origin").eq(lit(1_u8)))
                .collect()
                .unwrap(),
        )
    }
}

fn session_read(
    fixture: &Fixture,
    manifest: &persistence::Manifest,
    bytes: usize,
    refused: bool,
) -> Option<DataFrame> {
    let mut session = Session::open(&fixture.root).unwrap();
    session.event_query_budget = Arc::new(ByteBudget::new(bytes));
    let window = session
        .windows
        .iter()
        .position(|window| window.identity == manifest.identity)
        .unwrap();
    let trace = persistence::work::ingestion::Trace::new(manifest.identity.run.recording);
    let scope = trace.enter();
    let reader = session.reader();
    let observation = reader.source_scope(window, Some("events"));
    let result = reader.source(window, "events").and_then(|source| {
        session
            .connection
            .query(&format!("SELECT * FROM {source} ORDER BY event"))
    });
    drop(observation);
    drop(scope);
    if refused {
        let error = result.expect_err("legacy Session decode bypassed its budget");
        assert!(
            matches!(&error, persistence::Error::Bound("event table budget")),
            "{error}"
        );
        check_no_event_decode(&trace);
        None
    } else {
        Some(result.unwrap())
    }
}

fn check_no_event_decode(trace: &persistence::work::ingestion::Trace) {
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    assert!(!events.iter().any(|event| {
        event.table == Some("events")
            && matches!(
                event.kind,
                persistence::work::ingestion::Kind::NativeCollect
                    | persistence::work::ingestion::Kind::ProjectionDecode
            )
    }));
}

fn check_rows(rows: &DataFrame, count: usize, bytes: usize, event: &str) {
    assert_eq!(rows.height(), count);
    let events = rows.column(event).unwrap().u64().unwrap();
    let names = rows.column("executable_name").unwrap().binary().unwrap();
    let apps = rows.column("app_path").unwrap().binary().unwrap();
    for index in 0..count {
        let number = collector::u64_from_usize(index) + 1;
        assert_eq!(events.get(index), Some(number));
        assert_eq!(names.get(index), Some(key(number, bytes, false).as_slice()));
        assert_eq!(apps.get(index), Some(key(number, bytes, true).as_slice()));
    }
}

fn fixture(count: usize, bytes: usize) -> (Fixture, persistence::Manifest) {
    assert!(collector::u64_from_usize(count) <= persistence::schema::IDENTITY_ROWS_MAX);
    let mut fixture = Fixture::legacy_of(true, 2);
    fixture.publish(1000);
    fixture.seal_minute(0);
    let identity = persistence::WindowIdentity {
        run: fixture.writer.identity().unwrap(),
        window: 0,
    };
    let marker = persistence::layout::WindowKey::from(&identity).marker(&fixture.root);
    let mut manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let original = fixture.root.join(&manifest.data_directory);
    // A distinct committed run reuses valid base tables, without rewriting an existing seal.
    manifest.identity.run.run.0[0] ^= 1;
    let store = persistence::Store::open(&fixture.root).unwrap();
    let persistence::Begin::Pending(pending) = store.begin(&manifest.identity).unwrap() else {
        panic!("new test run")
    };
    for table in &manifest.tables {
        if table.name != "events" {
            let name = format!("{}.parquet", table.name);
            std::fs::copy(original.join(&name), pending.path().join(&name)).unwrap();
        }
    }
    let mut frame = fixture_frame(&original, count, bytes);
    let output = private::open_private_file(&pending.path().join("events.parquet"), true).unwrap();
    ParquetWriter::new(output)
        .with_row_group_size(Some(persistence::event_observation::BATCH_ROWS_MAX))
        .set_parallel(false)
        .finish(&mut frame)
        .unwrap();
    manifest.events_first = 1;
    manifest.events_end = collector::u64_from_usize(count) + 1;
    manifest
        .tables
        .iter_mut()
        .find(|table| table.name == "events")
        .unwrap()
        .rows = collector::u64_from_usize(count);
    manifest.data_directory.clear();
    store.commit(pending, manifest.clone()).unwrap();
    let marker = persistence::layout::WindowKey::from(&manifest.identity).marker(&fixture.root);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    (fixture, manifest)
}

fn fixture_frame(original: &std::path::Path, count: usize, bytes: usize) -> DataFrame {
    let mut frame = persistence::engine::scan(&original.join("events.parquet"))
        .unwrap()
        .collect()
        .unwrap();
    assert_eq!(frame.height(), 1);
    frame = frame
        .take(&IdxCa::from_vec("index".into(), vec![0; count]))
        .unwrap();
    let numbers: Vec<_> = (1..=collector::u64_from_usize(count)).rev().collect();
    frame
        .with_column(Column::new("event".into(), numbers.as_slice()))
        .unwrap();
    for (name, app) in [("executable_name", false), ("app_path", true)] {
        let values: Vec<_> = numbers
            .iter()
            .map(|number| key(*number, bytes, app))
            .collect();
        let values: Vec<_> = values.iter().map(Vec::as_slice).collect();
        frame
            .with_column(Column::new(name.into(), values.as_slice()))
            .unwrap();
    }
    for (name, _) in persistence::event_observation::EVENT_OBSERVATION_FIELDS {
        if frame.column(name).is_ok() {
            frame.drop_in_place(name).unwrap();
        }
    }
    frame
}

fn key(number: u64, bytes: usize, app: bool) -> Vec<u8> {
    assert!(bytes >= 16);
    assert!(bytes <= WIDE_BYTES);
    let mut key = vec![if app { 0xfe } else { 0xff }; bytes];
    key[1..9].copy_from_slice(format!("{number:08x}").as_bytes());
    if app {
        key[0] = b'/';
        key[bytes - 4..].copy_from_slice(b".app");
    }
    key
}
