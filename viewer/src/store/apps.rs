//! Each App's totals at one second of the live span, as the App rule derives them from the
//! ring's tick or a sealed minute keeps them, ranked by the rule that ranks a sample's Apps.

use std::cmp::Ordering;
use std::fmt;
use std::mem;
use std::sync::Arc;

use collector::ring::RunIdentity;

use super::attribution::Totals;
use crate::apps::Stamp;
use crate::live_app_history::CompactMeasure;
use crate::live_app_response::{
    App, KeyKind, Measure, Metric, Observation, Rankable, Ranked, RankedApp, Ranks, Sample,
    rank_apps,
};
use crate::recording::{self, AppMinute, SampleRow};

#[cfg(test)]
#[path = "apps_tests.rs"]
mod tests;

/// The Apps one second holds at most. The App rule finds about 470 Apps a tick on the user's
/// M5 Pro, so a second past this bound holds none and says so.
pub const APPS_PER_SECOND_MAX: usize = 2_048;

/// Each App's totals at one second, and what the App rule found of the second as a whole.
#[derive(Debug, PartialEq)]
pub struct AppsSecond {
    /// Why the second holds no Apps, or `None` where it holds every App of its tick.
    pub missing: Option<Missing>,
    /// The tick the second shows, which the store names for every second it takes in, holding
    /// the tick's Apps or not, so a chart places each second as the worker's App histories do.
    /// `None` only in a buffer the store has not filled.
    pub observation: Option<Observation>,
    /// Whether `observation` is the tick after the tick of the second the store took in before
    /// this one, by the rule at which the worker's App histories break a chart's line.
    pub consecutive: bool,
    pub elapsed_ns: Option<u64>,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    /// The ungrouped members live at the tick, which the worker's history of resident memory
    /// counts, since an exited process holds no memory.
    pub live_ungrouped_members: u32,
    pub usage_refused: u32,
    pub usage_gone: u32,
    /// Every App of the tick, once each, in key order.
    pub apps: Vec<AppRow>,
    /// Every App of `apps` added up, as `Totals::of` adds them, so a chart finds what the Apps
    /// it does not draw on lines of their own used without adding them up.
    pub totals: Totals,
}

/// Why a second holds no Apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Missing {
    /// The second was read from a sealed minute older than the live span, or one that keeps no
    /// Apps this build reads, so the store did not read its Apps.
    Unread,
    /// The App rule refused the tick, for this reason.
    Refused(String),
    /// The tick has more Apps than a second holds.
    TooMany,
}

