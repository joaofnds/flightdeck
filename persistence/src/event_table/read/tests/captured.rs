use std::fs::File;

use super::*;
use crate::event_table::EventTable;

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
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let other = Arc::new(ByteBudget::new(256 * 1024 * 1024));
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
    let manifest = fixture.manifest();
    EventTable::open_budgeted(&fixture.0, &manifest, budget).unwrap()
}
