use std::sync::Arc;

use super::{AppAttributionMetric, Key, Point};
use crate::apps::AppKey;

/// The cached points' per-App totals, kept as points enter and leave, so a query adds each
/// App once instead of once for every cached second it appears in.
pub(crate) struct Sums {
    /// Sorted by key, with one entry for each App some cached point holds.
    entries: Vec<Entry>,
}

struct Entry {
    key: Arc<Key>,
    points: u32,
    totals: [Total; 3],
}

/// The measured amount and elapsed time one App adds to its context mean.
#[derive(Clone, Copy, Default)]
pub(crate) struct Total {
    pub amount: u128,
    pub elapsed_ns: u128,
}

const _: () = {
    assert!(size_of::<Entry>() <= 112);
};

impl Sums {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub(super) fn key(&self, key: &AppKey<'_>) -> Option<&Arc<Key>> {
        let index = self.search(key).ok()?;
        Some(&self.entries[index].key)
    }

    /// The bytes adding `point` would take, its new keys and the entries they need.
    pub(super) fn added_bytes(&self, point: &Point) -> usize {
        let mut entries = 0;
        let mut keys = 0;
        for app in &point.apps {
            if self.search(&app.key.view()).is_err() {
                entries += 1;
                keys += app.key.bytes();
            }
        }
        let needed = self.entries.len() + entries;

        keys + needed.saturating_sub(self.entries.capacity()) * size_of::<Entry>()
    }

    /// Adds `point` and returns the bytes it took, which `added_bytes` gave beforehand.
    pub(super) fn add(&mut self, point: &Point) -> usize {
        let capacity = self.entries.capacity();
        let missing = point
            .apps
            .iter()
            .filter(|app| self.search(&app.key.view()).is_err())
            .count();
        self.entries.reserve_exact(missing);
        let mut keys = 0;
        for app in &point.apps {
            let totals = totals(point, app);
            match self.search(&app.key.view()) {
                Ok(index) => {
                    let entry = &mut self.entries[index];
                    entry.points += 1;
                    for (total, value) in entry.totals.iter_mut().zip(totals) {
                        total.amount += value.amount;
                        total.elapsed_ns += value.elapsed_ns;
                    }
                }
                Err(index) => {
                    keys += app.key.bytes();
                    self.entries.insert(
                        index,
                        Entry {
                            key: Arc::clone(&app.key),
                            points: 1,
                            totals,
                        },
                    );
                }
            }
        }

        keys + (self.entries.capacity() - capacity) * size_of::<Entry>()
    }

    /// Removes `point`, which an earlier `add` counted, and returns the bytes it released.
    pub(super) fn remove(&mut self, point: &Point) -> usize {
        let capacity = self.entries.capacity();
        let mut keys = 0;
        for app in &point.apps {
            let totals = totals(point, app);
            let found = self.search(&app.key.view());
            assert!(found.is_ok(), "a cached point's App has a total");
            let (Ok(index) | Err(index)) = found;
            let entry = &mut self.entries[index];
            entry.points -= 1;
            for (total, value) in entry.totals.iter_mut().zip(totals) {
                total.amount -= value.amount;
                total.elapsed_ns -= value.elapsed_ns;
            }
            if entry.points == 0 {
                keys += entry.key.bytes();
                self.entries.remove(index);
            }
        }
        if self.entries.len() < capacity {
            self.entries.shrink_to_fit();
        }

        keys + (capacity - self.entries.capacity()) * size_of::<Entry>()
    }

    pub(super) fn totals(
        &self,
        metric: AppAttributionMetric,
    ) -> impl Iterator<Item = (AppKey<'_>, Total)> {
        let index = metric_index(metric);
        self.entries
            .iter()
            .map(move |entry| (entry.key.view(), entry.totals[index]))
    }

    fn search(&self, key: &AppKey<'_>) -> Result<usize, usize> {
        self.entries
            .binary_search_by(|entry| entry.key.view().cmp(key))
    }
}

fn totals(point: &Point, app: &super::App) -> [Total; 3] {
    let mut totals = [Total::default(); 3];
    // A mean counts only seconds with a measured amount and a clock that moved.
    let Some(elapsed) = point.capture.elapsed_ns.filter(|elapsed| *elapsed > 0) else {
        return totals;
    };
    for (total, measure) in totals.iter_mut().zip([app.cpu, app.power, app.disk]) {
        if let Some(amount) = measure.measure().measured() {
            total.amount = u128::from(amount);
            total.elapsed_ns = u128::from(elapsed);
        }
    }

    totals
}

fn metric_index(metric: AppAttributionMetric) -> usize {
    match metric {
        AppAttributionMetric::Cpu => 0,
        AppAttributionMetric::Power => 1,
        AppAttributionMetric::Disk => 2,
    }
}
