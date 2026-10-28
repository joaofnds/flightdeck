//! The Apps each of Overview's App charts draws on a line of its own over a window of wall
//! seconds, chosen from what a View holds: each stretch that lies wholly in the window, and each
//! other second of the window one by one.

use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

use super::attribution::PointRow;
use super::attribution::{COHORT_MAX, METRICS, ranked_metric};
use super::stretch::StretchRow;
use super::{
    AppRow, AppsSecond, AppsStretch, EARLIER_STRIDE_SECONDS, MachineSecond, SECONDS_MAX,
    STRETCHES_MAX,
};
use crate::app_attribution_history::AppAttributionMetric;
use crate::app_attribution_history::means::{Comparison, Mean, compare};
use crate::apps::AppKey;
use crate::live_app_response::{Metric, Rankable, RankedApp};

/// The Apps a window holds at most, as the worker's App histories hold them, past which it
/// chooses no Apps.
const APPS_MAX: usize = 24_000;

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

const _: () = {
    assert!(APPS_MAX == crate::live_app_response::APPS_MAX);
    // A gauge counts the seconds of a View that measured an App in a `u32`.
    assert!(EARLIER_STRIDE_SECONDS == 10);
    assert!(SECONDS_MAX + STRETCHES_MAX * 10 < 4_294_967_295);
};

/// The Apps each of Overview's App charts draws on a line of its own, chosen over the seconds
/// the charts draw, as the worker's App histories choose their cohorts.
#[derive(Debug, Default, PartialEq)]
pub struct Cohorts {
    /// How many of the View's newest seconds lie in the window, one by one: those after the
    /// newest second the clock was set back past.
    pub seconds: usize,
    /// The Apps that used the most CPU over those seconds, at most eight, the most first.
    pub cpu: Vec<Arc<RankedApp>>,
    /// The Apps that used the most energy over those seconds, at most eight, the most first.
    pub power: Vec<Arc<RankedApp>>,
    /// The Apps that read and wrote the most bytes over those seconds, at most eight, the most
    /// first.
    pub disk: Vec<Arc<RankedApp>>,
    /// The Apps that held the most resident memory over the seconds that measured it, on
    /// average, at most eight, the most first.
    pub resident: Vec<Arc<RankedApp>>,
    /// Whether those seconds hold more Apps than the store adds up, so it chose none.
    pub refused: bool,
}

impl Cohorts {
    /// Cohorts with room for the Apps every chart draws, holding none.
    #[must_use]
    pub fn with_room() -> Self {
        Self {
            seconds: 0,
            cpu: Vec::with_capacity(COHORT_MAX),
            power: Vec::with_capacity(COHORT_MAX),
            disk: Vec::with_capacity(COHORT_MAX),
            resident: Vec::with_capacity(COHORT_MAX),
            refused: false,
        }
    }

    /// The cohort of `metric`. Overview draws no App chart of GPU time.
    #[must_use]
    pub fn of(&self, metric: AppAttributionMetric) -> Option<&[Arc<RankedApp>]> {
        match metric {
            AppAttributionMetric::Cpu => Some(&self.cpu),
            AppAttributionMetric::Power => Some(&self.power),
            AppAttributionMetric::Disk => Some(&self.disk),
            AppAttributionMetric::Gpu => None,
        }
    }

    fn of_mut(&mut self, metric: AppAttributionMetric) -> &mut Vec<Arc<RankedApp>> {
        match metric {
            AppAttributionMetric::Cpu => &mut self.cpu,
            AppAttributionMetric::Power => &mut self.power,
            AppAttributionMetric::Disk => &mut self.disk,
            AppAttributionMetric::Gpu => unreachable!("Overview draws no App chart of GPU time"),
        }
    }
}

/// The wall seconds from `first` to `last`, both included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallSeconds {
    pub first: u64,
    pub last: u64,
}

