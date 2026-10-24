mod certificate;

use std::io::{self, Cursor, Read};
use std::sync::Arc;

use polars::prelude::{Column, DataFrame};
use polars_parquet_format::thrift::protocol::{
    TCompactInputProtocol, TCompactOutputProtocol, TFieldIdentifier, TListIdentifier,
    TOutputProtocol, TType,
};

use super::{AdmissionRefusal, AdmittedFile, capture_budgeted, open_file};
use crate::allocation_tests as allocation;
use crate::engine::source_tests::{Fixture, write_frame};
use crate::resources::ByteBudget;

#[test]
fn retains_only_compressed_charge_until_the_final_input_owner_drops() {
    // Goal: independent projections share charged immutable input after its source is gone.
    // Method: retain two Arc readers, remove the path, project both columns, then drop owners.
    let fixture = Fixture::new();
    std::fs::remove_file(&fixture.path).unwrap();
    let counter = Column::new("counter".into(), [Some(u64::MAX), None]);
    let other = Column::new("other".into(), [Some(-7i64), Some(0)]);
    write_frame(
        &fixture.path,
        &mut DataFrame::new(2, vec![counter.clone(), other.clone()]).unwrap(),
    );
    let size = usize::try_from(std::fs::metadata(&fixture.path).unwrap().len()).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let (opened, admission_allocations) = allocation::observe(&budget, || {
        AdmittedFile::open_budgeted(&fixture.path, 1024 * 1024, &budget)
    });
    assert_eq!(budget.used(), size);
    assert_eq!(
        admission_allocations.allocated_bytes - admission_allocations.deallocated_bytes,
        size
    );
    assert_eq!(
        admission_allocations.allocation_charge_min,
        size + super::admission::cursor_scratch_bytes().unwrap()
    );
    let admitted = Arc::new(opened.unwrap());
    let retained = Arc::clone(&admitted);
    std::fs::remove_file(&fixture.path).unwrap();
    let first = admitted.read(&["counter"]).unwrap();
    drop(admitted);
    let second = retained.read(&["other"]).unwrap();

    assert_eq!(first, DataFrame::new(2, vec![counter]).unwrap());
    assert_eq!(second, DataFrame::new(2, vec![other]).unwrap());
    assert_eq!(budget.used(), size);
    let final_owner = Arc::try_unwrap(retained).unwrap_or_else(|_| panic!("input still shared"));
    let ((), observation) = allocation::observe(&budget, || drop(final_owner));
    assert_eq!(budget.used(), 0);
    assert_eq!(observation.deallocated_bytes, size);
    assert_eq!(observation.deallocation_charge_min, size);
    assert_eq!(first.height(), 2);
    assert_eq!(second.height(), 2);
}

#[test]
fn destroys_malformed_parser_errors_and_input_before_releasing_their_charges() {
    // Goal: a boxed parser error cannot escape the scratch lease as an inline refusal.
    // Method: truncate a footer varint, observing every allocation and free with live charge.
    let fixture = Fixture::new();
    let bytes = malformed_footer(&fixture.path);
    std::fs::write(&fixture.path, &bytes).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let retained = budget.reserve(1024).unwrap();
    let required =
        retained.bytes() + bytes.len() + super::admission::cursor_scratch_bytes().unwrap();

    let (result, observation) = allocation::observe(&budget, || {
        AdmittedFile::open_budgeted(&fixture.path, 1024 * 1024, &budget)
    });

    assert!(matches!(
        result,
        Err(AdmissionRefusal::Io {
            kind: io::ErrorKind::InvalidData,
            raw_os_error: None,
        })
    ));
    assert!(observation.allocations > 3);
    assert_eq!(observation.allocated_bytes, observation.deallocated_bytes);
    assert_eq!(observation.allocation_charge_min, required);
    assert_eq!(observation.deallocation_charge_min, required);
    assert_eq!(budget.used(), retained.bytes());
}

