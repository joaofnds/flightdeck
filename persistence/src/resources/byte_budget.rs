//! One byte budget shared by loader inputs, decoded work and immutable retained payloads.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
mod tests;

/// Capacity is reserved before allocation and released only with its owning reservation.
#[derive(Debug)]
pub struct ByteBudget {
    maximum: usize,
    used: AtomicUsize,
    used_peak: AtomicUsize,
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
            used_peak: AtomicUsize::new(0),
        }
    }

    /// Returns no owner when the full allocation cannot fit; it never reserves a prefix.
    pub fn reserve(self: &Arc<Self>, bytes: usize) -> Option<ByteReservation> {
        if !self.charge(bytes) {
            return None;
        }

        Some(ByteReservation {
            budget: Arc::clone(self),
            bytes,
        })
    }

    /// Whether `bytes` fit beside what is reserved, in which case they are now reserved too.
    fn charge(&self, bytes: usize) -> bool {
        let charged = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= self.maximum)
            });
        let Ok(before) = charged else {
            return false;
        };

        self.used_peak.fetch_max(before + bytes, Ordering::Relaxed);
        true
    }

    #[must_use]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    /// Largest reserved total recorded by successful calls over this budget's lifetime.
    /// Concurrent `used` and peak reads are independent snapshots; join callers for a final peak.
    /// This measures reservations, excluding uncharged allocations and physical footprint.
    #[must_use]
    pub fn used_peak(&self) -> usize {
        self.used_peak.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn maximum(&self) -> usize {
        self.maximum
    }
}

impl ByteReservation {
    /// Adds `bytes` to this owner when the whole of them fit, as `ByteBudget::reserve` would, so
    /// an allocation built a piece at a time is charged before each piece stays. It never grows by
    /// a prefix.
    #[must_use]
    pub fn grow(&mut self, bytes: usize) -> bool {
        if !self.budget.charge(bytes) {
            return false;
        }

        self.bytes += bytes;
        true
    }

    /// Returns unused preflight room while the actual allocation keeps its owner.
    pub fn shrink_to(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        let released = self.bytes - bytes;
        if released == 0 {
            return;
        }

        let before = self.budget.used.fetch_sub(released, Ordering::AcqRel);
        assert!(before >= released);
        self.bytes = bytes;
    }

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
