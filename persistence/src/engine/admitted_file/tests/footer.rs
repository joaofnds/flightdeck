use std::io::Cursor;
use std::sync::Arc;

use polars::prelude::{Column, DataFrame};
use polars_parquet::parquet::metadata::ThriftFileMetadata;
use polars_parquet_format::KeyValue;
use polars_parquet_format::thrift::protocol::{TCompactInputProtocol, TCompactOutputProtocol};

use crate::allocation_tests::observe;
use crate::engine::AdmittedFile;
use crate::engine::source_tests::{Fixture, write_frame};
use crate::resources::ByteBudget;

#[test]
fn a_small_projection_does_not_copy_the_captured_file_for_its_footer() {
    // Goal: native metadata borrows the admitted capture instead of copying its trailing bytes.
    // Method: observe a real small-file projection and require its exact counter afterward.
    let fixture = Fixture::new();
    let size = usize::try_from(std::fs::metadata(&fixture.path).unwrap().len()).unwrap();
    assert!(size < 65_536);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let admitted = AdmittedFile::open_budgeted(&fixture.path, 1024 * 1024, &budget).unwrap();
    drop(admitted.read(&["counter"]).unwrap());

    let (result, allocations) = observe(&budget, || admitted.read(&["counter"]));
    let frame = result.unwrap();
    drop(admitted);

    assert_eq!(frame, expected_counter());
    assert!(!allocations.layouts[..allocations.allocations].contains(&(size, 1)));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_large_footer_needs_neither_prefetch_nor_a_second_footer_buffer() {
    // Goal: crossing the native prefetch limit cannot create a second footer copy.
    // Method: add unused metadata to a real file and observe its unchanged projected value.
    let fixture = Fixture::new();
    let footer_bytes = extend_footer(&fixture, 131_073);
    assert!(footer_bytes > 2 * 65_536);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let admitted = AdmittedFile::open_budgeted(&fixture.path, 1024 * 1024, &budget).unwrap();
    drop(admitted.read(&["counter"]).unwrap());

    let (result, allocations) = observe(&budget, || admitted.read(&["counter"]));
    let frame = result.unwrap();
    drop(admitted);

    assert_eq!(frame, expected_counter());
    assert!(!allocations.layouts[..allocations.allocations].contains(&(65_536, 1)));
    assert!(!allocations.layouts[..allocations.allocations].contains(&(footer_bytes, 1)));
    assert_eq!(budget.used(), 0);
}

#[test]
fn string_results_survive_the_borrowed_footer_and_input_owners() {
    // Goal: long string values remain independent of temporary metadata storage.
    // Method: read a nullable repeated string column, then remove the file and captured owner.
    let fixture = Fixture::new();
    let label = "a retained machine label".repeat(16);
    let expected = DataFrame::new(
        3,
        vec![Column::new(
            "label".into(),
            [Some(label.as_str()), None, Some(label.as_str())],
        )],
    )
    .unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    write_frame(&fixture.path, &mut expected.clone());
    let admitted = AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();

    let frame = admitted.read(&["label"]).unwrap();
    drop(admitted);
    std::fs::remove_file(&fixture.path).unwrap();

    assert_eq!(frame, expected);
}

#[test]
fn binary_results_survive_the_borrowed_footer_and_input_owners() {
    // Goal: byte results remain independent of native metadata and the captured input.
    // Method: read repeated non-UTF8 values and nulls, then release both original owners.
    let fixture = Fixture::new();
    let bytes = [0xff_u8; 384];
    let expected = DataFrame::new(
        3,
        vec![Column::new(
            "bytes".into(),
            [Some(bytes.as_slice()), None, Some(bytes.as_slice())],
        )],
    )
    .unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    write_frame(&fixture.path, &mut expected.clone());
    let admitted = AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();

    let frame = admitted.read(&["bytes"]).unwrap();
    drop(admitted);
    std::fs::remove_file(&fixture.path).unwrap();

    assert_eq!(frame, expected);
}

#[test]
fn arrow_schema_precedence_is_preserved_over_unannotated_integer_storage() {
    // Goal: supplying native metadata preserves the existing Arrow-schema override.
    // Method: contrast one UInt64 Arrow field with unannotated storage and no override.
    let fixture = Fixture::new();
    std::fs::remove_file(&fixture.path).unwrap();
    write_frame(
        &fixture.path,
        &mut DataFrame::new(1, vec![Column::new("counter".into(), [u64::MAX])]).unwrap(),
    );
    rewrite_footer(&fixture, |metadata| {
        assert!(
            metadata
                .key_value_metadata
                .as_ref()
                .unwrap()
                .iter()
                .any(|entry| { entry.key == "ARROW:schema" })
        );
        metadata.schema[1].converted_type = None;
        metadata.schema[1].logical_type = None;
    });
    let overridden = AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();
    rewrite_footer(&fixture, |metadata| {
        metadata
            .key_value_metadata
            .as_mut()
            .unwrap()
            .retain(|entry| entry.key != "ARROW:schema");
    });
    let physical = AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();

    let unsigned = overridden.read(&["counter"]).unwrap();
    let signed = physical.read(&["counter"]).unwrap();
    drop(overridden);
    drop(physical);

    assert_eq!(
        unsigned,
        DataFrame::new(1, vec![Column::new("counter".into(), [u64::MAX])]).unwrap()
    );
    assert_eq!(
        signed,
        DataFrame::new(1, vec![Column::new("counter".into(), [-1_i64])]).unwrap()
    );
}

#[test]
fn a_missing_column_error_outlives_the_metadata_and_input() {
    // Goal: native error ownership cannot retain the scoped borrowed footer.
    // Method: request an absent column, release the captured file, then inspect the rich error.
    let fixture = Fixture::new();
    let admitted = AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();

    let error = admitted.read(&["absent"]).unwrap_err();
    drop(admitted);
    std::fs::remove_file(&fixture.path).unwrap();

    assert!(matches!(
        error,
        crate::Error::Engine(polars::error::PolarsError::ColumnNotFound(_))
    ));
    assert!(!error.to_string().is_empty());
}

fn expected_counter() -> DataFrame {
    DataFrame::new(1, vec![Column::new("counter".into(), [42_u64])]).unwrap()
}

fn extend_footer(fixture: &Fixture, padding_bytes: usize) -> usize {
    rewrite_footer(fixture, |metadata| {
        metadata
            .key_value_metadata
            .get_or_insert_with(Vec::new)
            .push(KeyValue {
                key: "unused_padding".to_owned(),
                value: Some("x".repeat(padding_bytes)),
            });
    })
}

fn rewrite_footer(fixture: &Fixture, change: impl FnOnce(&mut ThriftFileMetadata)) -> usize {
    let original = std::fs::read(&fixture.path).unwrap();
    let end = original.len() - 8;
    let length = usize::try_from(u32::from_le_bytes(
        original[end..end + 4].try_into().unwrap(),
    ))
    .unwrap();
    let start = end - length;
    let mut protocol =
        TCompactInputProtocol::new(Cursor::new(&original[start..end]), 4 * 1024 * 1024);
    let mut metadata = ThriftFileMetadata::read_from_in_protocol(&mut protocol).unwrap();
    change(&mut metadata);
    let mut footer = Vec::new();
    metadata
        .write_to_out_protocol(&mut TCompactOutputProtocol::new(&mut footer))
        .unwrap();
    let length = u32::try_from(footer.len()).unwrap();
    let mut rewritten = original[..start].to_vec();
    rewritten.extend_from_slice(&footer);
    rewritten.extend_from_slice(&length.to_le_bytes());
    rewritten.extend_from_slice(b"PAR1");
    std::fs::write(&fixture.path, rewritten).unwrap();

    footer.len() + 8
}
