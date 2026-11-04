//! Shared pressure must preserve exact values when only individual columns fit.
use super::*;

const EXPECTED_PROCESS_BYTES: usize =
    64 * (2 * collector::ring::PROGRAM_PATH_BYTES_MAX + 1024) + 8 * 1024 * 1024;

#[test]
fn ordinary_column_pressure_numeric_recovers() {
    // Goal: numeric Store recovery converges when a full process comparison cannot fit.
    // Method: retain external budget pressure after the expected process chunk is allocated.
    column_pressure_valid(false);
}

#[test]
fn ordinary_column_pressure_captured_recovers() {
    // Goal: captured values and native bytes survive column comparison under shared pressure.
    // Method: constrain the allowance below full decode work and compare every original byte.
    column_pressure_valid(true);
}

#[test]
fn ordinary_column_pressure_numeric_same_count_corruption_refuses() {
    // Goal: column comparison refuses changed values despite unchanged keys and cardinality.
    // Method: change process thread counts and recover under independently calculated pressure.
    column_pressure_refuses(false);
}

#[test]
fn ordinary_column_pressure_captured_same_count_corruption_refuses() {
    // Goal: captured column comparison cannot grant authority to a same-count value forgery.
    // Method: preserve all process keys and identities while changing a non-key numeric value.
    column_pressure_refuses(true);
}

fn column_pressure_stage(captured: bool) -> (Fixture, PathBuf, Arc<crate::resources::ByteBudget>) {
    let mut fixture = metric_fixture_with_processes(captured, 1, 512);
    metric_publish(&mut fixture, 0, true, true);
    fixture.writer.finish();
    eprintln!(
        "ORDINARY-COLUMN root={} source={}",
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
        complete_payload(&attempt, &fixture, 1);
    }
    (fixture, object, budget)
}

fn column_pressure_allowance(path: &Path, budget: &Arc<crate::resources::ByteBudget>) -> usize {
    let table =
        crate::engine::AdmittedTable::from_file_streaming(File::open(path).unwrap(), budget)
            .unwrap();
    let count = table.bounded_slice_count(0, 64).unwrap();
    let full = table.slice_work_shape(0, count).unwrap().0;
    let metadata = table.reserve_native_metadata().unwrap().bytes();
    let mut largest_column = 0;
    for (name, _) in table.fields() {
        let reservation = table.reserve_column_work(0, count, name).unwrap();
        largest_column = largest_column.max(reservation.bytes());
    }
    let allowance = metadata + largest_column + 8192;
    eprintln!(
        "ORDINARY-COLUMN full={full} metadata={metadata} largest_column={largest_column} \
         allowance={allowance}"
    );
    assert!(allowance < full + metadata);
    allowance
}

fn column_pressure_apply(
    owner: &mut OrdinaryRecovery<'_>,
    budget: &Arc<crate::resources::ByteBudget>,
    allowance: usize,
) -> crate::resources::ByteReservation {
    for _ in 0..20_000 {
        let before = budget.used();
        assert_eq!(owner.step().unwrap(), PublicationStep::Continue);
        if budget.used().checked_sub(before) == Some(EXPECTED_PROCESS_BYTES) {
            return budget
                .reserve(budget.maximum() - budget.used() - allowance)
                .unwrap();
        }
    }
    panic!("expected comparison chunk was not reached within the fixture bound");
}

fn column_pressure_valid(captured: bool) {
    let (fixture, object, budget) = column_pressure_stage(captured);
    let pending = object.join("pending");
    let allowance = column_pressure_allowance(&pending.join("processes.parquet"), &budget);
    let raw = raw_bytes(&pending);
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    let pressure = column_pressure_apply(&mut owner, &budget, allowance);
    let before = owner.work();
    column_pressure_finish(&mut owner, &budget).unwrap();
    assert!(owner.work().decode_admission_bytes > before.decode_admission_bytes);
    assert!(owner.work().proof_operations > before.proof_operations);
    drop(pressure);
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
    assert_eq!(frontier.publication_end(), 3);
    assert_eq!(frontier.event_end(), 3);
}

fn column_pressure_finish(
    owner: &mut OrdinaryRecovery<'_>,
    budget: &Arc<crate::resources::ByteBudget>,
) -> io::Result<()> {
    for _ in 0..20_000 {
        let before = budget.used();
        assert_eq!(owner.step()?, PublicationStep::Continue);
        if before
            .checked_sub(budget.used())
            .is_some_and(|released| released >= EXPECTED_PROCESS_BYTES)
        {
            return Ok(());
        }
    }
    panic!("process column proof did not release its expected chunk within the fixture bound");
}

fn column_pressure_refuses(captured: bool) {
    let (fixture, object, budget) = column_pressure_stage(captured);
    let pending = object.join("pending");
    rewrite(&pending.join("processes.parquet"), |frame| {
        frame
            .with_column(Column::new("threads".into(), vec![999i32; frame.height()]))
            .unwrap();
    });
    let allowance = column_pressure_allowance(&pending.join("processes.parquet"), &budget);
    let raw = raw_bytes(&pending);
    let occupied = fixture.slot();
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    let pressure = column_pressure_apply(&mut owner, &budget, allowance);
    let result = column_pressure_finish(&mut owner, &budget);
    drop(pressure);
    let Err(error) = result else {
        finish_recovery(&mut owner);
        panic!("same-count forged process values published");
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    drop(owner);
    assert_eq!(fixture.slot(), occupied);
    assert_eq!(raw_bytes(&pending), raw);
    assert!(!pending.join("metadata").exists());
}
