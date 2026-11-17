//! Each App's totals at one second of the live span, as the App rule derives them from the
//! ring's tick or a sealed minute keeps them, ranked by the rule that ranks a sample's Apps.

use std::cmp::Ordering;
use std::fmt;
use std::mem;
use std::sync::Arc;

use collector::ring::RunIdentity;

use super::attribution::Totals;
use super::stretch::Folded;
use crate::apps::{AppKey, Stamp};
use crate::live_app_history::CompactMeasure;
use crate::live_app_response::{
    App, KeyKind, Measure, Metric, Observation, RankApps, Rankable, Ranked, RankedApp, Ranks,
    Sample, rank_apps,
};
use crate::recording::{self, AppMinute, SampleRow};

#[cfg(test)]
#[path = "apps_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod presentation_tests;

#[cfg(test)]
#[path = "app_identity_tests.rs"]
mod app_identity_tests;

#[path = "presentation.rs"]
mod presentation;

#[path = "app_rows.rs"]
mod app_rows;
pub use app_rows::AppRows;

/// The Apps one second holds at most. The App rule finds about 470 Apps a tick on the user's
/// M5 Pro, so a second past this bound holds none and says so.
pub const APPS_PER_SECOND_MAX: usize = 2_048;

/// Each App's totals at one second, and what the App rule found of the second as a whole.
#[derive(Debug, PartialEq)]
pub struct AppsSecond {
    /// Provenance and the admission for non-row observation metadata.
    pub metadata: super::native::AppMetadata,
    /// Complete process rows and additional App measures, or an explicit unavailable reason.
    pub processes: Result<Arc<super::Payload>, Missing>,
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
    pub apps: AppRows,
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
    /// The complete payload does not fit the caller's retained-byte budget.
    Budget,
}

