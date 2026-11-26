//! The global allocator of the binaries that link polars and ship in the app or measure it:
//! `flightdeck` and `persister`, which ship, and `query_bench`, `resume_bench`,
//! `store_payload` and `projection_bench`, whose footprint and allocation figures stand for
//! theirs. The `persister` unit tests run on it too, since they build into that binary, and
//! the counting allocators of the `persistence` and `flightdeck` unit tests and of the
//! `admitted_buffer_observation` test delegate to it. Every other test and binary allocates
//! through `System`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::{c_char, c_int, c_void};
use std::io;
use std::ptr::null_mut;

/// The global allocator the module names its binaries for. It serves a block of at least
/// `MAPPED_BYTES_MIN` aligned past `SYSTEM_ALIGNMENT_MAX` but within `MAPPED_ALIGNMENT_MAX` from
/// pages mapped for it alone, and every other block from `System`. `System` serves such a
/// zeroed block by writing zeros over all of it, so all of it counts in the memory footprint at
/// once, as polars' process-wide 8 MiB block of zeros does. Mapped pages read zero and count in
/// no footprint until they are touched.
pub struct AlignedPages;

/// The smallest block mapped pages serve, so a small block aligned past 16 bytes, such as a
/// padded queue slot, stays on `System`, the rounding up to whole pages wastes under 2% of a
/// block, and each mapping spans 64 pages or more.
const MAPPED_BYTES_MIN: usize = 1 << 20;
/// The largest alignment mapped pages serve, the smallest page of any Mac, since a mapping
/// starts on a page.
const MAPPED_ALIGNMENT_MAX: usize = 4096;
/// The largest alignment `System` serves with `malloc`, past which it takes `posix_memalign`
/// and then writes the zeros of a zeroed block itself.
const SYSTEM_ALIGNMENT_MAX: usize = 16;

const _: () = assert!(SYSTEM_ALIGNMENT_MAX < MAPPED_ALIGNMENT_MAX);
const _: () = assert!(MAPPED_ALIGNMENT_MAX.is_power_of_two());
const _: () = assert!(MAPPED_ALIGNMENT_MAX <= MAPPED_BYTES_MIN);

// <sys/mman.h>, checked against the macOS 27.2 SDK.
const PROT_READ: c_int = 0x01;
const PROT_WRITE: c_int = 0x02;
const MAP_PRIVATE: c_int = 0x0002;
const MAP_ANON: c_int = 0x1000;
/// The address of `MAP_FAILED`, `(void *)-1`.
const MAP_FAILED: usize = usize::MAX;
const MINCORE_INCORE: c_char = 0x1;

/// The bytes of polars' process-wide block of zeros, `GLOBAL_ZERO_SIZE` in polars-buffer.
const SHARED_ZEROS_BYTES: usize = 8 << 20;

unsafe extern "C" {
    fn getpagesize() -> c_int;
    fn mincore(address: *const c_void, length: usize, vector: *mut c_char) -> c_int;
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        file: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
}

