//! The global allocator, judged by what the kernel and malloc report about each block: the
//! pages the kernel holds resident, whether malloc owns the block, and whether its address is
//! still mapped.

use std::alloc::{GlobalAlloc, Layout};
use std::ffi::{c_int, c_void};

use super::{AlignedPages, page_bytes, resident_page_count};

/// A zeroed block as large as polars' process-wide block of zeros.
const ZEROED_BLOCK_BYTES: usize = 8 << 20;
/// The bytes read from the zeroed block, a whole number of pages on every Mac.
const READ_BYTES: usize = 64 << 10;
/// A block below the floor of mapped pages, a whole number of pages on every Mac.
const SMALL_BLOCK_BYTES: usize = 64 << 10;
/// A block above the floor of mapped pages.
const LARGE_BLOCK_BYTES: usize = 2 << 20;
/// A block well below the floor.
const SLOT_BYTES: usize = 1 << 10;
/// The slots held at once to judge their alignment. Malloc packs 1 KiB blocks side by side
/// when no alignment is asked, so about one in four of those it holds at once starts on a
/// 4096-byte boundary, and sixteen in a row all doing so would be an accident.
const SLOT_COUNT: usize = 16;
/// More than the address space of any Mac, so the kernel refuses to map it.
const REFUSED_BYTES: usize = 1 << 60;
const PAGE_ALIGNMENT: usize = 4096;
const SYSTEM_ALIGNMENT: usize = 16;

// <mach/vm_region.h> and <mach/kern_return.h>, checked against the macOS 27.2 SDK.
const VM_REGION_BASIC_INFO_64: c_int = 9;
const VM_REGION_BASIC_INFO_COUNT_64: u32 = 9;
const KERN_SUCCESS: c_int = 0;
const KERN_INVALID_ADDRESS: c_int = 1;
/// `vm_region_basic_info_data_64_t`, read here only as the 32-bit words it spans.
type VmRegionBasicInfoData64 = [c_int; 9];
const _: () = assert!(size_of::<VmRegionBasicInfoData64>() == 36);

/// Set in the test process that `a_block_freed_from_mapped_pages_leaves_the_address_space`
/// starts for itself.
const ALONE_VARIABLE: &str = "FLIGHTDECK_ALIGNED_PAGES_ALONE";

unsafe extern "C" {
    static mach_task_self_: u32;
    fn mach_port_deallocate(task: u32, name: u32) -> c_int;
    fn mach_vm_region(
        target_task: u32,
        address: *mut u64,
        size: *mut u64,
        flavor: c_int,
        info: *mut c_int,
        info_count: *mut u32,
        object_name: *mut u32,
    ) -> c_int;
    /// The bytes malloc holds for the block at `pointer`, or zero where malloc does not own it.
    fn malloc_size(pointer: *const c_void) -> usize;
}

#[test]
fn a_zeroed_block_from_mapped_pages_is_resident_only_where_it_was_read() {
    // Goal: a large zeroed block aligned to 4096 bytes takes no resident page until it is
    // read, and then only the pages read, each reading zero. Method: an 8 MiB block through
    // the allocator, whose resident pages mincore counts before and after its first 64 KiB is
    // read.
    let layout = page_aligned(ZEROED_BLOCK_BYTES);
    let block = unsafe { AlignedPages.alloc_zeroed(layout) };
    assert!(!block.is_null());

    let resident_before = resident_page_count(block, ZEROED_BLOCK_BYTES).unwrap();
    let read_zero = reads_zero(block, READ_BYTES);
    let resident_after = resident_page_count(block, ZEROED_BLOCK_BYTES).unwrap();
    unsafe { AlignedPages.dealloc(block, layout) };

    assert_eq!(resident_before, 0);
    assert!(read_zero);
    assert_eq!(resident_after, READ_BYTES / page_bytes().unwrap());
}

