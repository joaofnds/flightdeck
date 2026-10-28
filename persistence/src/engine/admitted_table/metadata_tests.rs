use std::fs::File;
use std::io::Cursor;
use std::sync::Arc;

use polars_parquet_format::thrift::protocol::TCompactOutputProtocol;

use super::{AdmittedReadRefusal, AdmittedTable};
use crate::allocation_tests::observe;
use crate::engine::AdmissionRefusal;
use crate::engine::source_tests::Fixture;
use crate::resources::ByteBudget;

#[test]
fn insufficient_metadata_room_refuses_before_any_native_allocation() {
    // Goal: compressed input admission cannot authorize unreserved native metadata.
    // Method: leave one byte after capture and observe only the new native-read boundary.
    let fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        AdmittedTable::from_file_budgeted(File::open(&fixture.path).unwrap(), &budget).unwrap();
    let input_bytes = budget.used();
    let retained = budget.reserve(budget.maximum() - input_bytes - 1).unwrap();

    let (result, requests) = observe(&budget, || {
        table.read_metadata_budgeted(&["counter"], Some(1))
    });

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
    ));
    assert_eq!(requests.allocations, 0);
    assert_eq!(requests.deallocations, 0);
    assert_eq!(budget.used(), input_bytes + retained.bytes());
}

#[test]
fn metadata_charge_precedes_native_requests_and_ends_after_metadata_destruction() {
    // Goal: native construction and destruction stay inside the metadata lease.
    // Method: observe a real eager read, then retain its scalar result past input release.
    let fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        AdmittedTable::from_file_budgeted(File::open(&fixture.path).unwrap(), &budget).unwrap();
    let input_bytes = budget.used();
    let charge = input_bytes + table.metadata_bytes;

    let (result, requests) = observe(&budget, || {
        table.read_metadata_budgeted(&["counter"], Some(1))
    });
    let frame = result.unwrap();

    assert!(requests.allocations > 0);
    assert!(requests.deallocations > 0);
    assert!(requests.allocation_charge_min >= charge);
    assert!(requests.deallocation_charge_min >= charge);
    assert_eq!(budget.used(), input_bytes);
    drop(table);
    assert_eq!(budget.used(), 0);
    assert_eq!(
        crate::engine::Row::new(&frame, 0)
            .unwrap()
            .get::<u64>(0)
            .unwrap(),
        42
    );
}

#[test]
fn native_metadata_failure_drops_its_rich_message_before_releasing_the_lease() {
    // Goal: a pure native metadata error cannot outlive its scratch charge.
    // Method: admit an unknown codec, then observe the inline native-metadata refusal.
    let fixture = Fixture::new();
    replace_codec(&fixture.path);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        AdmittedTable::from_file_budgeted(File::open(&fixture.path).unwrap(), &budget).unwrap();
    let input_bytes = budget.used();
    let charge = input_bytes + table.metadata_bytes;

    let (result, requests) = observe(&budget, || {
        table.read_metadata_budgeted(&["counter"], Some(1))
    });

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Decode(crate::Error::Bound(
            "native parquet footer metadata"
        )))
    ));
    assert_eq!(requests.allocated_bytes, requests.deallocated_bytes);
    assert!(requests.deallocation_charge_min >= charge);
    assert_eq!(budget.used(), input_bytes);
    drop(table);
    assert_eq!(budget.used(), 0);
}

fn replace_codec(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).unwrap();
    let mut metadata = crate::engine::admission::validate_reader_metadata(
        &mut Cursor::new(&bytes),
        u64::try_from(bytes.len()).unwrap(),
    )
    .unwrap();
    metadata.row_groups[0].columns[0]
        .meta_data
        .as_mut()
        .unwrap()
        .codec = polars_parquet_format::CompressionCodec(9999);
    let mut footer = Vec::new();
    metadata
        .write_to_out_protocol(&mut TCompactOutputProtocol::new(&mut footer))
        .unwrap();
    let trailer = bytes.len() - 8;
    let length = u32::from_le_bytes(bytes[trailer..trailer + 4].try_into().unwrap());
    bytes.truncate(trailer - usize::try_from(length).unwrap());
    bytes.extend_from_slice(&footer);
    bytes.extend_from_slice(&u32::try_from(footer.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(b"PAR1");
    std::fs::write(path, bytes).unwrap();
}
