//! The buffers the store writes its readings into, all allocated when the store starts, and
//! the mailbox it leaves its Views in. Neither thread waits on the other: a buffer is written
//! again only once no View holds it, and the main thread takes a new View only when no store
//! step is handing one over.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};

#[cfg(test)]
#[path = "buffers_tests.rs"]
mod tests;

/// Buffers of one kind, all allocated when the store starts. A buffer is written again only
/// once nothing but these buffers holds it, so a reading a View holds never changes.
pub struct Buffers<T> {
    all: Vec<Arc<T>>,
    /// Where the search for a buffer to write starts, past the one written last, so a buffer
    /// just released is written again last.
    next: usize,
}

impl<T> Buffers<T> {
    /// `count` buffers, each made by `make`.
    pub fn new(count: usize, mut make: impl FnMut() -> T) -> Self {
        assert!(count > 0);

        let mut all = Vec::with_capacity(count);
        for _ in 0..count {
            all.push(Arc::new(make()));
        }

        Self { all, next: 0 }
    }

    /// A buffer that nothing else holds, written by `fill`, or `None` when every buffer is
    /// held.
    pub fn write(&mut self, fill: impl FnOnce(&mut T)) -> Option<Arc<T>> {
        let count = self.all.len();
        for offset in 0..count {
            let index = (self.next + offset) % count;
            if let Some(buffer) = Arc::get_mut(&mut self.all[index]) {
                fill(buffer);
                self.next = (index + 1) % count;

                return Some(Arc::clone(&self.all[index]));
            }
        }

        None
    }

    /// Clears with `clear` each buffer that nothing else holds, so a buffer waiting to be
    /// written again keeps nothing it held alive.
    pub fn release(&mut self, mut clear: impl FnMut(&mut T)) {
        for buffer in &mut self.all {
            if let Some(buffer) = Arc::get_mut(buffer) {
                clear(buffer);
            }
        }
    }

    /// How many buffers nothing but these buffers holds.
    #[must_use]
    pub fn free(&self) -> usize {
        self.all
            .iter()
            .filter(|buffer| Arc::strong_count(buffer) == 1)
            .count()
    }
}

/// The readings of one span, oldest first, at most `capacity` of them, each in a buffer of
/// its own.
pub struct Span<T> {
    buffers: Buffers<T>,
    held: VecDeque<Arc<T>>,
    capacity: usize,
}

impl<T> Span<T> {
    /// A span of at most `capacity` readings over `buffers`, which needs more buffers than
    /// readings, since a View the main thread holds keeps readings the span let go of.
    #[must_use]
    pub fn new(capacity: usize, buffers: Buffers<T>) -> Self {
        assert!(capacity > 0);
        assert!(buffers.all.len() > capacity);

        Self {
            buffers,
            held: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Adds the reading `fill` writes after the newest, letting go of the oldest when the span
    /// is full. Returns false and changes nothing when every buffer is held, so the caller
    /// takes the reading again at its next step.
    pub fn push(&mut self, fill: impl FnOnce(&mut T)) -> bool {
        if !self.has_room() {
            return false;
        }

        if self.held.len() == self.capacity {
            drop(self.held.pop_front());
        }
        assert!(self.held.len() < self.capacity);

        let Some(reading) = self.buffers.write(fill) else {
            unreachable!("a span writes a reading only once a buffer is free");
        };

        self.held.push_back(reading);
        true
    }

    /// Whether `push` finds a buffer to write: a free one, or the oldest reading's where the
    /// span is full and no View holds it.
    #[must_use]
    pub fn has_room(&self) -> bool {
        if self.buffers.free() > 0 {
            return true;
        }

        if self.held.len() == self.capacity {
            self.oldest_held_only_here()
        } else {
            false
        }
    }

    /// The reading the next `push` lets go of, the oldest where the span is full.
    #[must_use]
    pub fn leaving(&self) -> Option<&T> {
        if self.held.len() == self.capacity {
            self.held.front().map(AsRef::as_ref)
        } else {
            None
        }
    }

    /// Whether the oldest reading's buffer is free once the span lets go of it, since no View
    /// holds it.
    fn oldest_held_only_here(&self) -> bool {
        match self.held.front() {
            Some(oldest) => Arc::strong_count(oldest) == 2,
            None => false,
        }
    }

    /// The readings the span holds, oldest first.
    #[must_use]
    pub fn held(&self) -> impl ExactSizeIterator<Item = &Arc<T>> {
        self.held.iter()
    }

    pub fn newest(&self) -> Option<&T> {
        self.held.back().map(AsRef::as_ref)
    }
}

/// Where the store thread leaves its newest View for the main thread.
pub struct Mailbox<V> {
    newest: Mutex<Option<Arc<V>>>,
}

impl<V> Mailbox<V> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            newest: Mutex::new(None),
        }
    }

    /// Leaves `view` for the main thread in place of any View it has not taken.
    pub fn put(&self, view: Arc<V>) {
        let mut newest = match self.newest.lock() {
            Ok(newest) => newest,
            Err(poisoned) => poisoned.into_inner(),
        };
        let untaken = newest.replace(view);

        drop(newest);
        drop(untaken);
    }

    /// The View the store left since the last take, or `None` when it left none or is leaving
    /// one now, so the main thread never waits on the store.
    pub fn take(&self) -> Option<Arc<V>> {
        let mut newest = match self.newest.try_lock() {
            Ok(newest) => newest,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };

        newest.take()
    }
}

impl<V> Default for Mailbox<V> {
    fn default() -> Self {
        Self::new()
    }
}