/// What the store found of a tick's Apps as it takes the tick's second in.
pub(super) enum TickApps<'tick> {
    /// Why the store holds none of the tick's Apps.
    Missing(&'tick Missing),
    /// The App rule's sample of the tick.
    Derived(&'tick Sample),
    /// Why the App rule refused the tick.
    Refused(&'tick str),
    /// The Apps a loader read from the tick's sealed minute, which the store holds in place of
    /// the buffer it takes them into.
    Loaded(&'tick mut AppsSecond),
}

/// One observation's Apps as its sealed minute keeps them.
pub(super) struct Kept<'minute> {
    pub(super) run: &'minute RunIdentity,
    pub(super) sample: &'minute SampleRow,
    /// The observation's App rows, in key order.
    pub(super) rows: &'minute [recording::AppRow],
    pub(super) minute: &'minute AppMinute,
}

/// One App's totals at one second.
#[derive(Clone, Debug, PartialEq)]
pub struct AppRow {
    /// The App's key and name, one allocation for every second that names the App.
    pub name: Arc<RankedApp>,
    pub members: u32,
    pub membership_complete: bool,
    cpu_ns: CompactMeasure,
    energy_nj: CompactMeasure,
    gpu_ns: CompactMeasure,
    disk_io_bytes: CompactMeasure,
    resident_bytes: CompactMeasure,
}

const _: () = {
    assert!(size_of::<AppRow>() <= 80);
};

impl AppsSecond {
    /// A second with room for `APPS_PER_SECOND_MAX` Apps, holding none.
    pub(super) fn with_room() -> Self {
        Self {
            apps: Vec::with_capacity(APPS_PER_SECOND_MAX),
            ..Self::default()
        }
    }

    /// Holds what the store found of a tick's Apps, `apps`. An App the second `previous` holds
    /// keeps the name it holds there.
    pub(super) fn take(&mut self, apps: TickApps<'_>, previous: Option<&Self>) {
        match apps {
            TickApps::Missing(missing) => self.clear(missing.clone()),
            TickApps::Derived(sample) => self.fill(sample, previous),
            TickApps::Refused(error) => self.clear(Missing::Refused(error.to_owned())),
            TickApps::Loaded(loaded) => {
                mem::swap(self, loaded);
                assert!(self.apps.capacity() >= APPS_PER_SECOND_MAX);
                self.take_names(previous);
            }
        }
    }

    /// Gives each App that `previous` holds the name it holds there, so a second a loader read
    /// shares its names with the seconds the store took in before it.
    fn take_names(&mut self, previous: Option<&Self>) {
        let previous = previous.map_or(&[][..], |previous| previous.apps.as_slice());
        let mut previous_index = 0;
        for row in &mut self.apps {
            let name = &row.name;
            if let Some(held) = previous_name(name.kind, &name.bytes, previous, &mut previous_index)
            {
                row.name = held;
            }
        }
    }

    /// Holds the Apps of the observation `kept`. An App that `previous` holds keeps the name it
    /// holds there, so a loader keeps one name for every second of an App it reads.
    pub(super) fn keep(&mut self, kept: &Kept<'_>, previous: Option<&Self>) {
        if kept.rows.len() > APPS_PER_SECOND_MAX {
            self.clear(Missing::TooMany);
            return;
        }

        let capacity = self.apps.capacity();
        let previous = previous.map_or(&[][..], |previous| previous.apps.as_slice());
        self.apps.clear();
        let mut previous_index = 0;
        for row in kept.rows {
            let bytes = kept.minute.key(row);
            if let Some(last) = self.apps.last() {
                // A minute is read from disk, so it may hold an App twice or out of order.
                if last.name.kind.key(&last.name.bytes) >= row.kind.key(bytes) {
                    self.clear(Missing::Refused(String::from(
                        "the sealed minute holds its Apps out of key order",
                    )));
                    return;
                }
            }
            let name = match previous_name(row.kind, bytes, previous, &mut previous_index) {
                Some(name) => name,
                None => match RankedApp::new(row.kind, bytes.to_vec()) {
                    Ok(name) => Arc::new(name),
                    Err(error) => {
                        self.clear(Missing::Refused(error.to_string()));
                        return;
                    }
                },
            };
            self.apps.push(AppRow::kept(row, name));
        }
        assert_eq!(self.apps.capacity(), capacity);

        let sample = kept.sample;
        self.missing = None;
        self.observation = Some(keep_observation(kept.run, sample));
        self.elapsed_ns = sample.elapsed_ns;
        self.membership_complete = sample.membership_complete;
        self.ungrouped_members = sample.ungrouped_members;
        self.live_ungrouped_members = sample.live_ungrouped_members;
        self.usage_refused = sample.usage_refused;
        self.usage_gone = sample.usage_gone;
        self.totals = Totals::of(&self.apps);
    }

    /// Holds no Apps, for `missing`.
    fn clear(&mut self, missing: Missing) {
        self.missing = Some(missing);
        self.observation = None;
        self.elapsed_ns = None;
        self.membership_complete = false;
        self.ungrouped_members = 0;
        self.live_ungrouped_members = 0;
        self.usage_refused = 0;
        self.usage_gone = 0;
        self.apps.clear();
        self.totals = Totals::default();
    }

    /// Holds each App of `sample`. An App that `previous` holds keeps the name it holds there, so
    /// the store keeps one name for every second of an App.
    fn fill(&mut self, sample: &Sample, previous: Option<&Self>) {
        let apps = sample.apps();
        if apps.len() > APPS_PER_SECOND_MAX {
            self.clear(Missing::TooMany);
            return;
        }

        let capacity = self.apps.capacity();
        let previous = previous.map_or(&[][..], |previous| previous.apps.as_slice());
        self.apps.clear();
        let mut previous_index = 0;
        for app in apps {
            if let Some(last) = self.apps.last() {
                // The App rule finds each App by its key among a sample's Apps.
                assert!(last.name.kind.key(&last.name.bytes) < app.key.kind.key(&app.key.bytes));
            }
            let name = previous_name(app.key.kind, &app.key.bytes, previous, &mut previous_index)
                .unwrap_or_else(|| app.ranked());
            self.apps.push(AppRow::new(app, name));
        }
        assert_eq!(self.apps.capacity(), capacity);

        self.missing = None;
        self.observation = Some(sample.selected);
        self.elapsed_ns = sample.elapsed_ns;
        self.membership_complete = sample.membership_complete;
        self.ungrouped_members = sample.ungrouped_members;
        self.live_ungrouped_members = sample.live_ungrouped_members;
        self.usage_refused = sample.usage_refused;
        self.usage_gone = sample.usage_gone;
        self.totals = Totals::of(&self.apps);
    }

    /// The Apps that used the most of each resource at this second, ranked by the rule that
    /// ranks a sample's Apps, or why the second holds no Apps to rank.
    pub fn ranks(&self) -> Result<Ranks, Missing> {
        if let Some(missing) = &self.missing {
            return Err(missing.clone());
        }
        let Some(observation) = self.observation else {
            unreachable!("a second that holds its Apps holds their tick");
        };

        let ranked = Ranked {
            selected: observation,
            elapsed_ns: self.elapsed_ns,
            membership_complete: self.membership_complete,
            ungrouped_members: self.ungrouped_members,
        };
        rank_apps(&ranked, &self.apps).map_err(|error| Missing::Refused(error.to_string()))
    }
}

/// The name `previous` holds for the App of key `kind` and `bytes`, where it holds the App.
/// `previous` is in key order, as a sample's Apps are, and `previous_index` is where the walk
/// through it stands.
fn previous_name(
    kind: KeyKind,
    bytes: &[u8],
    previous: &[AppRow],
    previous_index: &mut usize,
) -> Option<Arc<RankedApp>> {
    let key = kind.key(bytes);

    for _ in 0..=previous.len() {
        let row = previous.get(*previous_index)?;

        match row.name.kind.key(&row.name.bytes).cmp(&key) {
            Ordering::Less => *previous_index += 1,
            Ordering::Equal => return Some(Arc::clone(&row.name)),
            Ordering::Greater => return None,
        }
    }

    None
}

/// The observation of `sample`, of run `run`, as the App rule names a tick it samples.
fn keep_observation(run: &RunIdentity, sample: &SampleRow) -> Observation {
    Observation {
        recording: run.recording.0,
        stamp: Stamp {
            boot: run.boot.0,
            run: run.run.0,
            publication: sample.publication,
            continuous_ns: sample.continuous_ns,
            awake_ns: sample.awake_ns,
        },
        run_tick: sample.run_tick,
        wall_ns: sample.wall_ns,
    }
}

impl Default for AppsSecond {
    /// A second that holds no Apps, since it was read from a sealed minute, and has no room for
    /// any.
    fn default() -> Self {
        Self {
            missing: Some(Missing::Unread),
            observation: None,
            consecutive: false,
            elapsed_ns: None,
            membership_complete: false,
            ungrouped_members: 0,
            live_ungrouped_members: 0,
            usage_refused: 0,
            usage_gone: 0,
            apps: Vec::new(),
            totals: Totals::default(),
        }
    }
}

impl fmt::Display for Missing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Missing::Unread => formatter.write_str("this second's Apps are not read yet"),
            Missing::Refused(error) => formatter.write_str(error),
            Missing::TooMany => write!(
                formatter,
                "this second holds more than {APPS_PER_SECOND_MAX} Apps"
            ),
        }
    }
}

