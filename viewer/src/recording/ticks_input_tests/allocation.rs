use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Observer;

#[global_allocator]
static ALLOCATOR: Observer = Observer;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static REQUESTS: Cell<u64> = const { Cell::new(0) };
}

pub(super) fn observe<T>(work: impl FnOnce() -> T) -> (T, u64) {
    REQUESTS.set(0);
    assert!(!ACTIVE.replace(true));
    let guard = Guard;
    let result = work();
    drop(guard);
    (result, REQUESTS.get())
}

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        ACTIVE.set(false);
    }
}

fn count() {
    if ACTIVE.get() {
        REQUESTS.set(REQUESTS.get().checked_add(1).unwrap());
    }
}

unsafe impl GlobalAlloc for Observer {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(pointer, layout, bytes) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}