#[test]
fn a_block_grown_into_mapped_pages_keeps_its_contents() {
    // Goal: growing a block from below the floor into mapped pages keeps what it held. Method:
    // a 64 KiB block aligned to 4096 bytes, which `System` serves, filled with a pattern and
    // grown to 2 MiB, which mapped pages serve, and its first 64 KiB compared with the pattern.
    let block = filled_small_block();

    let moved = grown(block);
    let kept = pattern_byte_count(moved, SMALL_BLOCK_BYTES);
    unsafe { AlignedPages.dealloc(moved, page_aligned(LARGE_BLOCK_BYTES)) };

    assert_eq!(kept, SMALL_BLOCK_BYTES);
}

#[test]
fn a_block_grown_into_mapped_pages_makes_resident_only_the_pages_it_kept() {
    // Goal: growing a block into mapped pages copies what the old block held and no more, so
    // the copy reads nothing past the old block's end. Method: a 64 KiB block grown to 2 MiB,
    // whose resident pages mincore counts, since every page the copy writes becomes resident.
    let block = filled_small_block();

    let moved = grown(block);
    let resident = resident_page_count(moved, LARGE_BLOCK_BYTES).unwrap();
    unsafe { AlignedPages.dealloc(moved, page_aligned(LARGE_BLOCK_BYTES)) };

    assert_eq!(resident, SMALL_BLOCK_BYTES / page_bytes().unwrap());
}

#[test]
fn a_block_grown_past_the_floor_leaves_malloc_for_mapped_pages() {
    // Goal: growing a block from below the floor moves it to mapped pages, which `dealloc`
    // gives back to the kernel, rather than leaving it with `System`. Method: a 64 KiB block
    // aligned to 4096 bytes grown to 2 MiB, with `malloc_size` read before and after.
    let block = filled_small_block();
    let malloc_bytes_before = unsafe { malloc_size(block.cast()) };

    let moved = grown(block);
    let malloc_bytes_after = unsafe { malloc_size(moved.cast()) };
    unsafe { AlignedPages.dealloc(moved, page_aligned(LARGE_BLOCK_BYTES)) };

    assert!(SMALL_BLOCK_BYTES <= malloc_bytes_before);
    assert_eq!(malloc_bytes_after, 0);
}

#[test]
fn a_block_shrunk_out_of_mapped_pages_keeps_its_contents() {
    // Goal: shrinking a block from mapped pages below the floor keeps what it still holds.
    // Method: a 2 MiB block aligned to 4096 bytes, which mapped pages serve, its first 64 KiB
    // filled with a pattern, shrunk to 64 KiB, which `System` serves, and compared with it.
    let large = page_aligned(LARGE_BLOCK_BYTES);
    let block = allocated(large);
    fill_with_pattern(block, SMALL_BLOCK_BYTES);

    let moved = unsafe { AlignedPages.realloc(block, large, SMALL_BLOCK_BYTES) };
    assert!(!moved.is_null());
    let kept = pattern_byte_count(moved, SMALL_BLOCK_BYTES);
    unsafe { AlignedPages.dealloc(moved, page_aligned(SMALL_BLOCK_BYTES)) };

    assert_eq!(kept, SMALL_BLOCK_BYTES);
}

#[test]
fn a_block_freed_from_mapped_pages_leaves_the_address_space() {
    // Goal: `dealloc` gives a mapped block's pages back to the kernel. Method: a 2 MiB block
    // mapped and freed in a test process of its own on one thread, since another test's
    // mapping could land on the freed address, and `mach_vm_region` asked whether its address
    // is mapped before and after the free.
    if std::env::var_os(ALONE_VARIABLE).is_none() {
        run_alone("a_block_freed_from_mapped_pages_leaves_the_address_space");
        return;
    }
    let layout = page_aligned(LARGE_BLOCK_BYTES);
    let block = allocated(layout);
    let mapped_before = address_is_mapped(block.addr());

    unsafe { AlignedPages.dealloc(block, layout) };
    let mapped_after = address_is_mapped(block.addr());

    assert!(mapped_before);
    assert!(!mapped_after);
}