impl AppRow {
    /// The totals of `app`, under `name`.
    #[must_use]
    pub fn new(app: &App, name: Arc<RankedApp>) -> Self {
        assert_eq!(name.kind, app.key.kind);
        assert_eq!(name.bytes, app.key.bytes);

        Self {
            name,
            members: app.members,
            membership_complete: app.membership_complete,
            cpu_ns: CompactMeasure::capture(&app.cpu_ns),
            energy_nj: CompactMeasure::capture(&app.energy_nj),
            gpu_ns: CompactMeasure::capture(&app.gpu_ns),
            disk_io_bytes: CompactMeasure::capture(&app.disk_io_bytes),
            resident_bytes: CompactMeasure::capture(&app.resident_bytes),
        }
    }

    /// The totals of `row`, under `name`.
    fn kept(row: &recording::AppRow, name: Arc<RankedApp>) -> Self {
        assert_eq!(name.kind, row.kind);

        Self {
            name,
            members: row.members,
            membership_complete: row.membership_complete,
            cpu_ns: CompactMeasure::capture(&row.cpu_ns),
            energy_nj: CompactMeasure::capture(&row.energy_nj),
            gpu_ns: CompactMeasure::capture(&row.gpu_ns),
            disk_io_bytes: CompactMeasure::capture(&row.disk_io_bytes),
            resident_bytes: CompactMeasure::capture(&row.resident_bytes),
        }
    }
}

impl Rankable for AppRow {
    fn measure(&self, metric: Metric) -> Measure {
        match metric {
            Metric::Cpu => self.cpu_ns.measure(),
            Metric::Resident => self.resident_bytes.measure(),
            Metric::Power => self.energy_nj.measure(),
            Metric::Disk => self.disk_io_bytes.measure(),
            Metric::Gpu => self.gpu_ns.measure(),
        }
    }

    fn ranked(&self) -> Arc<RankedApp> {
        Arc::clone(&self.name)
    }
}
