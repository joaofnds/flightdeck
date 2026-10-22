use crate::apps::AppKey;
use crate::live_app_response::Measure;

use super::AppAttributionMetric;

/// Per-App totals of a set of seconds, kept as seconds enter and leave the set, so a query adds
/// each App once instead of once for every second it appears in.
pub(crate) struct Sums<K> {
    /// Sorted by key, with one entry for each App some second in the set holds.
    entries: Vec<Entry<K>>,
}

struct Entry<K> {
    key: K,
    key_bytes: usize,
    points: u32,
    totals: [Total; 4],
}

/// The measured amount and elapsed time one App adds to its context mean.
#[derive(Clone, Copy, Default)]
pub(crate) struct Total {
    pub amount: u128,
    pub elapsed_ns: u128,
}

/// One App of one second, with its CPU, power, disk and GPU measures in that order.
pub(crate) trait Summed {
    type Key: SumKey;

    fn view(&self) -> AppKey<'_>;
    fn measures(&self) -> [Measure; 4];
    /// The key an entry keeps for this App.
    fn key(&self) -> Self::Key;
    /// The bytes that key takes beyond the entry itself.
    fn key_bytes(&self) -> usize;
}

pub(crate) trait SumKey {
    fn view(&self) -> AppKey<'_>;
}

/// A key the totals own, for seconds whose own keys must not gain another reference.
pub(crate) struct OwnedKey {
    bundle: bool,
    bytes: Box<[u8]>,
}

impl<K: SumKey> Sums<K> {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// The key an entry keeps for `key`. A caller passing each App of one second in turn passes
    /// the same `index_next`, which the search tries first and leaves at the entry after `key`'s,
    /// or at the place `key` would take when no entry holds it.
    pub(crate) fn key(&self, key: &AppKey<'_>, index_next: &mut usize) -> Option<&K> {
        match self.search(key, *index_next) {
            Ok(index) => {
                *index_next = index + 1;
                Some(&self.entries[index].key)
            }
            Err(index) => {
                *index_next = index;
                None
            }
        }
    }

    /// The bytes adding `apps` would take, their new keys and the entries they need.
    pub(crate) fn added_bytes<A: Summed<Key = K>>(&self, apps: &[A]) -> usize {
        let mut entries = 0;
        let mut keys = 0;
        let mut index_next = 0;
        for app in apps {
            if self.key(&app.view(), &mut index_next).is_none() {
                entries += 1;
                keys += app.key_bytes();
            }
        }
        let needed = self.entries.len() + entries;

        keys + needed.saturating_sub(self.entries.capacity()) * size_of::<Entry<K>>()
    }

    /// Adds one second's `apps` and returns the bytes they took, which `added_bytes` gave
    /// beforehand.
    pub(crate) fn add<A: Summed<Key = K>>(&mut self, elapsed: Option<u64>, apps: &[A]) -> usize {
        let capacity = self.entries.capacity();
        self.entries.reserve_exact(self.add_missing(apps));
        let mut keys = 0;
        let mut index_next = 0;
        for app in apps {
            let totals = totals(elapsed, app.measures());
            match self.search(&app.view(), index_next) {
                Ok(index) => {
                    index_next = index + 1;
                    let entry = &mut self.entries[index];
                    entry.points += 1;
                    for (total, value) in entry.totals.iter_mut().zip(totals) {
                        total.amount += value.amount;
                        total.elapsed_ns += value.elapsed_ns;
                    }
                }
                Err(index) => {
                    index_next = index + 1;
                    keys += app.key_bytes();
                    self.entries.insert(
                        index,
                        Entry {
                            key: app.key(),
                            key_bytes: app.key_bytes(),
                            points: 1,
                            totals,
                        },
                    );
                }
            }
        }

        keys + (self.entries.capacity() - capacity) * size_of::<Entry<K>>()
    }

