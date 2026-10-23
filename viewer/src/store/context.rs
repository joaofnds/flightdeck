//! What each App used over the newest seconds Overview's App charts draw, added up as a second
//! enters them and taken away as it leaves, so the store chooses each chart's Apps at every
//! step without reading those seconds again.

use std::cmp::Ordering;
use std::sync::Arc;

use super::attribution::{COHORT_MAX, METRICS, ranked_metric};
use super::buffers::Span;
use super::{AppsSecond, MachineSecond};
use crate::app_attribution_history::AppAttributionMetric;
use crate::app_attribution_history::means::{Comparison, Mean, compare};
use crate::apps::AppKey;
use crate::live_app_response::{Rankable, RankedApp};
use crate::session::CHARTED_SECONDS;

/// The Apps the context holds at most, as the worker's App histories hold them, past which it
/// chooses no Apps.
const APPS_MAX: usize = 24_000;

/// The seconds a context that held too many Apps takes in before it adds its seconds up again,
/// which bounds what adding them up costs while the seconds hold that many.
const REBUILD_SECONDS: u32 = 60;

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

const _: () = {
    assert!(APPS_MAX == crate::live_app_response::APPS_MAX);
    // An entry counts the context's seconds that hold its App in a `u32`.
    assert!(super::SECONDS_MAX < 4_294_967_295);
};

/// The Apps each of Overview's App charts draws on a line of its own, chosen over the seconds
/// the charts draw, as the worker's App histories choose their cohorts.
#[derive(Debug, Default, PartialEq)]
pub struct Cohorts {
    /// How many of the View's newest seconds the charts draw: those of the newest
    /// `CHARTED_SECONDS` wall seconds.
    pub seconds: usize,
    /// The Apps that used the most CPU over those seconds, at most eight, the most first.
    pub cpu: Vec<Arc<RankedApp>>,
    /// The Apps that used the most energy over those seconds, at most eight, the most first.
    pub power: Vec<Arc<RankedApp>>,
    /// The Apps that read and wrote the most bytes over those seconds, at most eight, the most
    /// first.
    pub disk: Vec<Arc<RankedApp>>,
    /// Whether those seconds hold more Apps than the store adds up, so it chose none.
    pub refused: bool,
}

impl Cohorts {
    /// Cohorts with room for the Apps every chart draws, holding none.
    pub(super) fn with_room() -> Self {
        Self {
            seconds: 0,
            cpu: Vec::with_capacity(COHORT_MAX),
            power: Vec::with_capacity(COHORT_MAX),
            disk: Vec::with_capacity(COHORT_MAX),
            refused: false,
        }
    }

    /// Holds no Apps, keeping the room for them.
    pub(super) fn clear(&mut self) {
        self.seconds = 0;
        self.cpu.clear();
        self.power.clear();
        self.disk.clear();
        self.refused = false;
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

/// The Apps of the newest seconds of the store's live span that the App charts draw, and what
/// each used of each resource over them.
pub(super) struct Context {
    /// Every App the context's seconds hold, in key order.
    entries: Vec<Entry>,
    /// Where adding a second writes the entries, which then trade places with `entries`.
    spare: Vec<Entry>,
    /// How many of the span's newest seconds the context holds.
    held: usize,
    /// The wall second of the newest second the context took in.
    last_second: Option<u64>,
    /// The seconds the context takes in before it adds its seconds up again, where they hold
    /// more Apps than it holds, and `None` where it holds every App of its seconds.
    refused: Option<u32>,
    /// The entries a chart draws, the most first, as `choose` finds them.
    chosen: Vec<usize>,
}

/// One App of the context's seconds.
struct Entry {
    name: Arc<RankedApp>,
    /// How many of the context's seconds hold the App.
    seconds: u32,
    /// What the App used of each of `METRICS` over the seconds that measured it, in their order.
    means: [Mean; 3],
}

impl Context {
    /// A context with room for every App it holds, holding none.
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::with_capacity(APPS_MAX),
            spare: Vec::with_capacity(APPS_MAX),
            held: 0,
            last_second: None,
            refused: None,
            chosen: Vec::with_capacity(COHORT_MAX),
        }
    }

