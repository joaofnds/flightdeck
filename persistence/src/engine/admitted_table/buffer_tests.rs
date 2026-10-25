use std::fs::File;
use std::sync::Arc;

use polars::prelude::{ScanSources, col};

use super::AdmittedTable;
use crate::allocation_tests::observe;
use crate::engine::{AdmittedFile, Row, scan, source_tests::Fixture};
use crate::resources::ByteBudget;

#[test]
fn buffer_wrapping_preserves_the_original_backing_and_identifies_native_wrappers() {
    // Goal: moving into native ownership cannot clone compressed bytes or detach their lease.
    // Method: observe wrapping separately from schema/collect, then free the original pointer.
    let fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        AdmittedTable::from_file_budgeted(File::open(&fixture.path).unwrap(), &budget).unwrap();
    let pointer = table.input.bytes.as_ptr();
    let capacity = table.input.bytes.capacity();
    drop(table.names);
    assert_eq!(budget.used(), capacity);

    let (sources, allocation) = observe(&budget, || scan::buffer_sources(table.input));

    let ScanSources::Buffers(buffers) = &sources else {
        panic!("buffer source required");
    };
    assert_eq!(buffers[0].as_ptr(), pointer);
    assert_eq!(allocation.allocations, 3);
    assert_eq!(allocation.reallocations, 0);
    // Rust 1.98/aarch64 and the pinned polars-buffer: Box<InputOwner>, storage, source Arc.
    // SharedStorageInner has three words plus BackingStorage's tag and two-word payload.
    assert_eq!(
        &allocation.layouts[..3],
        &[
            (size_of::<AdmittedFile>(), align_of::<AdmittedFile>()),
            (6 * size_of::<usize>(), align_of::<usize>()),
            (
                2 * size_of::<usize>() + size_of::<polars_buffer::Buffer<u8>>(),
                align_of::<usize>()
            ),
        ]
    );
    eprintln!(
        "bridge requested layouts: {:?}; C={capacity}",
        &allocation.layouts[..3]
    );
    let ((), freed) = observe(&budget, || drop(sources));
    let original = original_frees(&freed, pointer);
    assert_eq!(original, [(pointer.addr(), capacity, capacity)]);
    assert_eq!(budget.used(), 0);
}

#[test]
fn lazy_clones_and_original_byte_slices_retain_the_compressed_charge() {
    // Goal: aliases outlive the initial adapter without reopening or releasing original bytes.
    // Method: retain a plan and byte slice, remove the path, collect, then drop every alias.
    let fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        AdmittedTable::from_file_budgeted(File::open(&fixture.path).unwrap(), &budget).unwrap();
    let capacity = table.input.bytes.capacity();
    let pointer = table.input.bytes.as_ptr();
    drop(table.names);
    let sources = scan::buffer_sources(table.input);
    let ScanSources::Buffers(buffers) = &sources else {
        panic!("buffer source required");
    };
    let alias = buffers[0].clone().sliced(0..4);
    let initial = scan::scan_sources(sources).unwrap();
    let retained = initial.clone();
    drop(initial);
    std::fs::remove_file(&fixture.path).unwrap();
    assert_eq!(budget.used(), capacity);

    let frame = retained.clone().select([col("counter")]).collect().unwrap();

    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 42);
    assert_eq!(budget.used(), capacity);
    drop(retained);
    drop(frame);
    assert_eq!(alias.as_ptr(), pointer);
    assert_eq!(alias.as_ref(), b"PAR1");
    assert_eq!(budget.used(), capacity);
    let ((), freed) = observe(&budget, || drop(alias));
    let original = original_frees(&freed, pointer);
    assert_eq!(original, [(pointer.addr(), capacity, capacity)]);
    assert_eq!(budget.used(), 0);
}

fn original_frees(
    observation: &crate::allocation_tests::Observation,
    pointer: *const u8,
) -> Vec<(usize, usize, usize)> {
    (0..observation.deallocations)
        .filter_map(|index| {
            let address = observation.free_addresses[index];
            (address == pointer.addr()).then_some((
                address,
                observation.free_bytes[index],
                observation.free_charges[index],
            ))
        })
        .collect()
}

#[test]
fn copied_primitive_results_survive_final_input_and_name_owner_release() {
    // Goal: independent decoded results need not retain compressed storage or column names.
    // Method: collect a retained public scan clone after removal, then drop its final plan.
    let fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        AdmittedTable::from_file_budgeted(File::open(&fixture.path).unwrap(), &budget).unwrap();
    let input_bytes = table.input.bytes.capacity();
    let name_bytes = table.names.reservation.bytes();
    let (initial, names) = table.into_scan().unwrap();
    let retained = initial.clone();
    drop(initial);
    std::fs::remove_file(&fixture.path).unwrap();
    assert_eq!(budget.used(), input_bytes + name_bytes);

    let frame = retained.clone().select([col("counter")]).collect().unwrap();
    drop(retained);

    assert_eq!(budget.used(), name_bytes);
    assert!(names.contains("counter"));
    drop(names);
    assert_eq!(budget.used(), 0);
    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 42);
    drop(frame);
    assert_eq!(budget.used(), 0);
}
