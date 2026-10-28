//! Each App's readings added up over one stretch of `EARLIER_STRIDE_SECONDS` wall seconds, so an
//! App chart of the loaded hour draws one point for each stretch that shows what its Apps used
//! over all of its seconds, where the older tail of a machine chart draws one reading of them.

use std::mem;
use std::sync::Arc;

use super::attribution::{self, Completeness, PointApps, PointRow, Totals};
use super::buffers::{Buffers, Span};
use super::{APPS_PER_SECOND_MAX, AppRow, AppsSecond, Attribution, Resident};
use crate::app_attribution_history::AppAttributionMetric;
use crate::apps::AppKey;
use crate::live_app_response::{Measure, Metric, Observation, Rankable, RankedApp};

/// The Apps a stretch holds at most, as many as a second holds, past which it holds none and
/// says so.
const STRETCH_APPS_MAX: usize = APPS_PER_SECOND_MAX;

/// The resources an App uses at a rate, in the order a stretch adds them up.
const RATES: [Metric; 4] = [Metric::Cpu, Metric::Power, Metric::Disk, Metric::Gpu];

const _: () = {
    assert!(size_of::<StretchRow>() <= 144);
};

/// The stretches of the seconds the store took in: the closed ones, oldest first, and the open
/// one, that of the newest second, which later seconds of the same stretch add to.
pub(super) struct Stretches {
    closed: Span<AppsStretch>,
    open: AppsStretch,
    /// Where adding a second writes the open stretch's Apps, which then trade places with them.
    spare: Vec<StretchRow>,
}

impl Stretches {
    /// Room for `count` closed stretches, over `count + held` buffers, since a View the main
    /// thread holds keeps stretches the store let go of.
    pub(super) fn new(count: usize, held: usize) -> Self {
        Self {
            closed: Span::new(count, Buffers::new(count + held, AppsStretch::with_room)),
            open: AppsStretch::with_room(),
            spare: Vec::with_capacity(STRETCH_APPS_MAX),
        }
    }

    /// Whether taking in a second of stretch `stretch` finds room: it adds to the open stretch,
    /// or closes it into a buffer no View holds.
    pub(super) fn has_room(&self, stretch: u64) -> bool {
        if self.closes(stretch) {
            self.closed.has_room()
        } else {
            true
        }
    }

    /// Adds `apps`, the Apps of a second of stretch `stretch`, closing the open stretch first
    /// where `apps` belongs to another.
    pub(super) fn take(&mut self, stretch: u64, apps: &AppsSecond) {
        if self.closes(stretch) {
            self.open.close();
            let open = &mut self.open;
            let pushed = self.closed.push(|buffer| mem::swap(buffer, open));
            assert!(pushed);
            // The buffer the closed stretch took the place of may hold a stretch let go of.
            self.open.clear();
        }

        self.open.add_second(stretch, apps, &mut self.spare);
    }

    /// The closed stretches, oldest first.
    pub(super) fn held(&self) -> impl ExactSizeIterator<Item = &Arc<AppsStretch>> {
        self.closed.held()
    }

    /// Clears each buffer no View holds, so a stretch let go of keeps no App's name alive.
    pub(super) fn release(&mut self) {
        self.closed.release(AppsStretch::clear);
    }

    pub(super) fn clear(&mut self) {
        self.closed.clear();
        self.release();
        self.open.clear();
    }

    fn closes(&self, stretch: u64) -> bool {
        if self.open.observation.is_none() {
            return false;
        }

        self.open.stretch != stretch
    }
}

/// Each App's readings added up over the seconds of one stretch the store took in, and what the
/// App rule found of those seconds as a whole.
#[derive(Debug, Default)]
pub struct AppsStretch {
    /// The stretch's wall seconds divided by `EARLIER_STRIDE_SECONDS`. The division floors.
    pub stretch: u64,
    /// The tick of the first second of the stretch the store took in, which places the
    /// stretch's point on a chart. `None` only in a stretch that holds no second.
    pub observation: Option<Observation>,
    /// The seconds of the stretch that hold their Apps.
    seconds: u32,
    /// The intervals those seconds measured their Apps over, added up, leaving out a second
    /// that measured them over none.
    elapsed_ns: u64,
    completeness: Completeness,
    /// Whether the stretch's seconds hold more Apps between them than a stretch holds, so it
    /// holds none.
    too_many: bool,
    /// Every App of the stretch's seconds, once each, in key order.
    rows: Vec<StretchRow>,
    /// Every App of `rows` added up, once the stretch is closed.
    totals: Totals,
}

