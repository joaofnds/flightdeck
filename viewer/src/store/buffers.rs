//! The buffers the store writes its readings into, allocated when the store starts, and the
//! mailbox it leaves its Views in. Neither thread waits on the other: a buffer is written again
//! only once no View holds it, a store that finds every buffer held makes a new one, and the main
//! thread takes a new View only when no store step is handing one over.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};

use collector::ring::Identity;
use persistence::retention::Retained;

#[cfg(test)]
#[path = "buffers_tests.rs"]
mod tests;

/// Buffers of one kind, allocated when the store starts. A buffer is written again only once
/// nothing but these buffers holds it, so a reading a View holds never changes.
pub struct Buffers<T> {
    all: Vec<Arc<T>>,
    make: fn() -> T,
    /// Where the search for a buffer to write starts, past the one written last, so a buffer
    /// just released is written again last.
    next: usize,
}

impl<T> Buffers<T> {
    /// `count` buffers, each made by `make`.
    pub fn new(count: usize, make: fn() -> T) -> Self {
        assert!(count > 0);

        let mut all = Vec::with_capacity(count);
        for _ in 0..count {
            all.push(Arc::new(make()));
        }

        Self { all, make, next: 0 }
    }

    /// A buffer that nothing else holds, written by `fill`, or `None` when every buffer is
    /// held.
    pub fn write(&mut self, fill: impl FnOnce(&mut T)) -> Option<Arc<T>> {
        let index = self.write_index()?;

        Some(self.write_at(index, fill))
    }

    /// A buffer written by `fill`, made new where every buffer is held. The next buffer is the
    /// one written longest ago, which a span has let go of once it is full, so its new one takes
    /// its place and only the View holding it keeps it alive. The readings in memory are these
    /// buffers and what the Views hold.
    fn write_replacing(&mut self, fill: impl FnOnce(&mut T)) -> Arc<T> {
        let index = self.write_index().unwrap_or_else(|| {
            self.all[self.next] = Arc::new((self.make)());
            self.next
        });

        self.write_at(index, fill)
    }

    /// Where the next buffer nothing else holds lies, if one does.
    fn write_index(&mut self) -> Option<usize> {
        let count = self.all.len();
        for offset in 0..count {
            let index = (self.next + offset) % count;
            if Arc::get_mut(&mut self.all[index]).is_some() {
                return Some(index);
            }
        }

        None
    }

    fn write_at(&mut self, index: usize, fill: impl FnOnce(&mut T)) -> Arc<T> {
        let Some(buffer) = Arc::get_mut(&mut self.all[index]) else {
            unreachable!("the store writes only a buffer nothing else holds");
        };
        fill(buffer);
        self.next = (index + 1) % self.all.len();

        Arc::clone(&self.all[index])
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
    pub(super) fn free(&self) -> usize {
        self.buffers.free()
    }

    /// Replaces one owner using a free spare; a retained reader keeps the displaced owner.
    pub(super) fn replace(&mut self, index: usize, fill: impl FnOnce(&mut T)) -> bool {
        assert!(index < self.held.len());
        let Some(reading) = self.buffers.write(fill) else {
            return false;
        };

        self.held[index] = reading;
        true
    }

    pub(super) fn clear(&mut self) {
        self.held.clear();
    }

    pub(super) fn release(&mut self, clear: impl FnMut(&mut T)) {
        self.buffers.release(clear);
    }

    /// A span of at most `capacity` readings over `buffers`, which holds more buffers than
    /// readings, so a View the main thread holds for a moment costs the store no new buffer.
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
    /// is full. A View holding the readings keeps them, and stops nothing.
    pub fn push(&mut self, fill: impl FnOnce(&mut T)) {
        if self.held.len() == self.capacity {
            drop(self.held.pop_front());
        }
        assert!(self.held.len() < self.capacity);

        let reading = self.buffers.write_replacing(fill);
        self.held.push_back(reading);
    }

    /// Lets go of the oldest reading, which a View may go on holding.
    pub(super) fn remove_oldest(&mut self) {
        drop(self.held.pop_front());
    }

    /// How many of the oldest readings have expired: those through the newest that expired before
    /// the first still covered. `place` gives a reading's boot and clock, and a reading with none
    /// has no place on the retention clock, so it stops nothing.
    pub(super) fn expired_count(
        &self,
        retained: &Retained,
        place: impl Fn(&T) -> Option<(Identity, u64)>,
    ) -> usize {
        let mut count = 0;
        for (index, reading) in self.held.iter().enumerate() {
            let Some((boot, continuous_ns)) = place(reading) else {
                continue;
            };
            if retained.holds(boot, Some(continuous_ns)) {
                break;
            }
            count = index + 1;
        }

        count
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

    /// The readings the span holds, oldest first.
    #[must_use]
    pub fn held(&self) -> impl ExactSizeIterator<Item = &Arc<T>> {
        self.held.iter()
    }

    pub fn newest(&self) -> Option<&T> {
        self.held.back().map(AsRef::as_ref)
    }

    /// The reading at `index` of those the span holds, the oldest at 0.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&T> {
        self.held.get(index).map(AsRef::as_ref)
    }

    /// How many readings the span holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.held.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// The newest reading's buffer, which a View may hold too.
    #[must_use]
    pub fn newest_held(&self) -> Option<&Arc<T>> {
        self.held.back()
    }
}

/// Where the store thread leaves its newest View for the main thread.
pub struct Mailbox<V> {
    newest: Mutex<Option<Arc<V>>>,
    /// What tells the main thread a View is waiting, so it takes the View as it arrives rather
    /// than when it next looks.
    waker: Mutex<Option<Waker>>,
}

/// A call that wakes the main thread, made from the thread that left a View.
pub type Waker = Box<dyn Fn() + Send + Sync>;

impl<V> Mailbox<V> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            newest: Mutex::new(None),
            waker: Mutex::new(None),
        }
    }

    /// Wakes the main thread through `waker`, in place of any waker before it, each time a View
    /// is left for it.
    pub fn wake_with(&self, waker: Waker) {
        let replaced = match self.waker.lock() {
            Ok(mut current) => current.replace(waker),
            Err(poisoned) => poisoned.into_inner().replace(waker),
        };

        drop(replaced);
    }

    /// Leaves `view` for the main thread in place of any View it has not taken, and wakes it
    /// once the View can be taken.
    pub fn put(&self, view: Arc<V>) {
        let mut newest = match self.newest.lock() {
            Ok(newest) => newest,
            Err(poisoned) => poisoned.into_inner(),
        };
        let untaken = newest.replace(view);

        drop(newest);
        drop(untaken);
        let waker = match self.waker.lock() {
            Ok(waker) => waker,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(waker) = waker.as_ref() {
            waker();
        }
    }

    /// Whether a View is waiting, including a handoff currently holding the mailbox lock.
    #[must_use]
    pub fn pending(&self) -> bool {
        match self.newest.try_lock() {
            Ok(newest) => newest.is_some(),
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().is_some(),
        }
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