/// What a View holds that the App charts choose their Apps over.
pub struct Readings<'view> {
    /// The seconds the View holds one by one, oldest first.
    pub seconds: &'view [Arc<MachineSecond>],
    /// Each App's totals at each of `seconds`, in the same order.
    pub apps: &'view [Arc<AppsSecond>],
    /// The closed stretches, oldest first, each adding up the seconds of `seconds` that lie in
    /// it.
    pub stretches: &'view [Arc<AppsStretch>],
}

/// Room to choose the Apps of Overview's App charts in, which holds nothing between choices.
pub struct Leaders {
    /// Every App of the window, in key order.
    entries: Vec<Entry>,
    /// Where adding a second or a stretch writes the entries, which then trade places with
    /// `entries`.
    spare: Vec<Entry>,
    /// The entries a chart draws, the most first.
    chosen: Vec<usize>,
}

/// One App of the window.
struct Entry {
    name: Arc<RankedApp>,
    /// What the App used of each of `METRICS` over the seconds that measured it, in their order.
    means: [Mean; 3],
    /// The App's resident memory added up over the seconds that measured it.
    resident: Gauge,
}

/// A reading of what an App holds, added up over some seconds, whose mean is per second rather
/// than per nanosecond, as the worker's history of resident memory takes it.
#[derive(Clone, Copy)]
struct Gauge {
    bytes: u128,
    /// The seconds that measured the reading.
    seconds: u32,
}

impl Gauge {
    const ZERO: Self = Self {
        bytes: 0,
        seconds: 0,
    };
}

impl Default for Leaders {
    fn default() -> Self {
        Self::new()
    }
}

impl Leaders {
    /// Room for every App a window holds.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(APPS_MAX),
            spare: Vec::with_capacity(APPS_MAX),
            chosen: Vec::with_capacity(COHORT_MAX),
        }
    }

    /// Writes into `cohorts` the Apps each chart draws over `window` of `readings`, those whose
    /// seconds measured the most of its resource on average, ties going to the App first in key
    /// order, as the worker's App histories choose them. A stretch the window cuts counts only
    /// its seconds `readings` holds one by one, and a second the clock was set back past
    /// counts none.
    pub fn choose(&mut self, readings: &Readings<'_>, window: WallSeconds, cohorts: &mut Cohorts) {
        assert_eq!(readings.seconds.len(), readings.apps.len());
        assert!(window.first <= window.last);
        assert_eq!(*cohorts, Cohorts::default());

        self.entries.clear();
        let run = choose_run(readings.seconds, window);
        cohorts.seconds = run.len();
        let Some(newest) = readings.seconds.last() else {
            return;
        };
        let wholly = choose_stretches(readings.stretches, super::stretch(newest), window);
        let stretches = &readings.stretches[wholly];

        let fits = self.choose_add_stretches(stretches)
            && self.choose_add_seconds(readings, run, stretches);
        if !fits {
            self.entries.clear();
            cohorts.refused = true;
            return;
        }
        self.choose_cohorts(cohorts);
    }

    /// Adds the Apps of `stretches`, none of a stretch that held too many to add them up, as a
    /// second that held too many adds none. Returns false where they come to more than
    /// `APPS_MAX` Apps.
    fn choose_add_stretches(&mut self, stretches: &[Arc<AppsStretch>]) -> bool {
        for stretch in stretches {
            let Some(rows) = stretch.apps() else {
                continue;
            };
            if !self.choose_add(rows, choose_add_stretch) {
                return false;
            }
        }
        true
    }

    /// Adds the Apps of the seconds `run` names of `readings`, but those of `stretches`, oldest
    /// first, which hold them. Returns false where they come to more than `APPS_MAX` Apps.
    fn choose_add_seconds(
        &mut self,
        readings: &Readings<'_>,
        run: Range<usize>,
        stretches: &[Arc<AppsStretch>],
    ) -> bool {
        for index in run {
            let id = super::stretch(&readings.seconds[index]);
            if stretches
                .binary_search_by_key(&id, |stretch| stretch.stretch)
                .is_ok()
            {
                continue;
            }

            let second = &readings.apps[index];
            let elapsed_ns = second.elapsed_ns.filter(|elapsed_ns| *elapsed_ns > 0);
            if !self.choose_add(&second.apps, |entry, row| {
                choose_add_second(entry, row, elapsed_ns);
            }) {
                return false;
            }
        }
        true
    }

    /// Adds `rows`, Apps in key order, to the entries with `add`. Returns false where they come
    /// to more than `APPS_MAX` Apps.
    fn choose_add<R: PointRow + Rankable>(
        &mut self,
        rows: &[R],
        add: impl Fn(&mut Entry, &R),
    ) -> bool {
        self.spare.clear();
        let fits = choose_add_merged(&mut self.entries, rows, &mut self.spare, add);
        std::mem::swap(&mut self.entries, &mut self.spare);
        fits
    }

    /// Writes into `cohorts` the leaders of each chart among the entries.
    fn choose_cohorts(&mut self, cohorts: &mut Cohorts) {
        for (place, metric) in METRICS.into_iter().enumerate() {
            choose_leaders(
                &self.entries,
                &mut self.chosen,
                |entry| entry.means[place].elapsed_ns > 0,
                |before, after| {
                    compare(Comparison {
                        before: &before.means[place],
                        after: &after.means[place],
                    })
                },
            );
            choose_cohort(&self.entries, &self.chosen, cohorts.of_mut(metric));
        }
        choose_leaders(
            &self.entries,
            &mut self.chosen,
            |entry| entry.resident.seconds > 0,
            |before, after| choose_resident_compare(&before.resident, &after.resident),
        );
        choose_cohort(&self.entries, &self.chosen, &mut cohorts.resident);
    }
}

