use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};

use crate::resources::ByteBudget;

struct Observer;

#[global_allocator]
static ALLOCATOR: Observer = Observer;

const EVENTS_MAX: usize = 1024;

thread_local! {
    static BUDGET: Cell<*const ByteBudget> = const { Cell::new(std::ptr::null()) };
    static TRACE: RefCell<Observation> = const { RefCell::new(Observation::EMPTY) };
}

#[derive(Clone, Copy)]
pub(crate) struct Observation {
    pub allocations: usize,
    pub deallocations: usize,
    pub allocated_bytes: usize,
    pub deallocated_bytes: usize,
    pub deallocation_charge_min: usize,
    pub allocation_charge_min: usize,
    pub layouts: [(usize, usize); EVENTS_MAX],
    pub free_addresses: [usize; EVENTS_MAX],
    pub free_bytes: [usize; EVENTS_MAX],
    pub free_charges: [usize; EVENTS_MAX],
    pub reallocations: usize,
    overflowed: bool,
}

impl Observation {
    const EMPTY: Self = Self {
        allocations: 0,
        deallocations: 0,
        allocated_bytes: 0,
        deallocated_bytes: 0,
        deallocation_charge_min: usize::MAX,
        allocation_charge_min: usize::MAX,
        layouts: [(0, 0); EVENTS_MAX],
        free_addresses: [0; EVENTS_MAX],
        free_bytes: [0; EVENTS_MAX],
        free_charges: [0; EVENTS_MAX],
        reallocations: 0,
        overflowed: false,
    };
}

pub(crate) fn observe<T>(budget: &ByteBudget, work: impl FnOnce() -> T) -> (T, Observation) {
    TRACE.with(|trace| *trace.borrow_mut() = Observation::EMPTY);
    BUDGET.with(|watched| {
        assert!(watched.get().is_null());
        watched.set(std::ptr::from_ref(budget));
    });
    let guard = Guard;

    let result = work();
    drop(guard);
    let observation = TRACE.with(|trace| *trace.borrow());

    assert!(!observation.overflowed);
    (result, observation)
}

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        BUDGET.with(|watched| watched.set(std::ptr::null()));
    }
}

fn record(bytes: usize, alignment: usize, allocated: bool, allocation: *mut u8) {
    BUDGET.with(|watched| {
        let pointer = watched.get();
        if pointer.is_null() {
            return;
        }

        // Observe holds this budget borrowed until the guard disables every allocator hook.
        let used = unsafe { &*pointer }.used();
        TRACE.with(|trace| {
            let mut trace = trace.borrow_mut();
            if trace.allocations + trace.deallocations == EVENTS_MAX {
                trace.overflowed = true;
                return;
            }

            if allocated {
                let index = trace.allocations;
                trace.layouts[index] = (bytes, alignment);
                trace.allocations += 1;
                trace.allocated_bytes += bytes;
                trace.allocation_charge_min = trace.allocation_charge_min.min(used);
            } else {
                let index = trace.deallocations;
                trace.free_addresses[index] = allocation.addr();
                trace.free_bytes[index] = bytes;
                trace.free_charges[index] = used;
                trace.deallocations += 1;
                trace.deallocated_bytes += bytes;
                trace.deallocation_charge_min = trace.deallocation_charge_min.min(used);
            }
        });
    });
}

unsafe impl GlobalAlloc for Observer {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), layout.align(), true, std::ptr::null_mut());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), layout.align(), true, std::ptr::null_mut());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record(layout.size(), layout.align(), false, pointer);
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        BUDGET.with(|watched| {
            if !watched.get().is_null() {
                TRACE.with(|trace| trace.borrow_mut().reallocations += 1);
            }
        });
        record(bytes, layout.align(), true, std::ptr::null_mut());
        let replacement = unsafe { System.realloc(pointer, layout, bytes) };
        if !replacement.is_null() {
            record(layout.size(), layout.align(), false, pointer);
        }
        replacement
    }
}
