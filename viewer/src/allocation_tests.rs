use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

pub(crate) fn allocations_in<T>(work: impl FnOnce() -> T) -> (T, u64) {
    assert!(!COUNTING.get());
    CALLS.set(0);
    COUNTING.set(true);
    let scope = Scope;

    let value = work();
    let calls = CALLS.get();
    drop(scope);

    (value, calls)
}

struct Counting;
struct Scope;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<u64> = const { Cell::new(0) };
}

impl Drop for Scope {
    fn drop(&mut self) {
        COUNTING.set(false);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.get() {
            CALLS.set(CALLS.get() + 1);
        }

        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNTING.get() {
            CALLS.set(CALLS.get() + 1);
        }

        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[test]
fn observes_the_allocations_in_the_measured_operation() {
    // Goal: a disconnected counter cannot make resource regression checks pass falsely.
    // Method: retain a heap allocation past the measured scope and inspect its count.
    let (bytes, calls) = allocations_in(|| vec![0_u8; 512]);

    std::hint::black_box(&bytes);

    assert!(calls > 0);
}
