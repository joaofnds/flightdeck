//! Counts the calls a test makes to the allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<u64> = const { Cell::new(0) };
}

pub(crate) fn allocator_calls_in(work: impl FnOnce()) -> u64 {
    CALLS.set(0);
    COUNTING.set(true);
    work();
    COUNTING.set(false);
    CALLS.get()
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.get() {
            CALLS.set(CALLS.get() + 1);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if COUNTING.get() {
            CALLS.set(CALLS.get() + 1);
        }
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNTING.get() {
            CALLS.set(CALLS.get() + 1);
        }
        unsafe { System.realloc(pointer, layout, size) }
    }
}