unsafe impl GlobalAlloc for AlignedPages {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if mapped(layout) {
            map(layout.size())
        } else {
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // Mapped pages read zero until they are written, so they need no zeros written.
        if mapped(layout) {
            map(layout.size())
        } else {
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if mapped(layout) {
            unsafe { unmap(pointer, layout.size()) };
        } else {
            unsafe { System.dealloc(pointer, layout) };
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // The caller promises that `size`, rounded up to the alignment of `layout`, does not
        // overflow `isize`, and that alignment comes from a `Layout`, so it is a power of two.
        // Those are all `Layout` asks, as std's default `realloc` relies on too.
        let resized = unsafe { Layout::from_size_align_unchecked(size, layout.align()) };

        if mapped(layout) || mapped(resized) {
            unsafe { realloc_moved(pointer, layout, resized) }
        } else {
            unsafe { System.realloc(pointer, layout, size) }
        }
    }
}

/// Whether mapped pages serve `layout`. `dealloc` routes by the layout alone, so `alloc` and
/// `alloc_zeroed` must both answer by this.
fn mapped(layout: Layout) -> bool {
    let aligned_past_system = SYSTEM_ALIGNMENT_MAX < layout.align();
    let aligned_within_pages = layout.align() <= MAPPED_ALIGNMENT_MAX;
    let large = MAPPED_BYTES_MIN <= layout.size();

    aligned_past_system && aligned_within_pages && large
}

/// A block of `resized` holding what the block at `pointer` held, up to the smaller size, with
/// the old block freed, or null with the old block kept where no block of `resized` is to be
/// had.
unsafe fn realloc_moved(pointer: *mut u8, layout: Layout, resized: Layout) -> *mut u8 {
    let moved = unsafe { AlignedPages.alloc(resized) };
    if moved.is_null() {
        return moved;
    }

    let kept_bytes = layout.size().min(resized.size());
    unsafe { std::ptr::copy_nonoverlapping(pointer, moved, kept_bytes) };
    unsafe { AlignedPages.dealloc(pointer, layout) };

    moved
}

/// Pages for `bytes`, private to this process and zero until written, or null where the
/// kernel refuses them, as a global allocator reports a failure.
fn map(bytes: usize) -> *mut u8 {
    let protection = PROT_READ | PROT_WRITE;
    let flags = MAP_PRIVATE | MAP_ANON;

    let pages = unsafe { mmap(null_mut(), bytes, protection, flags, -1, 0) };

    if pages.addr() == MAP_FAILED {
        null_mut()
    } else {
        pages.cast()
    }
}

/// Gives the pages `map` returned at `pointer` back to the kernel. A refusal is reported and
/// then aborts, since a global allocator must not unwind and has no caller to report to.
unsafe fn unmap(pointer: *mut u8, bytes: usize) {
    let status = unsafe { munmap(pointer.cast(), bytes) };

    if status != 0 {
        collector::diagnostics::error(format_args!(
            "aligned pages: munmap of {bytes} bytes failed: {}",
            io::Error::last_os_error()
        ));
        std::process::abort();
    }
}

/// The pages of polars' process-wide block of zeros that the kernel holds resident, found
/// without reading the block, so a release build can check that the block still comes from
/// mapped pages. The optimizer turns polars' `vec!` of zeroed chunks into `alloc_zeroed` only
/// in a release build, so in a debug build every page is resident.
///
/// # Errors
///
/// Returns the error of `mincore` where the kernel refuses to report residency.
pub fn shared_zeros_resident_page_count() -> io::Result<usize> {
    // A block of zeros that fits polars' shared block borrows it rather than allocating.
    let zeros = polars_buffer::Buffer::<u8>::zeroed(SHARED_ZEROS_BYTES);

    resident_page_count(zeros.as_ptr(), zeros.len())
}

fn page_bytes() -> io::Result<usize> {
    usize::try_from(unsafe { getpagesize() }).map_err(io::Error::other)
}

/// The count of the pages holding the `bytes` at `block` that the kernel holds resident.
fn resident_page_count(block: *const u8, bytes: usize) -> io::Result<usize> {
    let page_bytes = page_bytes()?;
    let offset = block.addr() % page_bytes;
    let first_page = block.wrapping_sub(offset);
    let spanned_bytes = offset + bytes;
    let mut pages: Vec<c_char> = vec![0; spanned_bytes.div_ceil(page_bytes)];

    let status = unsafe { mincore(first_page.cast(), spanned_bytes, pages.as_mut_ptr()) };

    if status == 0 {
        Ok(resident_page_count_of(&pages))
    } else {
        Err(io::Error::last_os_error())
    }
}

fn resident_page_count_of(pages: &[c_char]) -> usize {
    pages
        .iter()
        .filter(|page| **page & MINCORE_INCORE == MINCORE_INCORE)
        .count()
}

#[cfg(test)]
mod tests;