/// One App's readings added up over the seconds of a stretch that hold it.
#[derive(Clone, Debug)]
pub(super) struct StretchRow {
    name: Arc<RankedApp>,
    /// Whether the App rule found every member of the App at each second that holds it.
    membership_complete: bool,
    /// What the App used of each of `RATES`, in their order.
    rates: [Amount; 4],
    /// The App's resident memory added up over the seconds that hold it.
    resident: Amount,
    /// The App's resident memory over the stretch's seconds that hold their Apps, once the
    /// stretch is closed, as a second without the App counts it as none.
    resident_bytes: u64,
}

/// What an App used of one resource, added up over some seconds.
#[derive(Clone, Copy, Debug, Default)]
struct Amount {
    /// What the App used over the seconds that measured it, added up.
    known: u64,
    /// The most members that measured the resource at any one of those seconds.
    measured_members: u32,
    /// The most members that did not measure it at any one second that holds the App.
    unavailable_members: u32,
    /// Whether any second's measure overflowed, or the sums did.
    overflowed: bool,
}

impl AppsStretch {
    /// A stretch with room for `STRETCH_APPS_MAX` Apps, holding none.
    fn with_room() -> Self {
        Self {
            rows: Vec::with_capacity(STRETCH_APPS_MAX),
            ..Self::default()
        }
    }

    /// Holds no second, keeping the room for Apps.
    fn clear(&mut self) {
        self.stretch = 0;
        self.observation = None;
        self.seconds = 0;
        self.elapsed_ns = 0;
        self.completeness = Completeness::default();
        self.too_many = false;
        self.rows.clear();
        self.totals = Totals::default();
    }

    /// Adds the Apps of `second`, a second of stretch `stretch`, writing the stretch's Apps into
    /// `spare`, which then trades places with them.
    fn add_second(&mut self, stretch: u64, second: &AppsSecond, spare: &mut Vec<StretchRow>) {
        if self.observation.is_none() {
            self.stretch = stretch;
            self.observation = second.observation;
        }
        assert_eq!(self.stretch, stretch);

        if second.missing.is_some() {
            return;
        }
        let completeness = PointApps::completeness(second);
        self.completeness = if self.seconds == 0 {
            completeness
        } else {
            add_completeness(self.completeness, completeness)
        };
        self.seconds += 1;
        let elapsed_ns = second.elapsed_ns.filter(|elapsed_ns| *elapsed_ns > 0);
        if let Some(elapsed_ns) = elapsed_ns {
            self.elapsed_ns += elapsed_ns;
        }
        if self.too_many {
            return;
        }

        let capacity = spare.capacity();
        spare.clear();
        let fits = add_second_rows(&mut self.rows, &second.apps, elapsed_ns, spare);
        mem::swap(&mut self.rows, spare);
        if !fits {
            self.rows.clear();
            self.too_many = true;
        }
        assert_eq!(spare.capacity(), capacity);
    }

    /// Finds each App's resident memory over the stretch, and adds the Apps up, once the store
    /// adds no more seconds to the stretch.
    fn close(&mut self) {
        let seconds = u64::from(self.seconds);
        for row in &mut self.rows {
            assert!(seconds > 0);
            // The division floors.
            row.resident_bytes = row.resident.known / seconds;
        }
        self.totals = Totals::of_rows(&self.rows);
    }

    /// What the stretch shows on the App chart of `metric` whose own lines draw `cohort`.
    #[must_use]
    pub fn attribution(
        &self,
        metric: AppAttributionMetric,
        cohort: &[Arc<RankedApp>],
    ) -> Attribution {
        attribution::attribution(self, metric, cohort)
    }

    /// What the stretch shows on the chart of resident memory by App whose own lines draw
    /// `cohort`.
    #[must_use]
    pub fn resident(&self, cohort: &[Arc<RankedApp>]) -> Resident {
        attribution::resident(self, cohort)
    }
}

/// What the App rule found of two sets of seconds together: every member only where it found
/// every member of each, and the most of each kind of process it missed at any one second.
fn add_completeness(stretch: Completeness, second: Completeness) -> Completeness {
    Completeness {
        membership_complete: stretch.membership_complete && second.membership_complete,
        ungrouped_members: stretch.ungrouped_members.max(second.ungrouped_members),
        live_ungrouped_members: stretch
            .live_ungrouped_members
            .max(second.live_ungrouped_members),
        usage_refused: stretch.usage_refused.max(second.usage_refused),
        usage_gone: stretch.usage_gone.max(second.usage_gone),
    }
}

