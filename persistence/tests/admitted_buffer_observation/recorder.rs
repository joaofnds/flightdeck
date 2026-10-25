use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use persistence::resources::ByteBudget;

pub const EVENTS_MAX: usize = 256;
const OWNERS_MAX: usize = 128;
pub const OVERFLOW: u32 = 1;
pub const FOREIGN: u32 = 2;
pub const LOST: u32 = 4;
pub const FAILED: u32 = 8;
pub const UNMATCHED: u32 = 16;

struct Observer;

#[global_allocator]
static ALLOCATOR: Observer = Observer;
// The low bit admits new calls; other bits count in-flight calls, two per call.
// Closing admission and observing outstanding calls must be one atomic transition.
static STATE: AtomicUsize = AtomicUsize::new(0);
static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static FAULTS: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static BUDGET: Cell<*const ByteBudget> = const { Cell::new(std::ptr::null()) };
    static TRACE: RefCell<Trace> = const { RefCell::new(Trace::EMPTY) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Allocate,
    Zeroed,
    Resize,
    Free,
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub kind: Kind,
    pub pointer: usize,
    pub old_pointer: usize,
    pub bytes: usize,
    pub alignment: usize,
    pub old_bytes: usize,
    pub charge: usize,
}

impl Event {
    pub const EMPTY: Self = Self {
        kind: Kind::Allocate,
        pointer: 0,
        old_pointer: 0,
        bytes: 0,
        alignment: 1,
        old_bytes: 0,
        charge: 0,
    };
}

#[derive(Clone, Copy)]
pub struct Trace {
    pub events: [Event; EVENTS_MAX],
    pub count: usize,
    pub attempts: usize,
    pub faults: u32,
}

impl Trace {
    const EMPTY: Self = Self {
        events: [Event::EMPTY; EVENTS_MAX],
        count: 0,
        attempts: 0,
        faults: 0,
    };

    pub fn validity(&self) -> u32 {
        let mut faults = self.faults;
        if self.count != self.attempts {
            faults |= LOST;
        }

        faults
    }
}

pub struct Capture<'budget> {
    budget: &'budget ByteBudget,
}

impl<'budget> Capture<'budget> {
    pub fn start(budget: &'budget ByteBudget) -> Self {
        assert_eq!(STATE.load(Ordering::SeqCst), 0);
        TRACE.with(|trace| *trace.borrow_mut() = Trace::EMPTY);
        BUDGET.with(|watched| watched.set(std::ptr::from_ref(budget)));
        ATTEMPTS.store(0, Ordering::SeqCst);
        FAULTS.store(0, Ordering::SeqCst);
        STATE.store(1, Ordering::SeqCst);

        Self { budget }
    }

    pub fn count(&self) -> usize {
        self.assert_owner();
        TRACE.with(|trace| trace.borrow().count)
    }

    pub fn finish(self) -> Trace {
        self.assert_owner();
        let state = STATE.fetch_and(!1, Ordering::SeqCst);
        if state != 1 {
            FAULTS.fetch_or(LOST, Ordering::SeqCst);
        }
        let mut trace = TRACE.with(|trace| *trace.borrow());
        trace.attempts = ATTEMPTS.load(Ordering::SeqCst);
        trace.faults = FAULTS.load(Ordering::SeqCst);

        trace
    }

    fn assert_owner(&self) {
        BUDGET.with(|watched| assert_eq!(watched.get(), std::ptr::from_ref(self.budget)));
    }
}

impl Drop for Capture<'_> {
    fn drop(&mut self) {
        STATE.fetch_and(!1, Ordering::SeqCst);
        BUDGET.with(|watched| {
            assert_eq!(watched.get(), std::ptr::from_ref(self.budget));
            watched.set(std::ptr::null());
        });
    }
}

struct Call {
    owner: bool,
    charge: usize,
}

fn begin() -> Option<Call> {
    let entered = STATE.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |state| {
        if state & 1 == 0 {
            return None;
        }

        state.checked_add(2)
    });
    if let Err(state) = entered {
        if state & 1 != 0 {
            FAULTS.fetch_or(OVERFLOW, Ordering::SeqCst);
        }
        return None;
    }
    ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    let Ok(budget) = BUDGET.try_with(Cell::get) else {
        FAULTS.fetch_or(LOST, Ordering::SeqCst);
        return Some(Call {
            owner: false,
            charge: 0,
        });
    };
    if budget.is_null() {
        FAULTS.fetch_or(FOREIGN, Ordering::SeqCst);
        return Some(Call {
            owner: false,
            charge: 0,
        });
    }

    // Capture keeps this budget borrowed until the active phase has ended.
    Some(Call {
        owner: true,
        charge: unsafe { &*budget }.used(),
    })
}