    /// Lets go of the second `apps` lets go of at its next push, where the context holds it.
    pub(super) fn leave(&mut self, apps: &Span<AppsSecond>) {
        let Some(oldest) = apps.leaving() else {
            return;
        };
        if self.held < apps.len() {
            return;
        }

        self.exit(oldest);
        self.held -= 1;
    }

    /// Takes in the newest second of the span, `seconds` and `apps`, and lets go of the oldest
    /// seconds that lie before the newest `CHARTED_SECONDS` wall seconds.
    pub(super) fn follow(&mut self, seconds: &Span<MachineSecond>, apps: &Span<AppsSecond>) {
        assert_eq!(seconds.len(), apps.len());
        assert!(self.held < apps.len());
        let (Some(newest), Some(newest_apps)) = (seconds.newest(), apps.newest()) else {
            unreachable!("the context follows a second the span took in");
        };

        let second = newest.wall_ns / NANOSECONDS_PER_SECOND;
        // A clock set back leaves the seconds before it after the newest, outside the newest
        // wall seconds, so the context starts again at the newest.
        if self.last_second.is_some_and(|last| second < last) {
            self.entries.clear();
            self.held = 0;
        }
        self.enter(newest_apps);
        self.held += 1;
        self.last_second = Some(second);

        let first = second.saturating_sub(CHARTED_SECONDS - 1);
        for _ in 0..self.held {
            let index = apps.len() - self.held;
            let (Some(oldest), Some(oldest_apps)) = (seconds.get(index), apps.get(index)) else {
                unreachable!("a context holds only seconds of the span");
            };
            if oldest.wall_ns / NANOSECONDS_PER_SECOND >= first {
                break;
            }

            self.exit(oldest_apps);
            self.held -= 1;
        }
        self.follow_rebuild(apps);
    }

    /// Adds the context's seconds of `apps` up again where it held too many Apps and has taken
    /// in `REBUILD_SECONDS` seconds since.
    fn follow_rebuild(&mut self, apps: &Span<AppsSecond>) {
        let Some(waiting) = self.refused else {
            return;
        };
        if waiting > 0 {
            self.refused = Some(waiting - 1);
            return;
        }

        self.refused = None;
        self.entries.clear();
        for index in apps.len() - self.held..apps.len() {
            let Some(second) = apps.get(index) else {
                unreachable!("a context holds only seconds of the span");
            };
            self.enter(second);
            if self.refused.is_some() {
                return;
            }
        }
    }

    /// Writes into `cohorts` the Apps each chart draws, those whose seconds measured the most of
    /// its resource on average, ties going to the App first in key order, as the worker's App
    /// histories choose them.
    pub(super) fn choose(&mut self, cohorts: &mut Cohorts) {
        assert_eq!(cohorts.seconds, 0);
        assert!(cohorts.cpu.is_empty());
        assert!(cohorts.power.is_empty());
        assert!(cohorts.disk.is_empty());

        cohorts.seconds = self.held;
        if self.refused.is_some() {
            cohorts.refused = true;
            return;
        }
        for (place, metric) in METRICS.into_iter().enumerate() {
            choose_leaders(&self.entries, place, &mut self.chosen);
            let cohort = cohorts.of_mut(metric);
            let capacity = cohort.capacity();
            cohort.extend(
                self.chosen
                    .iter()
                    .map(|index| Arc::clone(&self.entries[*index].name)),
            );
            assert_eq!(cohort.capacity(), capacity);
        }
    }

    /// Adds `second`'s Apps to what the context holds, or holds none and waits to add its
    /// seconds up again where it holds too many Apps to take them in.
    fn enter(&mut self, second: &AppsSecond) {
        if self.refused.is_some() {
            return;
        }

        self.spare.clear();
        let fits = enter_merge(&mut self.entries, second, &mut self.spare);
        std::mem::swap(&mut self.entries, &mut self.spare);
        if !fits {
            self.entries.clear();
            self.refused = Some(REBUILD_SECONDS);
        }
    }

