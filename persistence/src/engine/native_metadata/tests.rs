mod certificate;
mod fixtures;

use std::io::Cursor;
use std::sync::Arc;

use polars_buffer::Buffer;
use polars_parquet::parquet::read::deserialize_metadata;
use polars_parquet_format::KeyValue;

use super::requested_bytes;
use crate::allocation_tests as allocation;
use crate::engine::admission::validate_reader_metadata;
use crate::resources::ByteBudget;

#[test]
fn accounts_for_a_large_unused_file_value_and_root_name() {
    // Goal: metadata unrelated to the selected columns remains charged in full.
    // Method: admit and natively construct a footer containing a large KV and root name.
    let mut metadata = fixtures::metadata(&["selected"], 1);
    metadata.schema[0].name = "r".repeat(256 * 1024);
    metadata.key_value_metadata = Some(vec![KeyValue {
        key: "unused".into(),
        value: Some("v".repeat(2 * 1024 * 1024)),
    }]);

    let observation = observe_metadata(&fixtures::encode(&metadata));

    assert!(observation.live_bytes >= 2 * 1024 * 1024 + 256 * 1024);
}

#[test]
fn accounts_for_every_empty_row_group_and_long_leaf_name() {
    // Goal: zero rows do not hide the full native row-group and descriptor allocations.
    // Method: admit 24 empty groups and three 128-byte names, then construct and drop them.
    let names = ["a".repeat(128), "b".repeat(128), "c".repeat(128)];
    let names: Vec<_> = names.iter().map(String::as_str).collect();
    let footer = fixtures::encode(&fixtures::metadata(&names, 24));

    let observation = observe_metadata(&footer);

    assert_eq!(observation.groups, 24);
    assert_eq!(observation.columns, 3);
    assert!(observation.live_bytes > 24 * 3 * 128);
}

#[test]
fn accounts_for_duplicate_name_lookup_storage() {
    // Goal: duplicate physical names charge the native UnitVec lookup allocations.
    // Method: admit eight identical long names in twelve empty groups and build metadata.
    let name = "n".repeat(128);
    let footer = fixtures::encode(&fixtures::metadata(&[name.as_str(); 8], 12));

    let observation = observe_metadata(&footer);

    assert_eq!(observation.groups, 12);
    assert_eq!(observation.columns, 8);
    assert!(observation.live_bytes > 12 * 8 * 128);
}

#[test]
fn retains_the_wire_envelope_after_a_large_value_is_replaced() {
    // Goal: a small final metadata value cannot erase an earlier allocation obligation.
    // Method: append an empty replacement KV list after an admitted large original list.
    let mut metadata = fixtures::metadata(&["selected"], 0);
    metadata.key_value_metadata = Some(vec![KeyValue {
        key: "unused".into(),
        value: Some("v".repeat(1024 * 1024)),
    }]);
    let footer = fixtures::replaced_values(&metadata);

    let observation = observe_metadata(&footer);

    let wire_bytes = super::super::admission::VECTOR_BYTES_MAX + footer.len();
    assert!(observation.requested_bytes >= wire_bytes);
    assert!(observation.allocated_bytes >= 1024 * 1024);
    assert!(observation.live_bytes < 1024 * 1024);
}

#[test]
fn accounts_for_replaced_native_order_lists_despite_zero_sized_raw_orders() {
    // Goal: zero-sized admission list elements do not hide native order-tag allocations.
    // Method: replace 4096 raw orders with an empty list and observe the native backing.
    use polars_parquet_format::{ColumnOrder, TypeDefinedOrder};
    let mut metadata = fixtures::metadata(&["selected"], 0);
    metadata.column_orders = Some(vec![ColumnOrder::TYPEORDER(TypeDefinedOrder {}); 4096]);
    let footer = fixtures::replaced_orders(&metadata);

    let observation = observe_metadata(&footer);

    assert_eq!(size_of::<ColumnOrder>(), 0);
    assert_eq!(observation.byte_layout_max, 4096);
    let wire_bytes = super::super::admission::VECTOR_BYTES_MAX + 2 * footer.len();
    assert!(observation.requested_bytes >= wire_bytes);
}

#[test]
fn refuses_overflow_before_a_native_allocation() {
    // Goal: the numeric envelope rejects a byte count that cannot be represented as a Layout.
    // Method: offer the maximum usize for an otherwise ordinary metadata record.
    let metadata = fixtures::metadata(&["selected"], 0);

    let result = requested_bytes(&metadata, usize::MAX);

    assert!(matches!(
        result,
        Err(crate::Error::Bound("parquet native metadata bytes"))
    ));
}

struct MetadataObservation {
    allocated_bytes: usize,
    byte_layout_max: usize,
    columns: usize,
    groups: usize,
    live_bytes: usize,
    requested_bytes: usize,
}

fn observe_metadata(footer: &[u8]) -> MetadataObservation {
    let file = fixtures::file(footer);
    let size = u64::try_from(file.len()).unwrap();
    let admitted = validate_reader_metadata(&mut Cursor::new(&file), size).unwrap();
    let requested_bytes = requested_bytes(&admitted, footer.len()).unwrap();
    let budget = Arc::new(ByteBudget::new(requested_bytes));
    let reservation = budget.reserve(requested_bytes).unwrap();

    let observation = Buffer::with_slice(footer, |buffer| {
        let (metadata, construction) =
            allocation::observe(&budget, || deserialize_metadata(buffer).unwrap());
        let live_bytes = construction.allocated_bytes - construction.deallocated_bytes;
        let byte_layout_max = construction.layouts[..construction.allocations]
            .iter()
            .filter(|layout| layout.1 == 1)
            .map(|layout| layout.0)
            .max()
            .unwrap();
        let columns = metadata.schema().columns().len();
        let groups = metadata.row_groups.len();
        let ((), destruction) = allocation::observe(&budget, || drop(metadata));

        // Cumulative requests bound peak conservatively for these fixtures, not in general.
        assert!(construction.allocated_bytes + 48 <= requested_bytes);
        assert!(live_bytes + 48 <= requested_bytes);
        assert_eq!(construction.allocation_charge_min, requested_bytes);
        assert_eq!(destruction.allocations, 0);
        assert_eq!(destruction.deallocated_bytes, live_bytes);
        assert_eq!(destruction.deallocation_charge_min, requested_bytes);
        MetadataObservation {
            allocated_bytes: construction.allocated_bytes,
            byte_layout_max,
            columns,
            groups,
            live_bytes,
            requested_bytes,
        }
    });
    drop(reservation);
    assert_eq!(budget.used(), 0);

    observation
}