    /// How many of one second's `apps` no entry holds yet.
    fn add_missing<A: Summed<Key = K>>(&self, apps: &[A]) -> usize {
        let mut missing = 0;
        let mut index_next = 0;
        for app in apps {
            if self.key(&app.view(), &mut index_next).is_none() {
                missing += 1;
            }
        }

        missing
    }

    /// Removes one second's `apps`, which an earlier `add` counted, and returns the bytes it
    /// released.
    pub(crate) fn remove<A: Summed<Key = K>>(&mut self, elapsed: Option<u64>, apps: &[A]) -> usize {
        let capacity = self.entries.capacity();
        let mut keys = 0;
        let mut index_next = 0;
        for app in apps {
            let totals = totals(elapsed, app.measures());
            let found = self.search(&app.view(), index_next);
            assert!(found.is_ok(), "a summed second's App has a total");
            let (Ok(index) | Err(index)) = found;
            index_next = index + 1;
            let entry = &mut self.entries[index];
            entry.points -= 1;
            for (total, value) in entry.totals.iter_mut().zip(totals) {
                total.amount -= value.amount;
                total.elapsed_ns -= value.elapsed_ns;
            }
            if entry.points == 0 {
                keys += entry.key_bytes;
                self.entries.remove(index);
                index_next = index;
            }
        }
        if self.entries.len() < capacity {
            self.entries.shrink_to_fit();
        }

        keys + (capacity - self.entries.capacity()) * size_of::<Entry<K>>()
    }

    /// The bytes the entries and their keys take now, counted from scratch.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Entry<K>>()
            + self
                .entries
                .iter()
                .map(|entry| entry.key_bytes)
                .sum::<usize>()
    }

    pub(crate) fn totals(
        &self,
        metric: AppAttributionMetric,
    ) -> impl Iterator<Item = (AppKey<'_>, Total)> {
        let index = match metric {
            AppAttributionMetric::Cpu => 0,
            AppAttributionMetric::Power => 1,
            AppAttributionMetric::Disk => 2,
            AppAttributionMetric::Gpu => 3,
        };
        self.entries
            .iter()
            .map(move |entry| (entry.key.view(), entry.totals[index]))
    }

    /// Searches the entries for `key` as `binary_search_by` does. The Apps of one second arrive
    /// in key order, so the entry at `index_next`, after the last App's, usually holds the next
    /// App, and trying it first spares the search, whose key comparisons dominate a cold history.
    fn search(&self, key: &AppKey<'_>, index_next: usize) -> Result<usize, usize> {
        if let Some(entry) = self.entries.get(index_next) {
            if entry.key.view() == *key {
                return Ok(index_next);
            }
        }

        self.entries
            .binary_search_by(|entry| entry.key.view().cmp(key))
    }
}

impl OwnedKey {
    pub(crate) fn capture(key: &AppKey<'_>) -> Self {
        let (AppKey::Bundle(bytes) | AppKey::Executable(bytes)) = key;
        Self {
            bundle: matches!(key, AppKey::Bundle(_)),
            bytes: (*bytes).into(),
        }
    }

    pub(crate) fn heap_bytes(key: &AppKey<'_>) -> usize {
        let (AppKey::Bundle(bytes) | AppKey::Executable(bytes)) = key;
        bytes.len()
    }
}

impl SumKey for OwnedKey {
    fn view(&self) -> AppKey<'_> {
        if self.bundle {
            AppKey::Bundle(&self.bytes)
        } else {
            AppKey::Executable(&self.bytes)
        }
    }
}

fn totals(elapsed: Option<u64>, measures: [Measure; 4]) -> [Total; 4] {
    let mut totals = [Total::default(); 4];
    // A mean counts only seconds with a measured amount and a clock that moved.
    let Some(elapsed) = elapsed.filter(|elapsed| *elapsed > 0) else {
        return totals;
    };
    for (total, measure) in totals.iter_mut().zip(measures) {
        if let Some(amount) = measure.measured() {
            total.amount = u128::from(amount);
            total.elapsed_ns = u128::from(elapsed);
        }
    }

    totals
}