/// The indexes of the newest seconds of `seconds` that lie in `window`, those after the newest
/// second the clock was set back past, which lie before them in the span but after them in
/// time.
fn choose_run(seconds: &[Arc<MachineSecond>], window: WallSeconds) -> Range<usize> {
    let mut newer: Option<u64> = None;
    let mut end = seconds.len();
    let mut start = seconds.len();
    for (index, second) in seconds.iter().enumerate().rev() {
        // Two ticks can fall in one wall second, so only a later wall second is a set back.
        let wall_second = second.wall_ns / NANOSECONDS_PER_SECOND;
        if newer.is_some_and(|newer| wall_second > newer) {
            break;
        }
        if wall_second < window.first {
            break;
        }

        newer = Some(wall_second);
        if wall_second > window.last {
            end = index;
        }
        start = index;
    }

    assert!(start <= end);
    start..end
}

/// The indexes of the closed `stretches` that lie wholly in `window`, after the newest
/// stretch the clock was set back past. `open` is the stretch of the newest second, which no
/// closed stretch follows.
fn choose_stretches(
    stretches: &[Arc<AppsStretch>],
    open: u64,
    window: WallSeconds,
) -> Range<usize> {
    let mut newer = open;
    let mut end = stretches.len();
    let mut start = stretches.len();
    for (index, stretch) in stretches.iter().enumerate().rev() {
        // The store closes a stretch only at a second of another stretch.
        assert_ne!(stretch.stretch, newer);
        if stretch.stretch > newer {
            break;
        }
        let first = stretch.stretch * EARLIER_STRIDE_SECONDS;
        if first < window.first {
            break;
        }

        newer = stretch.stretch;
        if first + EARLIER_STRIDE_SECONDS - 1 > window.last {
            end = index;
        }
        start = index;
    }

    assert!(start <= end);
    start..end
}

/// Adds `row`, an App's readings at a second that measured its Apps over `elapsed_ns`.
fn choose_add_second(entry: &mut Entry, row: &AppRow, elapsed_ns: Option<u64>) {
    if let Some(elapsed_ns) = elapsed_ns {
        for (place, metric) in METRICS.into_iter().enumerate() {
            if let Some(amount) = row.measure(ranked_metric(metric)).measured() {
                entry.means[place].amount += u128::from(amount);
                entry.means[place].elapsed_ns += u128::from(elapsed_ns);
            }
        }
    }
    if let Some(bytes) = row.measure(Metric::Resident).measured() {
        entry.resident.bytes += u128::from(bytes);
        entry.resident.seconds += 1;
    }
}