#[test]
fn a_block_below_the_floor_keeps_its_alignment() {
    // Goal: a block aligned to 4096 bytes but smaller than mapped pages serve is still aligned.
    // Method: sixteen 1 KiB blocks aligned to 4096 bytes, held at once, whose addresses are
    // checked, since malloc would pack them side by side were the alignment dropped.
    let layout = page_aligned(SLOT_BYTES);

    let aligned_count = aligned_block_count(layout, SLOT_COUNT);

    assert_eq!(aligned_count, SLOT_COUNT);
}

#[test]
fn a_block_below_the_floor_stays_with_malloc() {
    // Goal: a block aligned past 16 bytes but smaller than the floor, such as a padded queue
    // slot, is served by `System` and not given pages of its own. Method: a 64 KiB block
    // aligned to 4096 bytes, whose `malloc_size` reads zero for a block malloc does not own.
    let layout = page_aligned(SMALL_BLOCK_BYTES);

    let malloc_bytes = malloc_bytes_of(layout);

    assert!(layout.size() <= malloc_bytes);
}

#[test]
fn a_large_block_aligned_to_16_bytes_stays_with_malloc() {
    // Goal: a large block with the alignment malloc gives anyway, such as a `Vec<u8>`'s, is
    // served by `System`, which writes no zeros of its own over it. Method: a 2 MiB block
    // aligned to 16 bytes, whose `malloc_size` reads zero for a block malloc does not own.
    let layout = Layout::from_size_align(LARGE_BLOCK_BYTES, SYSTEM_ALIGNMENT).unwrap();

    let malloc_bytes = malloc_bytes_of(layout);

    assert!(layout.size() <= malloc_bytes);
}

#[test]
fn a_large_block_aligned_past_4096_bytes_stays_with_malloc() {
    // Goal: a large block aligned past what every Mac's pages give is served by `System`,
    // since a mapping promises only the alignment of a page. Method: a 2 MiB block aligned to
    // 1 MiB, whose `malloc_size` reads zero for a block malloc does not own.
    let layout = Layout::from_size_align(LARGE_BLOCK_BYTES, 1 << 20).unwrap();

    let malloc_bytes = malloc_bytes_of(layout);

    assert!(layout.size() <= malloc_bytes);
}

#[test]
fn a_block_the_kernel_refuses_to_map_is_null() {
    // Goal: a mapping the kernel refuses reads as a failed allocation, as `GlobalAlloc`
    // reports one. Method: a block larger than the address space, aligned to 4096 bytes.
    let block = unsafe { AlignedPages.alloc(page_aligned(REFUSED_BYTES)) };

    assert!(block.is_null());
}

#[test]
fn a_block_grown_past_what_the_kernel_maps_is_null_and_keeps_its_contents() {
    // Goal: a grow the kernel refuses to map fails and leaves the old block as it was, which
    // `GlobalAlloc::realloc` promises. Method: a 64 KiB block filled with a pattern and grown
    // to more than the address space, then compared with the pattern.
    let small = page_aligned(SMALL_BLOCK_BYTES);
    let block = filled_small_block();

    let moved = unsafe { AlignedPages.realloc(block, small, REFUSED_BYTES) };
    let kept = pattern_byte_count(block, SMALL_BLOCK_BYTES);
    unsafe { AlignedPages.dealloc(block, small) };

    assert!(moved.is_null());
    assert_eq!(kept, SMALL_BLOCK_BYTES);
}

fn page_aligned(bytes: usize) -> Layout {
    Layout::from_size_align(bytes, PAGE_ALIGNMENT).unwrap()
}

fn allocated(layout: Layout) -> *mut u8 {
    let block = unsafe { AlignedPages.alloc(layout) };
    assert!(!block.is_null());

    block
}

/// A block of `SMALL_BLOCK_BYTES` aligned to 4096 bytes, filled with the pattern.
fn filled_small_block() -> *mut u8 {
    let block = allocated(page_aligned(SMALL_BLOCK_BYTES));
    fill_with_pattern(block, SMALL_BLOCK_BYTES);

    block
}