fn finish(call: Option<Call>, mut event: Event) {
    let Some(call) = call else {
        return;
    };
    if call.owner {
        event.charge = call.charge;
        let recorded = TRACE.try_with(|trace| {
            let Ok(mut trace) = trace.try_borrow_mut() else {
                return false;
            };
            if trace.count == EVENTS_MAX {
                FAULTS.fetch_or(OVERFLOW, Ordering::SeqCst);
                return true;
            }
            let index = trace.count;
            trace.events[index] = event;
            trace.count += 1;

            true
        });
        if !matches!(recorded, Ok(true)) {
            FAULTS.fetch_or(LOST, Ordering::SeqCst);
        }
    }
    STATE.fetch_sub(2, Ordering::SeqCst);
}

unsafe impl GlobalAlloc for Observer {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let call = begin();
        let pointer = unsafe { System.alloc(layout) };
        finish(
            call,
            Event {
                kind: Kind::Allocate,
                pointer: pointer.addr(),
                bytes: layout.size(),
                alignment: layout.align(),
                ..Event::EMPTY
            },
        );

        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let call = begin();
        let pointer = unsafe { System.alloc_zeroed(layout) };
        finish(
            call,
            Event {
                kind: Kind::Zeroed,
                pointer: pointer.addr(),
                bytes: layout.size(),
                alignment: layout.align(),
                ..Event::EMPTY
            },
        );

        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let call = begin();
        unsafe { System.dealloc(pointer, layout) };
        finish(
            call,
            Event {
                kind: Kind::Free,
                pointer: pointer.addr(),
                bytes: layout.size(),
                alignment: layout.align(),
                ..Event::EMPTY
            },
        );
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        let call = begin();
        let replacement = unsafe { System.realloc(pointer, layout, bytes) };
        finish(
            call,
            Event {
                kind: Kind::Resize,
                pointer: replacement.addr(),
                old_pointer: pointer.addr(),
                old_bytes: layout.size(),
                bytes,
                alignment: layout.align(),
                ..Event::EMPTY
            },
        );

        replacement
    }
}

#[derive(Clone, Copy)]
struct Owner {
    pointer: usize,
    bytes: usize,
    alignment: usize,
}

impl Owner {
    const EMPTY: Self = Self {
        pointer: 0,
        bytes: 0,
        alignment: 1,
    };
}

pub struct Ledger {
    owners: [Owner; OWNERS_MAX],
    pub faults: u32,
}

impl Ledger {
    pub const EMPTY: Self = Self {
        owners: [Owner::EMPTY; OWNERS_MAX],
        faults: 0,
    };

    pub fn apply(&mut self, event: Event) {
        match event.kind {
            Kind::Allocate | Kind::Zeroed => self.allocate(event),
            Kind::Free => self.free(event.pointer, event.bytes, event.alignment),
            Kind::Resize => {
                if !self.matches(event.old_pointer, event.old_bytes, event.alignment) {
                    self.faults |= UNMATCHED;
                    return;
                }
                if event.pointer == 0 {
                    self.faults |= FAILED;
                    return;
                }
                self.free(event.old_pointer, event.old_bytes, event.alignment);
                self.allocate(event);
            }
        }
    }

    fn allocate(&mut self, event: Event) {
        if event.pointer == 0 {
            self.faults |= FAILED;
            return;
        }
        if self
            .owners
            .iter()
            .any(|owner| owner.pointer == event.pointer)
        {
            self.faults |= UNMATCHED;
            return;
        }
        let Some(owner) = self.owners.iter_mut().find(|owner| owner.pointer == 0) else {
            self.faults |= OVERFLOW;
            return;
        };
        *owner = Owner {
            pointer: event.pointer,
            bytes: event.bytes,
            alignment: event.alignment,
        };
    }

    fn free(&mut self, pointer: usize, bytes: usize, alignment: usize) {
        if !self.matches(pointer, bytes, alignment) {
            self.faults |= UNMATCHED;
            return;
        }
        if let Some(owner) = self
            .owners
            .iter_mut()
            .find(|owner| owner.pointer == pointer)
        {
            *owner = Owner::EMPTY;
        }
    }

    pub fn matches(&self, pointer: usize, bytes: usize, alignment: usize) -> bool {
        if pointer == 0 {
            return false;
        }

        self.owners.iter().any(|owner| {
            (owner.pointer, owner.bytes, owner.alignment) == (pointer, bytes, alignment)
        })
    }

    pub fn live(&self) -> usize {
        self.owners
            .iter()
            .filter(|owner| owner.pointer != 0)
            .count()
    }

    pub fn complete(&self) -> u32 {
        let mut faults = self.faults;
        if self.live() != 0 {
            faults |= UNMATCHED;
        }

        faults
    }
}

pub fn reentrant_events(budget: &ByteBudget) -> Trace {
    let capture = Capture::start(budget);
    TRACE.with(|trace| {
        let held = trace.borrow_mut();
        let value = std::hint::black_box(Box::new(7_u64));
        drop(value);
        drop(held);
    });

    capture.finish()
}

pub fn outstanding_call_events(budget: &ByteBudget) -> Trace {
    // Exercise the same entry/exit protocol around a phase boundary without touching malloc.
    let capture = Capture::start(budget);
    let call = begin();
    let trace = capture.finish();
    finish(call, Event::EMPTY);

    trace
}
