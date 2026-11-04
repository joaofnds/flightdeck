//! Physical catalog order is independent of each publication's row subsequence.
use super::*;

#[test]
fn ordinary_file_order_numeric_control_recovers() {
    // Goal: exact source rows in catalog order remain recoverable across row groups.
    // Method: repack two ticks into two CPU row groups and compare the published raw files.
    file_order_valid(false);
}

#[test]
fn ordinary_file_order_captured_control_recovers() {
    // Goal: captured ordinary recovery retains an admissible physical file order.
    // Method: repack only CPU row groups and require raw preservation and final frontiers.
    file_order_valid(true);
}

#[test]
fn ordinary_file_order_numeric_within_group_refuses() {
    // Goal: each publication's exact subsequence cannot excuse reversed catalog keys.
    // Method: swap the two CPU-0 publication rows within one admitted row group.
    file_order_refuses(false, 0);
}

#[test]
fn ordinary_file_order_captured_within_group_refuses() {
    // Goal: captured completeness proof retains global physical catalog order.
    // Method: reverse CPU-0 publications without altering either publication's subsequence.
    file_order_refuses(true, 0);
}

#[test]
fn ordinary_file_order_numeric_across_groups_refuses() {
    // Goal: the preceding key survives a physical row-group and comparison boundary.
    // Method: swap the middle CPU rows across two two-row groups, preserving subsequences.
    file_order_refuses(false, 1);
}

#[test]
fn ordinary_file_order_captured_across_groups_refuses() {
    // Goal: captured recovery detects catalog-order reversal between successive groups.
    // Method: move the middle CPU rows across the group boundary without changing values.
    file_order_refuses(true, 1);
}

fn file_order_stage(captured: bool) -> (Fixture, PathBuf, Arc<crate::resources::ByteBudget>) {
    let mut fixture = metric_fixture(captured, 2);
    metric_publish(&mut fixture, 0, true, true);
    metric_publish(&mut fixture, 1, false, true);
    fixture.writer.finish();
    eprintln!(
        "ORDINARY-ORDER root={} source={}",
        fixture.base.display(),
        fixture.writer.name()
    );
    let budget = registration_budget();
    let object;
    {
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(&complete_input(&fixture), &mut fixture.reader(), &budget)
            .unwrap();
        object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        complete_payload(&attempt, &fixture, 2);
    }
    (fixture, object, budget)
}

fn file_order_repack(path: &Path, swap: Option<usize>) {
    let budget = registration_budget();
    let table =
        crate::engine::AdmittedTable::from_file_budgeted(File::open(path).unwrap(), &budget)
            .unwrap();
    let names: Vec<_> = table.fields().map(|(name, _)| name).collect();
    let mut frame = table.read_metadata_budgeted(&names, None).unwrap();
    assert_eq!(frame.height(), 4);
    if let Some(position) = swap {
        let mut positions = [0, 1, 2, 3];
        positions.swap(position, position + 1);
        frame = frame
            .take(&IdxCa::from_vec("".into(), positions.to_vec()))
            .unwrap();
    }
    drop(table);
    ParquetWriter::new(File::create(path).unwrap())
        .with_row_group_size(Some(2))
        .finish(&mut frame)
        .unwrap();
}

fn file_order_valid(captured: bool) {
    let (fixture, object, budget) = file_order_stage(captured);
    file_order_repack(&object.join("pending/cpus.parquet"), None);
    let raw = raw_bytes(&object.join("pending"));
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    finish_recovery(&mut owner);
    drop(owner);
    assert_eq!(raw_bytes(&object.join("ready")), raw);
    let frontier = store
        .source_frontier(
            &fixture.run,
            &budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 4);
    assert_eq!(frontier.event_end(), 3);
}

fn file_order_refuses(captured: bool, position: usize) {
    let (fixture, object, budget) = file_order_stage(captured);
    let pending = object.join("pending");
    file_order_repack(&pending.join("cpus.parquet"), Some(position));
    let raw = raw_bytes(&pending);
    let occupied = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    assert_eq!(failure(&mut owner).kind(), io::ErrorKind::InvalidData);
    drop(owner);
    assert_eq!(fixture.slot(), occupied);
    assert_eq!(raw_bytes(&pending), raw);
    assert!(!pending.join("metadata").exists());
}
