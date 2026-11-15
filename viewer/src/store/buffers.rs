//! The buffers the store writes its readings into, allocated when the store starts, and the
//! mailbox it leaves its Views in. Neither thread waits on the other: a buffer is written again
//! only once nothing but the buffers holds it, a store that finds every buffer held makes a new
//! one in the slot of the one written longest ago, and the main thread takes a new View only
//! when no store step is handing one over. So a View or a query context, however long it is
//! held, never stops the store or its launch, and keeps alive only the readings it holds.

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
    /// Whether `release_once` cleared each buffer after it was last written or released, so it
    /// clears a free buffer once and not at every View the store publishes. Only a write makes a
    /// buffer held, so a buffer cleared stays as cleared until it is written or released. A
    /// buffer `make` just made counts as cleared, since nothing was written into it.
    cleared: Vec<bool>,
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

        Self {
            all,
            cleared: vec![true; count],
            make,
            next: 0,
        }
    }

    /// A buffer that nothing else holds, written by `fill`, or `None` when every buffer is
    /// held.
    pub fn write(&mut self, fill: impl FnOnce(&mut T)) -> Option<Arc<T>> {
        let index = self.write_index()?;

        Some(self.write_at(index, fill))
    }

    /// A buffer written by `fill`, made new where every buffer is held. The new one takes the
    /// slot of the one written longest ago, which only its holders then keep alive: a View, a
    /// query context, or the span where it still holds that reading. The count of buffers never
    /// grows, and the readings in memory are these buffers and what the span, the Views and
    /// the contexts hold.
    pub(super) fn write_replacing(&mut self, fill: impl FnOnce(&mut T)) -> Arc<T> {
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
        self.cleared[index] = false;
        self.next = (index + 1) % self.all.len();

        Arc::clone(&self.all[index])
    }

    /// Clears with `clear` each buffer that nothing else holds, so a buffer waiting to be
    /// written again keeps nothing it held alive.
    pub fn release(&mut self, mut clear: impl FnMut(&mut T)) {
        assert_eq!(self.cleared.len(), self.all.len());
        for (buffer, cleared) in self.all.iter_mut().zip(&mut self.cleared) {
            if let Some(buffer) = Arc::get_mut(buffer) {
                clear(buffer);
                // `clear` may leave a buffer otherwise than `release_once` does.
                *cleared = false;
            }
        }
    }

    /// Clears with `clear` each buffer that nothing else holds and that it has not cleared
    /// since the buffer was last written or released, so a store whose buffers are mostly free
    /// pays for each once. `clear` must leave a buffer as clearing it again would.
    pub(super) fn release_once(&mut self, mut clear: impl FnMut(&mut T)) {
        assert_eq!(self.cleared.len(), self.all.len());
        for (buffer, cleared) in self.all.iter_mut().zip(&mut self.cleared) {
            if *cleared {
                continue;
            }
            if let Some(buffer) = Arc::get_mut(buffer) {
                clear(buffer);
                *cleared = true;
            }
        }
    }

    /// Moves out what each buffer holds that nothing else holds and that has not been cleared
    /// since it was last written or released, at most `count_max` buffers, and leaves each the
    /// buffer `released` makes from it, which must leave it as clearing it again would. Returns
    /// what it moved out, which it allocates nothing for where it found nothing, so the caller
    /// can free it elsewhere.
    pub(super) fn release_once_batch(
        &mut self,
        count_max: usize,
        mut released: impl FnMut(&T) -> T,
    ) -> Vec<T> {
        assert!(count_max > 0);
        assert_eq!(self.cleared.len(), self.all.len());

        let mut batch = Vec::new();
        for (buffer, cleared) in self.all.iter_mut().zip(&mut self.cleared) {
            if batch.len() == count_max {
                break;
            }
            if *cleared {
                continue;
            }
            let Some(buffer) = Arc::get_mut(buffer) else {
                continue;
            };
            if batch.is_empty() {
                batch.reserve_exact(count_max);
            }
            let left = released(buffer);
            batch.push(std::mem::replace(buffer, left));
            *cleared = true;
        }

        batch
    }

    /// Whether a buffer that nothing else holds has not been cleared since it was last written
    /// or released.
    #[cfg(test)]
    pub(super) fn releasable(&self) -> bool {
        self.all
            .iter()
            .zip(&self.cleared)
            .any(|(buffer, &cleared)| !cleared && Arc::strong_count(buffer) == 1)
    }

    /// The buffers that nothing else holds.
    #[cfg(test)]
    pub(super) fn unheld(&self) -> impl Iterator<Item = &T> {
        self.all
            .iter()
            .filter(|buffer| Arc::strong_count(buffer) == 1)
            .map(AsRef::as_ref)
    }

    /// How many buffers there are, held or not.
    pub(super) fn count(&self) -> usize {
        self.all.len()
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

    /// Clears with `clear` each buffer nothing else holds once, as `Buffers::release_once` does.
    pub(super) fn release_once(&mut self, clear: impl FnMut(&mut T)) {
        self.buffers.release_once(clear);
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