fn malformed_footer(path: &std::path::Path) -> Vec<u8> {
    let original = std::fs::read(path).unwrap();
    let end = original.len() - 8;
    let length = usize::try_from(u32::from_le_bytes(
        original[end..end + 4].try_into().unwrap(),
    ))
    .unwrap();
    let mut input =
        TCompactInputProtocol::new(Cursor::new(&original[end - length..end]), 4 * 1024 * 1024);
    let metadata =
        polars_parquet::parquet::metadata::ThriftFileMetadata::read_from_in_protocol(&mut input)
            .unwrap();
    let mut footer = Vec::new();
    let mut output = TCompactOutputProtocol::new(&mut footer);
    output
        .write_field_begin(&TFieldIdentifier::new("version", TType::I32, 1))
        .unwrap();
    output.write_i32(1).unwrap();
    output
        .write_field_begin(&TFieldIdentifier::new("schema", TType::List, 2))
        .unwrap();
    output
        .write_list_begin(&TListIdentifier::new(
            TType::Struct,
            u32::try_from(metadata.schema.len()).unwrap(),
        ))
        .unwrap();
    for field in &metadata.schema {
        field.write_to_out_protocol(&mut output).unwrap();
    }
    output.write_list_end().unwrap();
    output
        .write_field_begin(&TFieldIdentifier::new("num_rows", TType::I64, 3))
        .unwrap();
    footer.push(0x80);
    let mut bytes = b"PAR1".to_vec();
    bytes.extend_from_slice(&footer);
    bytes.extend_from_slice(&u32::try_from(footer.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(b"PAR1");

    bytes
}

#[test]
fn preserves_static_bound_context_without_retaining_failed_input() {
    // Goal: budgeted admission preserves bound classification and frees captured input.
    // Method: capture a framed file declaring a footer beyond the shared admission limit.
    let fixture = Fixture::new();
    std::fs::write(&fixture.path, b"PAR1\xff\xff\xff\xffPAR1").unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let result = AdmittedFile::open_budgeted(&fixture.path, 1024 * 1024, &budget);

    assert!(matches!(
        result,
        Err(AdmissionRefusal::Bound("parquet footer bytes"))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn preserves_open_failure_io_kind_and_raw_status() {
    // Goal: inline I/O refusals preserve OS classification without retaining a rich error.
    // Method: remove a real fixture and compare the failure with the OS's own open error.
    let fixture = Fixture::new();
    std::fs::remove_file(&fixture.path).unwrap();
    let expected = std::fs::File::open(&fixture.path).unwrap_err();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let result = AdmittedFile::open_budgeted(&fixture.path, 1024 * 1024, &budget);

    assert!(
        matches!(result, Err(AdmissionRefusal::Io { kind, raw_os_error })
        if kind == expected.kind() && raw_os_error == expected.raw_os_error())
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn refuses_exhausted_shared_budget_before_allocating_or_reading_input() {
    // Goal: denial cannot allocate compressed storage or read content, even for valid input.
    // Method: exhaust the real budget, observe layouts and count reads of a real file.
    let fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(1024));
    let retained = budget.reserve(1024).unwrap();
    let (file, size) = open_file(&fixture.path, 1024 * 1024).unwrap();
    let mut reader = CountedReader { file, calls: 0 };

    let (result, observation) =
        allocation::observe(&budget, || capture_budgeted(&mut reader, size, &budget));

    assert!(matches!(result, Err(AdmissionRefusal::Budget)));
    assert_eq!(reader.calls, 0);
    assert_eq!(observation.allocations, 0);
    assert_eq!(budget.used(), retained.bytes());
}

#[test]
fn refuses_input_when_only_scratch_fits_before_allocating_or_reading() {
    // Goal: partial reservation cannot start capture when compressed input cannot also fit.
    // Method: allow all scratch but one byte less than the real file, then observe denial.
    let fixture = Fixture::new();
    let (file, size) = open_file(&fixture.path, 1024 * 1024).unwrap();
    let maximum =
        super::admission::cursor_scratch_bytes().unwrap() + usize::try_from(size).unwrap() - 1;
    let budget = Arc::new(ByteBudget::new(maximum));
    let mut reader = CountedReader { file, calls: 0 };

    let (result, observation) =
        allocation::observe(&budget, || capture_budgeted(&mut reader, size, &budget));

    assert!(matches!(result, Err(AdmissionRefusal::Budget)));
    assert_eq!(reader.calls, 0);
    assert_eq!(observation.allocations, 0);
    assert_eq!(budget.used(), 0);
}

struct CountedReader {
    file: std::fs::File,
    calls: u64,
}

impl Read for CountedReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        self.file.read(bytes)
    }
}
