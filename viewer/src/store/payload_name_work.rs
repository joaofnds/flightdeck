//! Bounded test observations of logical name-comparison and allocation work.
use std::cell::{Cell, RefCell};
use std::sync::Arc;

use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::live_app_response::{Bytes, Observation};

#[derive(Clone, Copy, Debug)]
pub(super) enum Comparison {
    ScratchSort,
    ScratchDedup,
    AcceptedSearch,
    PendingSearch,
    AcceptedSort,
}

impl Comparison {
    fn index(self) -> usize {
        match self {
            Self::ScratchSort => 0,
            Self::ScratchDedup => 1,
            Self::AcceptedSearch => 2,
            Self::PendingSearch => 3,
            Self::AcceptedSort => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::store) struct NameWork {
    pub comparisons: [u64; 5],
    pub logical_bytes: [u64; 5],
    pub lookups: u64,
    pub named_rows: u64,
    pub distinct_names: u64,
    pub missing_names: u64,
    pub new_names: u64,
    pub allocation_requests: u64,
    pub allocation_bytes: u64,
    pub scratch_bytes: u64,
    pub pending_bytes: u64,
    pub sorts: u64,
    pub sorted_entries: u64,
}

impl NameWork {
    pub(super) fn since(self, before: Self) -> Self {
        Self {
            comparisons: std::array::from_fn(|index| {
                self.comparisons[index] - before.comparisons[index]
            }),
            logical_bytes: std::array::from_fn(|index| {
                self.logical_bytes[index] - before.logical_bytes[index]
            }),
            lookups: self.lookups - before.lookups,
            named_rows: self.named_rows - before.named_rows,
            distinct_names: self.distinct_names - before.distinct_names,
            missing_names: self.missing_names - before.missing_names,
            new_names: self.new_names - before.new_names,
            allocation_requests: self.allocation_requests - before.allocation_requests,
            allocation_bytes: self.allocation_bytes - before.allocation_bytes,
            scratch_bytes: self.scratch_bytes - before.scratch_bytes,
            pending_bytes: self.pending_bytes - before.pending_bytes,
            sorts: self.sorts - before.sorts,
            sorted_entries: self.sorted_entries - before.sorted_entries,
        }
    }
}

pub(super) fn comparison(work: &Cell<NameWork>, site: Comparison, left: &Bytes, right: &Bytes) {
    let raw = logical_prefix(&left.bytes, &right.bytes);
    let display = if left.bytes == right.bytes {
        logical_prefix(left.display.as_bytes(), right.display.as_bytes())
    } else {
        0
    };
    let mut value = work.get();
    value.comparisons[site.index()] += 1;
    value.logical_bytes[site.index()] += u64::try_from(raw + display).unwrap();
    work.set(value);
}

fn logical_prefix(left: &[u8], right: &[u8]) -> usize {
    let equal = left
        .iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count();

    equal + usize::from(equal < left.len().min(right.len()))
}

#[derive(Clone, Copy, Debug)]
pub(in crate::store) struct Receipt {
    pub observation: Observation,
    pub committed: bool,
    pub index_count: usize,
    pub index_capacity: usize,
    pub index_bytes: usize,
    pub work: NameWork,
}

pub(in crate::store) struct Records {
    values: Vec<Receipt>,
    reservation: ByteReservation,
}

impl Records {
    pub(in crate::store) fn values(&self) -> &[Receipt] {
        assert_eq!(
            self.reservation.bytes(),
            self.values.capacity() * size_of::<Receipt>()
        );

        &self.values
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Records>> = const { RefCell::new(None) };
}

pub(in crate::store) struct Trace {
    active: bool,
}

impl Trace {
    pub(in crate::store) fn new(capacity: usize, observer: &Arc<ByteBudget>) -> Self {
        let bytes = capacity.checked_mul(size_of::<Receipt>()).unwrap();
        let reservation = observer.reserve(bytes).unwrap();
        let values = Vec::with_capacity(capacity);
        assert_eq!(values.capacity(), capacity);
        ACTIVE.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Records {
                values,
                reservation,
            });
        });

        Self { active: true }
    }

    pub(in crate::store) fn finish(mut self) -> Records {
        let records = ACTIVE.with(|slot| slot.borrow_mut().take().unwrap());
        self.active = false;

        records
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        if self.active {
            ACTIVE.with(|slot| drop(slot.borrow_mut().take()));
        }
    }
}

pub(super) fn record(receipt: &Receipt) {
    ACTIVE.with(|slot| {
        if let Some(records) = &mut *slot.borrow_mut() {
            assert!(records.values.len() < records.values.capacity());
            records.values.push(*receipt);
        }
    });
}
