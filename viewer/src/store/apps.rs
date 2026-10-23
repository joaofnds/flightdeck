//! Each App's totals at one second of the live span, as the App rule derives them from the
//! ring's tick, ranked by the rule that ranks a sample's Apps.

use std::cmp::Ordering;
use std::sync::Arc;

use crate::live_app_history::CompactMeasure;
use crate::live_app_response::{
    App, Measure, Metric, Observation, Rankable, Ranked, RankedApp, Ranks, Sample, rank_apps,
};

/// The Apps one second holds at most. The App rule finds about 470 Apps a tick on the user's
/// M5 Pro, so a second past this bound holds none and says so.
pub const APPS_PER_SECOND_MAX: usize = 2_048;

/// Each App's totals at one second, and what the App rule found of the second as a whole.
pub struct AppsSecond {
    /// Why the second holds no Apps, or `None` where it holds every App of its tick.
    pub missing: Option<Missing>,
    /// The tick the Apps were derived from, `None` where the second holds no Apps.
    pub observation: Option<Observation>,
    pub elapsed_ns: Option<u64>,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    pub usage_refused: u32,
    pub usage_gone: u32,
    /// Every App of the tick, in key order.
    pub apps: Vec<AppRow>,
}

/// Why a second holds no Apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Missing {
    /// The second was read from a sealed minute, whose Apps the store does not read yet.
    Unread,
    /// The App rule refused the tick, for this reason.
    Refused(String),
    /// The tick has more Apps than a second holds.
    TooMany,
}

/// What the store found of a tick's Apps as it takes the tick's second in.
pub(super) enum TickApps<'sample> {
    /// The second was read from a sealed minute, whose Apps the store does not read yet.
    Unread,
    /// The App rule's sample of the tick.
    Derived(&'sample Sample),
    /// Why the App rule refused the tick.
    Refused(&'sample str),
}

/// One App's totals at one second.
#[derive(Clone)]
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
    pub(super) fn take(&mut self, apps: &TickApps<'_>, previous: Option<&Self>) {
        match apps {
            TickApps::Unread => self.clear(Missing::Unread),
            TickApps::Derived(sample) => self.fill(sample, previous),
            TickApps::Refused(error) => self.clear(Missing::Refused((*error).to_owned())),
        }
    }

    /// Holds no Apps, for `missing`.
    fn clear(&mut self, missing: Missing) {
        self.missing = Some(missing);
        self.observation = None;
        self.elapsed_ns = None;
        self.membership_complete = false;
        self.ungrouped_members = 0;
        self.usage_refused = 0;
        self.usage_gone = 0;
        self.apps.clear();
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
            let name = fill_name(app, previous, &mut previous_index);
            self.apps.push(AppRow {
                name,
                members: app.members,
                membership_complete: app.membership_complete,
                cpu_ns: CompactMeasure::capture(&app.cpu_ns),
                energy_nj: CompactMeasure::capture(&app.energy_nj),
                gpu_ns: CompactMeasure::capture(&app.gpu_ns),
                disk_io_bytes: CompactMeasure::capture(&app.disk_io_bytes),
                resident_bytes: CompactMeasure::capture(&app.resident_bytes),
            });
        }
        assert_eq!(self.apps.capacity(), capacity);

        self.missing = None;
        self.observation = Some(sample.selected);
        self.elapsed_ns = sample.elapsed_ns;
        self.membership_complete = sample.membership_complete;
        self.ungrouped_members = sample.ungrouped_members;
        self.usage_refused = sample.usage_refused;
        self.usage_gone = sample.usage_gone;
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

/// The name of `app`: the one `previous` holds for its key, or a new one. `previous` is in key
/// order, as a sample's Apps are, and `previous_index` is where the walk through it stands.
fn fill_name(app: &App, previous: &[AppRow], previous_index: &mut usize) -> Arc<RankedApp> {
    let key = app.key.kind.key(&app.key.bytes);

    for _ in 0..=previous.len() {
        let Some(row) = previous.get(*previous_index) else {
            break;
        };

        match row.name.kind.key(&row.name.bytes).cmp(&key) {
            Ordering::Less => *previous_index += 1,
            Ordering::Equal => return Arc::clone(&row.name),
            Ordering::Greater => break,
        }
    }

    app.ranked()
}

impl Default for AppsSecond {
    /// A second that holds no Apps, since it was read from a sealed minute, and has no room for
    /// any.
    fn default() -> Self {
        Self {
            missing: Some(Missing::Unread),
            observation: None,
            elapsed_ns: None,
            membership_complete: false,
            ungrouped_members: 0,
            usage_refused: 0,
            usage_gone: 0,
            apps: Vec::new(),
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