/// Adds `row`, an App's readings added up over a stretch.
fn choose_add_stretch(entry: &mut Entry, row: &StretchRow) {
    for (place, metric) in METRICS.into_iter().enumerate() {
        if let Some((amount, elapsed_ns)) = row.rate_measured(ranked_metric(metric)) {
            entry.means[place].amount += u128::from(amount);
            entry.means[place].elapsed_ns += u128::from(elapsed_ns);
        }
    }
    if let Some((bytes, seconds)) = row.resident_measured() {
        entry.resident.bytes += u128::from(bytes);
        entry.resident.seconds += seconds;
    }
}

/// Writes into `merged` the entries of `entries` with `rows` added with `add`, in key order.
/// Returns false where they come to more than `APPS_MAX` Apps.
fn choose_add_merged<R: PointRow + Rankable>(
    entries: &mut Vec<Entry>,
    rows: &[R],
    merged: &mut Vec<Entry>,
    add: impl Fn(&mut Entry, &R),
) -> bool {
    assert!(merged.is_empty());

    let mut held = entries.drain(..).peekable();
    for row in rows {
        let key = key_of(row.name());
        while let Some(entry) = held.next_if(|entry| key_of(&entry.name) < key) {
            if merged.len() == APPS_MAX {
                return false;
            }
            merged.push(entry);
        }
        // An App out of key order would take a second entry of its own.
        if let Some(last) = merged.last() {
            assert!(key_of(&last.name) < key);
        }
        let mut entry = match held.next_if(|entry| key_of(&entry.name) == key) {
            Some(entry) => entry,
            None => Entry {
                name: row.ranked(),
                means: [Mean::ZERO; 3],
                resident: Gauge::ZERO,
            },
        };

        add(&mut entry, row);
        if merged.len() == APPS_MAX {
            return false;
        }
        merged.push(entry);
    }
    for entry in held {
        if merged.len() == APPS_MAX {
            return false;
        }
        merged.push(entry);
    }

    true
}

/// Writes into `chosen` the entries a chart draws, the most first: those with the highest mean,
/// as `compare` orders the means of two entries, ties going to the App first in key order, and
/// none whose seconds never measured the resource.
fn choose_leaders(
    entries: &[Entry],
    chosen: &mut Vec<usize>,
    measured: impl Fn(&Entry) -> bool,
    compare: impl Fn(&Entry, &Entry) -> Ordering,
) {
    chosen.clear();
    for (index, entry) in entries.iter().enumerate() {
        if !measured(entry) {
            continue;
        }

        let position = chosen.partition_point(|other| {
            let other = &entries[*other];
            compare(other, entry)
                .reverse()
                .then_with(|| key_of(&other.name).cmp(&key_of(&entry.name)))
                != Ordering::Greater
        });
        if position < COHORT_MAX {
            if chosen.len() == COHORT_MAX {
                chosen.truncate(COHORT_MAX - 1);
            }
            chosen.insert(position, index);
        }
    }
    assert!(chosen.len() <= COHORT_MAX);
}

/// How the mean of `before` compares with that of `after`, exactly, since neither cross product
/// passes a `u128`: a mean adds up a `u64` over at most the seconds a View holds.
fn choose_resident_compare(before: &Gauge, after: &Gauge) -> Ordering {
    let before_scaled = before.bytes * u128::from(after.seconds);
    let after_scaled = after.bytes * u128::from(before.seconds);

    before_scaled.cmp(&after_scaled)
}

/// Writes into `cohort` the Apps of `entries` that `chosen` names, in its order.
fn choose_cohort(entries: &[Entry], chosen: &[usize], cohort: &mut Vec<Arc<RankedApp>>) {
    let capacity = cohort.capacity();
    cohort.extend(chosen.iter().map(|index| Arc::clone(&entries[*index].name)));
    assert_eq!(cohort.capacity(), capacity);
}

fn key_of(name: &RankedApp) -> AppKey<'_> {
    name.kind.key(&name.bytes)
}
