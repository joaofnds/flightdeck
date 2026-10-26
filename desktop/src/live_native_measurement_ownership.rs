use serde_json::{Value, json};

#[repr(C)]
#[derive(Default)]
struct MallocStatistics {
    blocks_in_use: u32,
    size_in_use: usize,
    max_size_in_use: usize,
    size_allocated: usize,
}

unsafe extern "C" {
    fn malloc_zone_statistics(zone: *mut std::ffi::c_void, statistics: *mut MallocStatistics);
}

pub(super) fn snapshot() -> Value {
    let mut statistics = MallocStatistics::default();
    unsafe { malloc_zone_statistics(std::ptr::null_mut(), &raw mut statistics) };
    let footprint = persistence::resources::Footprint::read().unwrap();
    json!({"allocator_blocks_in_use": statistics.blocks_in_use,
        "allocator_size_in_use_bytes": statistics.size_in_use,
        "allocator_reserved_bytes": statistics.size_allocated,
        "allocator_sum_zone_touched_high_water_bytes": statistics.max_size_in_use,
        "physical_footprint_bytes": footprint.current_bytes,
        "physical_footprint_lifetime_max_bytes": footprint.lifetime_bytes_max,
        "resident_bytes": footprint.resident_bytes})
}

#[test]
fn observes_allocator_ownership_while_a_real_allocation_is_held() {
    // Goal: current ownership and reservation stay distinct from cumulative request bytes.
    // Method: hold and touch an 8 MiB system allocation between process allocator snapshots.
    let buffer = vec![37_u8; 8 * 1024 * 1024];
    std::hint::black_box(&buffer);

    let held = snapshot();

    assert!(
        held["allocator_size_in_use_bytes"].as_u64().unwrap()
            >= u64::try_from(buffer.len()).unwrap()
    );
    assert!(
        held["allocator_reserved_bytes"].as_u64().unwrap()
            >= held["allocator_size_in_use_bytes"].as_u64().unwrap()
    );
    drop(buffer);
}
