use std::fs::File;

use super::*;
use crate::event_table::EventTable;

#[test]
fn captured_slices_keep_original_order_after_the_factory_replaces_the_path() {
    // Goal: all bounded slices consume the same owned bytes after a private path replacement.
    // Method: replace the file after real capture, then read both sides of the batch boundary.
    let rows = BATCH_ROWS_MAX + 1;
    let (fixture, replacement) =
        captured_slices_keep_original_order_after_the_factory_replaces_the_path_fixture(rows);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table = open(&fixture, &budget);
    let part = table.part(0).unwrap();
    let path = part.path().to_owned();
    let trace = crate::work::ingestion::Trace::new(
        collector::recording::read_identity(&fixture.0).unwrap(),
    );
    let scope = trace.enter();

    let captured = part
        .capture_with(&budget, |file, budget| {
            let input = AdmittedTable::from_file_budgeted(file, budget)?;
            std::fs::rename(&replacement, &path).unwrap();
            Ok(input)
        })
        .unwrap();
    let first = captured
        .read_slice(0, u32::try_from(BATCH_ROWS_MAX).unwrap(), &budget)
        .unwrap();
    let last = captured
        .read_slice(collector::u64_from_usize(BATCH_ROWS_MAX), 1, &budget)
        .unwrap();

    assert_eq!(first.rows().height(), BATCH_ROWS_MAX);
    assert_eq!(
        first.rows().column("event").unwrap().u64().unwrap().get(0),
        Some(1)
    );
    assert_eq!(
        last.rows().column("event").unwrap().u64().unwrap().get(0),
        Some(collector::u64_from_usize(rows))
    );
    let names = last.rows().column("executable_name").unwrap();
    assert_eq!(names.binary().unwrap().get(0), Some(b"x".as_slice()));
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == crate::work::ingestion::Kind::ContentCapture)
            .count(),
        1
    );
    drop((first, last, captured, table));
    assert_eq!(budget.used(), 0);
    drop(scope);
}

fn captured_slices_keep_original_order_after_the_factory_replaces_the_path_fixture(
    rows: usize,
) -> (Fixture, PathBuf) {
    let mut stored = frame(rows);
    for (name, value) in [
        ("executable_name", b"x".as_slice()),
        ("app_path", b"/a.app"),
    ] {
        stored
            .with_column(Column::new(name.into(), vec![value; rows]))
            .unwrap();
    }
    let fixture = Fixture::with_frame(stored.clone(), None);
    let replacement = fixture.0.join("replacement.parquet");
    stored
        .with_column(Column::new(
            "executable_name".into(),
            vec![b"replacement".as_slice(); rows],
        ))
        .unwrap();
    crate::encoder::encode_write(
        &mut stored,
        crate::encoder::encode_open(&replacement).unwrap(),
    )
    .unwrap();
    (fixture, replacement)
}

#[test]
fn captured_part_refuses_a_factory_that_returns_another_files_bytes() {
    // Goal: a forwarding factory cannot substitute a different admitted source identity.
    // Method: return a real byte-identical copy with a distinct inode and observe refusal.
    let fixture = Fixture::with_rows(1);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table = open(&fixture, &budget);
    let part = table.part(0).unwrap();
    let copy = fixture.0.join("other.parquet");
    std::fs::copy(part.path(), &copy).unwrap();

    let result = part.capture_with(&budget, |file, budget| {
        drop(file);
        AdmittedTable::from_file_budgeted(File::open(&copy).unwrap(), budget)
    });

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Decode(Error::Bound(
            "changed event source input"
        )))
    ));
    drop(table);
    assert_eq!(budget.used(), 0);
}

#[test]
fn captured_part_refuses_an_input_charged_to_a_different_ledger() {
    // Goal: the typed caller's budget owns capture, profile, decode and output together.
    // Method: return the right file under another real ledger and require a typed refusal.
    let fixture = Fixture::with_rows(1);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let other = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table = open(&fixture, &budget);

    let result = table
        .part(0)
        .unwrap()
        .capture_with(&budget, |file, _budget| {
            AdmittedTable::from_file_budgeted(file, &other)
        });

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Decode(Error::Bound(
            "event input budget"
        )))
    ));
    assert_eq!(other.used(), 0);
    drop(table);
    assert_eq!(budget.used(), 0);
}

#[test]
fn captured_slice_cannot_move_its_native_work_to_another_ledger() {
    // Goal: one captured input cannot decode against a different caller's remaining capacity.
    // Method: retain a real input, request its slice under another budget, then retry correctly.
    let fixture = Fixture::with_rows(1);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let other = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table = open(&fixture, &budget);
    let input = table
        .part(0)
        .unwrap()
        .capture_with(&budget, AdmittedTable::from_file_budgeted)
        .unwrap();
    let held = budget.used();

    let refused = input.read_slice(0, 1, &other);

    assert!(matches!(
        refused,
        Err(AdmittedReadRefusal::Decode(Error::Bound(
            "event input budget"
        )))
    ));
    assert_eq!(other.used(), 0);
    assert_eq!(budget.used(), held);
    observe_rows(input.read_slice(0, 1, &budget).unwrap().rows(), 1);
    drop((input, table));
    assert_eq!(budget.used(), 0);
}

fn open(fixture: &Fixture, budget: &Arc<ByteBudget>) -> EventTable {
    let recording = collector::recording::read_identity(&fixture.0).unwrap();
    let marker = crate::layout::WindowKey::from(&identity(recording)).marker(&fixture.0);
    let manifest = crate::read_committed(&fixture.0, &marker).unwrap();
    EventTable::open_budgeted(&fixture.0, &manifest, budget).unwrap()
}