    /// Takes `second`'s Apps away from what the context holds, letting go of each App no other
    /// second of the context holds.
    fn exit(&mut self, second: &AppsSecond) {
        if self.refused.is_some() {
            return;
        }

        let elapsed_ns = measured_elapsed(second);
        let mut index = 0;
        for row in &second.apps {
            let key = key_of(&row.name);
            index += self.entries[index..].partition_point(|entry| key_of(&entry.name) < key);
            let Some(entry) = self.entries.get_mut(index) else {
                unreachable!("the context holds every App of its seconds");
            };
            assert_eq!(key_of(&entry.name), key);

            entry.seconds -= 1;
            for (place, metric) in METRICS.into_iter().enumerate() {
                if let Some((amount, elapsed_ns)) = measured(row, metric, elapsed_ns) {
                    entry.means[place].amount -= amount;
                    entry.means[place].elapsed_ns -= elapsed_ns;
                }
            }
        }

        self.entries.retain(|entry| {
            if entry.seconds == 0 {
                for mean in &entry.means {
                    assert_eq!(mean.amount, 0);
                    assert_eq!(mean.elapsed_ns, 0);
                }
            }
            entry.seconds > 0
        });
    }
}

/// Writes into `merged` the entries of `entries` with `second`'s Apps added, in key order.
/// Returns false where they come to more than `APPS_MAX` Apps.
fn enter_merge(entries: &mut Vec<Entry>, second: &AppsSecond, merged: &mut Vec<Entry>) -> bool {
    assert!(merged.is_empty());

    let elapsed_ns = measured_elapsed(second);
    let mut held = entries.drain(..).peekable();
    for row in &second.apps {
        let key = key_of(&row.name);
        while let Some(entry) = held.next_if(|entry| key_of(&entry.name) < key) {
            merged.push(entry);
        }
        let mut entry = match held.next_if(|entry| key_of(&entry.name) == key) {
            Some(entry) => entry,
            None => Entry {
                name: Arc::clone(&row.name),
                seconds: 0,
                means: [Mean::ZERO; 3],
            },
        };

        entry.seconds += 1;
        for (place, metric) in METRICS.into_iter().enumerate() {
            if let Some((amount, elapsed_ns)) = measured(row, metric, elapsed_ns) {
                entry.means[place].amount += amount;
                entry.means[place].elapsed_ns += elapsed_ns;
            }
        }
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

/// Writes into `chosen` the entries a chart of the resource at `place` among `METRICS` draws,
/// the most first: those with the highest mean, ties going to the App first in key order, and
/// none whose seconds never measured the resource.
fn choose_leaders(entries: &[Entry], place: usize, chosen: &mut Vec<usize>) {
    chosen.clear();
    for (index, entry) in entries.iter().enumerate() {
        let mean = &entry.means[place];
        if mean.elapsed_ns == 0 {
            continue;
        }

        let position = chosen.partition_point(|other| {
            let other = &entries[*other];
            compare(Comparison {
                before: &other.means[place],
                after: mean,
            })
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

/// The interval `second`'s Apps were measured over, where it measured them over one, as the
/// worker's means count only Apps measured over an interval.
fn measured_elapsed(second: &AppsSecond) -> Option<u64> {
    second.elapsed_ns.filter(|elapsed_ns| *elapsed_ns > 0)
}

/// What `row` used of `metric` and the interval it was measured over, where the second measured
/// it over `elapsed_ns`.
fn measured(
    row: &super::AppRow,
    metric: AppAttributionMetric,
    elapsed_ns: Option<u64>,
) -> Option<(u128, u128)> {
    let elapsed_ns = elapsed_ns?;
    let amount = row.measure(ranked_metric(metric)).measured()?;

    Some((u128::from(amount), u128::from(elapsed_ns)))
}

fn key_of(name: &RankedApp) -> AppKey<'_> {
    name.kind.key(&name.bytes)
}