/// What the store found of a tick's Apps as it takes the tick's second in.
pub(super) enum TickApps<'tick> {
    /// Why the store holds none of the tick's Apps.
    Missing(&'tick Missing),
    /// Why the App rule refused the tick.
    Refused(&'tick str),
    /// The Apps a loader read from the tick's sealed minute, which the store holds in place of
    /// the buffer it takes them into.
    Loaded(&'tick mut AppsSecond),
    /// The header of a second a launch takes before it installs the second's Apps, which the
    /// store holds as it holds `Loaded`, and whose block waits to be packed only once the launch
    /// has installed them or installs no more.
    Awaiting(&'tick mut AppsSecond),
    /// What a loader added up of the Apps of the tick and of the seconds of its stretch and run
    /// after it, which the store adds to the stretch and holds none of at the second.
    Folded(&'tick Folded),
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

/// The measures of an App row, each two words of a packed block.
const APP_MEASURE_COUNT: usize = 5;

/// The words of an App row a packed block holds: its members, whether its membership is
/// complete, and each measure's two words. Its name is the block's App and takes no word.
pub(in crate::store) const APP_ROW_WORDS: usize = 2 + 2 * APP_MEASURE_COUNT;

/// The word of an App row that holds its members, the first that `words` writes.
pub(in crate::store) const APP_ROW_WORD_MEMBERS: usize = 0;

impl AppRow {
    /// The row's fields as words, in the order `set_word` reads them. Every field
    /// is named, so a field added to the row is one a packed block has to hold.
    pub(in crate::store) fn words(&self) -> [u64; APP_ROW_WORDS] {
        let Self {
            name: _,
            members,
            membership_complete,
            cpu_ns,
            energy_nj,
            gpu_ns,
            disk_io_bytes,
            resident_bytes,
        } = self;
        let measures: [CompactMeasure; APP_MEASURE_COUNT] = [
            *cpu_ns,
            *energy_nj,
            *gpu_ns,
            *disk_io_bytes,
            *resident_bytes,
        ];
        let mut words = [0; APP_ROW_WORDS];
        words[0] = u64::from(*members);
        words[1] = u64::from(*membership_complete);
        for (index, measure) in measures.into_iter().enumerate() {
            let [known, coverage] = measure.words();
            words[2 + 2 * index] = known;
            words[3 + 2 * index] = coverage;
        }

        words
    }

    /// Sets every field of the row but its name to the field of `other`, so a row that keeps its
    /// name touches no reference count. Every field is named, so a field added to the row is
    /// one this copies.
    pub(in crate::store) fn copy_readings(&mut self, other: &Self) {
        let Self {
            name: _,
            members,
            membership_complete,
            cpu_ns,
            energy_nj,
            gpu_ns,
            disk_io_bytes,
            resident_bytes,
        } = other;
        self.members = *members;
        self.membership_complete = *membership_complete;
        self.cpu_ns = *cpu_ns;
        self.energy_nj = *energy_nj;
        self.gpu_ns = *gpu_ns;
        self.disk_io_bytes = *disk_io_bytes;
        self.resident_bytes = *resident_bytes;
    }

    /// Sets the field that word `index` of `words` holds to `word`.
    pub(in crate::store) fn set_word(&mut self, index: usize, word: u64) {
        assert!(index < APP_ROW_WORDS);
        match index {
            APP_ROW_WORD_MEMBERS => self.members = app_row_word_members(word),
            1 => self.membership_complete = word != 0,
            _ => {
                // Each measure takes two words, so the division floors to the measure the word
                // belongs to, and the remainder is the word's place within it.
                let measure = self.set_word_measure((index - 2) / 2);
                let mut words = measure.words();
                words[(index - 2) % 2] = word;
                *measure = CompactMeasure::of_words(words);
            }
        }
    }

    fn set_word_measure(&mut self, measure_index: usize) -> &mut CompactMeasure {
        match measure_index {
            0 => &mut self.cpu_ns,
            1 => &mut self.energy_nj,
            2 => &mut self.gpu_ns,
            3 => &mut self.disk_io_bytes,
            4 => &mut self.resident_bytes,
            _ => unreachable!("an App row holds {APP_MEASURE_COUNT} measures"),
        }
    }
}

/// The members count a word of an App row holds, which `words` wrote from a `u32`.
pub(in crate::store) fn app_row_word_members(word: u64) -> u32 {
    let Ok(members) = u32::try_from(word) else {
        unreachable!("an App row's members word holds a count of members");
    };

    members
}

impl AppsSecond {
    /// Whether the second equals `other` in everything but its own App rows, which a packed
    /// second leaves to its block.
    pub(super) fn header_eq(&self, other: &Self) -> bool {
        let Self {
            metadata,
            processes,
            missing,
            observation,
            consecutive,
            elapsed_ns,
            membership_complete,
            ungrouped_members,
            live_ungrouped_members,
            usage_refused,
            usage_gone,
            apps: _,
            totals,
        } = self;

        *metadata == other.metadata
            && *processes == other.processes
            && *missing == other.missing
            && *observation == other.observation
            && *consecutive == other.consecutive
            && *elapsed_ns == other.elapsed_ns
            && *membership_complete == other.membership_complete
            && *ungrouped_members == other.ungrouped_members
            && *live_ungrouped_members == other.live_ungrouped_members
            && *usage_refused == other.usage_refused
            && *usage_gone == other.usage_gone
            && *totals == other.totals
    }

    /// A second with room for `APPS_PER_SECOND_MAX` Apps, holding none.
    pub(super) fn with_room() -> Self {
        Self {
            apps: AppRows::with_capacity(APPS_PER_SECOND_MAX),
            ..Self::default()
        }
    }

    /// Holds what the store found of a tick's Apps, `apps`. An App the second `previous` holds
    /// keeps the name it holds there.
    pub(super) fn take(&mut self, apps: TickApps<'_>, previous: Option<&Self>) {
        match apps {
            TickApps::Missing(missing) => self.clear(missing.clone()),
            TickApps::Refused(error) => self.clear(Missing::Refused(error.to_owned())),
            TickApps::Folded(_) => self.clear(Missing::Unread),
            TickApps::Loaded(loaded) | TickApps::Awaiting(loaded) => {
                assert_eq!(
                    loaded.apps.reserved_bytes(),
                    loaded.apps.capacity() * size_of::<AppRow>()
                );
                mem::swap(self, loaded);
                self.take_names(previous);
            }
        }
    }

    /// Gives each App that `previous` holds the name it holds there, so a second a loader read
    /// shares its names with the seconds the store took in before it.
    fn take_names(&mut self, previous: Option<&Self>) {
        let previous = previous.map_or(&[][..], |previous| previous.apps.as_slice());
        let mut previous_index = 0;
        for row in self.apps.rows_mut() {
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
        self.metadata = super::native::AppMetadata::default();
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
        self.processes = Err(missing.clone());
        self.missing = Some(missing);
        self.observation = None;
        self.elapsed_ns = None;
        self.membership_complete = false;
        self.ungrouped_members = 0;
        self.live_ungrouped_members = 0;
        self.usage_refused = 0;
        self.usage_gone = 0;
        self.apps.clear();
        self.metadata = super::native::AppMetadata::default();
        self.totals = Totals::default();
    }

    /// Takes from `previous`, the second before, the name of each App it holds and each process
    /// row and App's additional measures that read the same there, as `fill` and the payload's
    /// capture take them when they are handed `previous`, for a second captured apart from the
    /// second before it.
    pub(in crate::store) fn share_with(&mut self, previous: &Self) {
        self.take_names(Some(previous));
        if let Ok(processes) = &mut self.processes {
            let Some(payload) = Arc::get_mut(processes) else {
                unreachable!("a second captured apart holds the only reference to its payload");
            };
            payload.share_with(self.apps.as_slice(), previous);
        }
    }

    /// Holds each App of `sample`. An App that `previous` holds keeps the name it holds there, so
    /// the store keeps one name for every second of an App.
    pub(super) fn fill(
        &mut self,
        sample: &Sample,
        previous: Option<&Self>,
        budget: &Arc<super::ByteBudget>,
    ) {
        let apps = sample.apps();
        if apps.len() > APPS_PER_SECOND_MAX {
            self.clear(Missing::TooMany);
            return;
        }

        let capacity = self.apps.capacity();
        let previous = previous.map_or(&[][..], |previous| previous.apps.as_slice());
        self.apps.clear();
        self.metadata = super::native::AppMetadata::default();
        let mut previous_index = 0;
        for app in apps {
            if let Some(last) = self.apps.last() {
                // The App rule finds each App by its key among a sample's Apps.
                assert!(last.name.kind.key(&last.name.bytes) < app.key.kind.key(&app.key.bytes));
            }
            let previous_name =
                previous_name(app.key.kind, &app.key.bytes, previous, &mut previous_index);
            let Some(name) = fill_name(app, previous_name, budget) else {
                self.clear(Missing::Budget);
                return;
            };
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
    pub(in crate::store) fn ranks(&self) -> Result<Ranks, Missing> {
        self.ranks_of(&self.apps)
    }

    /// The Apps of `apps`, the rows of this second's Apps, ranked as `ranks` ranks them.
    pub(in crate::store) fn ranks_of(&self, apps: &[AppRow]) -> Result<Ranks, Missing> {
        let ranked = self.ranks_ranked()?;

        rank_apps(&ranked, apps).map_err(|error| Missing::Refused(error.to_string()))
    }

    /// The `app_count` Apps that `add` hands to the ranking it is given, the Apps of this
    /// second in key order, ranked as `ranks` ranks them.
    pub(in crate::store) fn ranks_by(
        &self,
        app_count: usize,
        add: impl FnOnce(&mut RankApps),
    ) -> Result<Ranks, Missing> {
        let ranked = self.ranks_ranked()?;
        let mut ranking = RankApps::new(&ranked, app_count)
            .map_err(|error| Missing::Refused(error.to_string()))?;
        add(&mut ranking);

        Ok(ranking.ranks())
    }

    /// What the ranks of this second carry beside its Apps, or why the second holds no Apps to
    /// rank.
    fn ranks_ranked(&self) -> Result<Ranked, Missing> {
        if let Some(missing) = &self.missing {
            return Err(missing.clone());
        }
        let Some(observation) = self.observation else {
            unreachable!("a second that holds its Apps holds their tick");
        };

        Ok(Ranked {
            selected: observation,
            elapsed_ns: self.elapsed_ns,
            membership_complete: self.membership_complete,
            ungrouped_members: self.ungrouped_members,
        })
    }
}

/// The name the second holds for `app`: the one `previous` holds, or a copy charged to
/// `budget`, or `None` where the budget refuses it.
fn fill_name(
    app: &App,
    previous: Option<Arc<RankedApp>>,
    budget: &Arc<super::ByteBudget>,
) -> Option<Arc<RankedApp>> {
    match previous {
        Some(name) => {
            // Every name a second holds was charged when a second first held it, so sharing it
            // charges nothing more.
            assert!(name.reserved_bytes() > 0);
            Some(name)
        }
        None => RankedApp::captured(app, budget),
    }
}

/// The name `previous` holds for the App of key `kind` and `bytes`, where it holds the App.
fn previous_name(
    kind: KeyKind,
    bytes: &[u8],
    previous: &[AppRow],
    previous_index: &mut usize,
) -> Option<Arc<RankedApp>> {
    previous_app(kind, bytes, previous, previous_index)
        .map(|index| Arc::clone(&previous[index].name))
}

/// The index at which `previous` holds the App of key `kind` and `bytes`, where it holds the
/// App.
/// `previous` is in key order, as a sample's Apps are, and `previous_index` is where the walk
/// through it stands. The walk steps past an App it finds, since keys ascend strictly, so no
/// later key is that App's.
pub(super) fn previous_app(
    kind: KeyKind,
    bytes: &[u8],
    previous: &[AppRow],
    previous_index: &mut usize,
) -> Option<usize> {
    let key = kind.key(bytes);

    for _ in 0..=previous.len() {
        let row = previous.get(*previous_index)?;

        match row.name.kind.key(&row.name.bytes).cmp(&key) {
            Ordering::Less => *previous_index += 1,
            Ordering::Equal => {
                let found = *previous_index;
                *previous_index += 1;

                return Some(found);
            }
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
            metadata: super::native::AppMetadata::default(),
            processes: Err(Missing::Unread),
            missing: Some(Missing::Unread),
            observation: None,
            consecutive: false,
            elapsed_ns: None,
            membership_complete: false,
            ungrouped_members: 0,
            live_ungrouped_members: 0,
            usage_refused: 0,
            usage_gone: 0,
            apps: AppRows::default(),
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
            Missing::Budget => formatter.write_str("the complete payload exceeds its byte budget"),
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

    fn key(&self) -> AppKey<'_> {
        self.name.kind.key(&self.name.bytes)
    }

    fn ranked(&self) -> Arc<RankedApp> {
        Arc::clone(&self.name)
    }
}