/// The block `filled_small_block` returned, grown to `LARGE_BLOCK_BYTES`.
fn grown(block: *mut u8) -> *mut u8 {
    let moved =
        unsafe { AlignedPages.realloc(block, page_aligned(SMALL_BLOCK_BYTES), LARGE_BLOCK_BYTES) };
    assert!(!moved.is_null());

    moved
}

/// How many of `block_count` blocks of `layout`, held at once, start on its alignment.
fn aligned_block_count(layout: Layout, block_count: usize) -> usize {
    let blocks: Vec<*mut u8> = (0..block_count).map(|_| allocated(layout)).collect();

    let aligned_count = blocks
        .iter()
        .filter(|block| block.addr().is_multiple_of(layout.align()))
        .count();
    for block in blocks {
        unsafe { AlignedPages.dealloc(block, layout) };
    }

    aligned_count
}

/// What `malloc_size` reads for a block of `layout` from the allocator.
fn malloc_bytes_of(layout: Layout) -> usize {
    let block = allocated(layout);
    let malloc_bytes = unsafe { malloc_size(block.cast()) };
    unsafe { AlignedPages.dealloc(block, layout) };

    malloc_bytes
}

/// Runs the test named `test_name` of this module in a test process of its own, on one thread.
fn run_alone(test_name: &str) {
    let module = module_path!().strip_prefix("persistence::").unwrap();
    let test_path = format!("{module}::{test_name}");

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test_path, "--test-threads=1"])
        .env(ALONE_VARIABLE, "1")
        .status()
        .unwrap();

    assert!(status.success());
}

/// Whether any region of this process's address space holds `address`.
fn address_is_mapped(address: usize) -> bool {
    let mut region = u64::try_from(address).unwrap();
    let mut region_bytes = 0_u64;
    let mut info: VmRegionBasicInfoData64 = [0; 9];
    let mut info_count = VM_REGION_BASIC_INFO_COUNT_64;
    let mut object = 0_u32;

    // The region found holds `address`, or is the first one past it.
    let status = unsafe {
        mach_vm_region(
            mach_task_self_,
            &raw mut region,
            &raw mut region_bytes,
            VM_REGION_BASIC_INFO_64,
            info.as_mut_ptr(),
            &raw mut info_count,
            &raw mut object,
        )
    };
    if object != 0 {
        assert_eq!(unsafe { mach_port_deallocate(mach_task_self_, object) }, 0);
    }

    if status == KERN_INVALID_ADDRESS {
        false
    } else {
        assert_eq!(status, KERN_SUCCESS);
        region <= u64::try_from(address).unwrap()
    }
}

/// Whether each of the first `bytes` at `block` reads zero, read through `black_box` so the
/// compiler cannot answer from what it knows of a zeroed allocation.
fn reads_zero(block: *const u8, bytes: usize) -> bool {
    let block = unsafe { std::slice::from_raw_parts(std::hint::black_box(block), bytes) };

    block.iter().all(|byte| *byte == 0)
}

fn fill_with_pattern(block: *mut u8, bytes: usize) {
    let block = unsafe { std::slice::from_raw_parts_mut(block, bytes) };
    for (index, byte) in block.iter_mut().enumerate() {
        *byte = pattern_byte(index);
    }
}

fn pattern_byte_count(block: *const u8, bytes: usize) -> usize {
    let block = unsafe { std::slice::from_raw_parts(block, bytes) };

    block
        .iter()
        .enumerate()
        .filter(|(index, byte)| **byte == pattern_byte(*index))
        .count()
}

/// A byte that differs from its neighbours and from zero at most indices, so a block copied
/// to the wrong offset or not at all reads differently.
fn pattern_byte(index: usize) -> u8 {
    // A prime modulus keeps the pattern from repeating at any power-of-two offset.
    u8::try_from(index % 251).unwrap()
}
