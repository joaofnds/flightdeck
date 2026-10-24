//! One byte budget shared by loader inputs, decoded work and immutable retained payloads.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Capacity is reserved before allocation and released only with its owning reservation.
#[derive(Debug)]
pub struct ByteBudget {
    maximum: usize,
    used: AtomicUsize,
}

/// Move this owner with the allocation, including into a published immutable buffer.
#[derive(Debug)]
#[must_use]
pub struct ByteReservation {
    budget: Arc<ByteBudget>,
    bytes: usize,
}

impl ByteBudget {
    #[must_use]
    pub fn new(maximum: usize) -> Self {
        Self {
            maximum,
            used: AtomicUsize::new(0),
        }
    }

    /// Returns no owner when the full allocation cannot fit; it never reserves a prefix.
    pub fn reserve(self: &Arc<Self>, bytes: usize) -> Option<ByteReservation> {
        match self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= self.maximum)
            }) {
            Ok(_) => Some(ByteReservation {
                budget: Arc::clone(self),
                bytes,
            }),
            Err(_) => None,
        }
    }

    #[must_use]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn maximum(&self) -> usize {
        self.maximum
    }
}

impl ByteReservation {
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        let before = self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        assert!(before >= self.bytes);
    }
}