/// Writes into `merged` the Apps of `rows` with the Apps of a second, `second`, added, in key
/// order, where the second measured them over `elapsed_ns`. Returns false where they come to
/// more than `STRETCH_APPS_MAX` Apps.
fn add_second_rows(
    rows: &mut Vec<StretchRow>,
    second: &[AppRow],
    elapsed_ns: Option<u64>,
    merged: &mut Vec<StretchRow>,
) -> bool {
    assert!(merged.is_empty());

    let mut held = rows.drain(..).peekable();
    for app in second {
        let key = key_of(&app.name);
        while let Some(row) = held.next_if(|row| key_of(&row.name) < key) {
            if merged.len() == STRETCH_APPS_MAX {
                return false;
            }
            merged.push(row);
        }
        let mut row = match held.next_if(|row| key_of(&row.name) == key) {
            Some(row) => row,
            None => StretchRow::new(Arc::clone(&app.name)),
        };

        row.add(app, elapsed_ns);
        if merged.len() == STRETCH_APPS_MAX {
            return false;
        }
        merged.push(row);
    }
    for row in held {
        if merged.len() == STRETCH_APPS_MAX {
            return false;
        }
        merged.push(row);
    }

    true
}

fn key_of(name: &RankedApp) -> AppKey<'_> {
    name.kind.key(&name.bytes)
}

impl StretchRow {
    /// The App `name` names, over no seconds yet.
    fn new(name: Arc<RankedApp>) -> Self {
        Self {
            name,
            membership_complete: true,
            rates: [Amount::default(); 4],
            resident: Amount::default(),
            resident_bytes: 0,
        }
    }

    /// Adds `app`, the App's readings at a second that measured its Apps over `elapsed_ns`. A
    /// second that measured them over no interval adds none of the App's use, as it has no rate.
    fn add(&mut self, app: &AppRow, elapsed_ns: Option<u64>) {
        assert_eq!(key_of(&self.name), key_of(&app.name));

        for (amount, metric) in self.rates.iter_mut().zip(RATES) {
            let measure = app.measure(metric);
            if elapsed_ns.is_some() {
                amount.add(&measure);
            } else {
                amount.add_members(&measure);
            }
        }
        self.resident.add(&app.measure(Metric::Resident));
        self.membership_complete &= app.membership_complete;
    }
}

impl Amount {
    /// Adds `measure`, a second's.
    fn add(&mut self, measure: &Measure) {
        self.add_members(measure);
        let Some(known) = measure.measured() else {
            return;
        };

        self.measured_members = self.measured_members.max(measure.measured_members);
        match self.known.checked_add(known) {
            Some(known) => self.known = known,
            None => self.overflowed = true,
        }
    }

    /// Adds of `measure`, a second's, only the members that did not measure the resource and
    /// whether it overflowed.
    fn add_members(&mut self, measure: &Measure) {
        self.unavailable_members = self.unavailable_members.max(measure.unavailable_members);
        self.overflowed |= measure.overflowed;
    }

    /// The measure of the resource whose known part is `known`.
    fn measure(&self, known: u64) -> Measure {
        Measure {
            known,
            measured_members: self.measured_members,
            unavailable_members: self.unavailable_members,
            overflowed: self.overflowed,
        }
    }
}

impl PointApps for AppsStretch {
    type Row = StretchRow;

    fn rows(&self) -> &[StretchRow] {
        &self.rows
    }

    fn totals(&self) -> &Totals {
        &self.totals
    }

    fn elapsed_ns(&self) -> Option<u64> {
        (self.elapsed_ns > 0).then_some(self.elapsed_ns)
    }

    fn holds_apps(&self) -> bool {
        if self.too_many {
            return false;
        }

        self.seconds > 0
    }

    fn completeness(&self) -> Completeness {
        self.completeness
    }
}

impl PointRow for StretchRow {
    fn name(&self) -> &RankedApp {
        &self.name
    }

    fn membership_complete(&self) -> bool {
        self.membership_complete
    }
}

impl Rankable for StretchRow {
    /// What the App used of `metric` over the stretch, which a chart takes over the stretch's
    /// interval, and its resident memory over the stretch's seconds.
    fn measure(&self, metric: Metric) -> Measure {
        if metric == Metric::Resident {
            return self.resident.measure(self.resident_bytes);
        }
        let Some(place) = RATES.iter().position(|rate| *rate == metric) else {
            unreachable!("every resource but resident memory is a rate");
        };

        let rate = &self.rates[place];
        rate.measure(rate.known)
    }

    fn ranked(&self) -> Arc<RankedApp> {
        Arc::clone(&self.name)
    }
}
