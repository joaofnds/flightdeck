use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

thread_local! {
    static COUNTS: Cell<Option<Counts>> = const { Cell::new(None) };
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Counts {
    pub calls: u64,
    pub bytes: u64,
}

struct Allocator;
struct Measurement;

pub(crate) fn measure<T>(operation: impl FnOnce() -> T) -> (T, Counts) {
    COUNTS.with(|state| assert!(state.replace(Some(Counts::default())).is_none()));
    let measurement = Measurement;
    let result = operation();
    let counts = COUNTS.with(|state| state.take().unwrap());
    drop(measurement);

    (result, counts)
}

impl Drop for Measurement {
    fn drop(&mut self) {
        COUNTS.with(|state| state.set(None));
    }
}

// Measurements cover only the calling test thread; other tests retain the system allocator.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

fn record(size: usize) {
    COUNTS.with(|state| {
        if let Some(mut counts) = state.get() {
            counts.calls += 1;
            counts.bytes += u64::try_from(size).unwrap();
            state.set(Some(counts));
        }
    });
}
