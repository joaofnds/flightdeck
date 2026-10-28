//! Each App's readings added up over one stretch of `EARLIER_STRIDE_SECONDS` wall seconds, so an
//! App chart of the loaded hour draws one point for each stretch that shows what its Apps used
//! over all of its seconds, where the older tail of a machine chart draws one reading of them.

use std::mem;
use std::sync::Arc;

use super::attribution::{self, Completeness, PointApps, PointRow, Totals};
use super::buffers::{Buffers, Span};
use super::{APPS_PER_SECOND_MAX, AppRow, AppsSecond, Attribution, Resident};
use crate::EARLIER_STRIDE_SECONDS;
use crate::app_attribution_history::AppAttributionMetric;
use crate::apps::AppKey;
use crate::live_app_response::{Measure, Metric, Observation, Rankable, RankedApp};

/// The Apps a stretch holds at most, as many as a second holds, past which it holds none and
/// says so.
const STRETCH_APPS_MAX: usize = APPS_PER_SECOND_MAX;

/// The resources an App uses at a rate, in the order a stretch adds them up.
const RATES: [Metric; 4] = [Metric::Cpu, Metric::Power, Metric::Disk, Metric::Gpu];

const _: () = {
    assert!(size_of::<StretchRow>() <= 184);
};

/// Where `metric`, a rate, stands among `RATES`.
fn rate_place(metric: Metric) -> usize {
    let Some(place) = RATES.iter().position(|rate| *rate == metric) else {
        unreachable!("resident memory is not a rate");
    };
    place
}

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

    /// Adds `folded`, what a loader added up of the Apps of seconds of the open stretch that the
    /// store took in without them.
    pub(super) fn add_folded(&mut self, folded: &AppsStretch) {
        self.open.add_stretch(folded, &mut self.spare);
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

/// The stretches a loader adds up from seconds of its batch, oldest first, each holding only
/// those seconds, which the store adds to the stretch it takes the same seconds into.
pub(super) struct Folding {
    open: AppsStretch,
    spare: Vec<StretchRow>,
}

impl Folding {
    pub(super) fn new() -> Self {
        Self {
            open: AppsStretch::with_room(),
            spare: Vec::with_capacity(STRETCH_APPS_MAX),
        }
    }

    /// Adds `apps`, the Apps of a second of stretch `stretch`. Returns the stretch it added up
    /// before, where `apps` belongs to another.
    pub(super) fn add(&mut self, stretch: u64, apps: &AppsSecond) -> Option<AppsStretch> {
        assert!(apps.missing.is_none());
        let finished = if self.open.observation.is_some() && self.open.stretch != stretch {
            self.finish()
        } else {
            None
        };

        self.open.add_second(stretch, apps, &mut self.spare);
        finished
    }

    /// The stretch it is adding up, where it added up any second, which it then holds no more.
    pub(super) fn finish(&mut self) -> Option<AppsStretch> {
        self.open.observation?;

        Some(self.open.detach())
    }
}

/// What a loader added up of the Apps of seconds of one stretch of one run, which it hands to the
/// oldest of those seconds, so the store holds them once it takes that second in.
#[derive(Debug)]
pub(super) struct Folded {
    pub(super) apps: AppsStretch,
    /// The publication of the newest of those seconds.
    pub(super) publication: u64,
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
    /// Whether those intervals added up past what `elapsed_ns` counts, so the stretch states no
    /// interval its rates were taken over.
    elapsed_overflowed: bool,
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
    rates: [Amount; RATES.len()],
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
    /// The intervals the seconds that measured a rate measured it over, added up, which a
    /// chart's leaders take the App's mean rate over.
    measured_ns: u64,
    /// The seconds that measured it, which a chart's leaders take the App's mean memory over.
    measured_seconds: u32,
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
        self.elapsed_overflowed = false;
        self.completeness = Completeness::default();
        self.too_many = false;
        self.rows.clear();
        self.totals = Totals::default();
    }

    /// Copies out what the stretch holds, and holds no second. Its rows are copied rather than
    /// moved, which keeps the room it holds for its Apps for the next stretch.
    fn detach(&mut self) -> Self {
        let detached = Self {
            stretch: self.stretch,
            observation: self.observation,
            seconds: self.seconds,
            elapsed_ns: self.elapsed_ns,
            elapsed_overflowed: self.elapsed_overflowed,
            completeness: self.completeness,
            too_many: self.too_many,
            rows: self.rows.clone(),
            totals: self.totals,
        };

        self.clear();
        detached
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
        self.add_completeness(PointApps::completeness(second));
        self.seconds += 1;
        let elapsed_ns = second.elapsed_ns.filter(|elapsed_ns| *elapsed_ns > 0);
        if let Some(elapsed_ns) = elapsed_ns {
            self.add_elapsed(elapsed_ns);
        }
        self.add_rows(&second.apps, spare, |row, app| row.add(app, elapsed_ns));
    }

    /// Adds `other`, the same stretch's Apps over seconds the stretch does not hold, writing the
    /// stretch's Apps into `spare`, which then trades places with them.
    fn add_stretch(&mut self, other: &Self, spare: &mut Vec<StretchRow>) {
        assert!(self.observation.is_some());
        assert_eq!(self.stretch, other.stretch);
        assert!(other.seconds > 0);

        self.add_completeness(other.completeness);
        self.seconds += other.seconds;
        self.add_elapsed(other.elapsed_ns);
        self.elapsed_overflowed |= other.elapsed_overflowed;
        if other.too_many {
            self.rows.clear();
            self.too_many = true;
        }
        self.add_rows(&other.rows, spare, StretchRow::add_row);
    }

    /// Adds `elapsed_ns` to the intervals the stretch's seconds measured their Apps over.
    fn add_elapsed(&mut self, elapsed_ns: u64) {
        match self.elapsed_ns.checked_add(elapsed_ns) {
            Some(sum) => self.elapsed_ns = sum,
            None => self.elapsed_overflowed = true,
        }
    }

    /// Adds `completeness`, what the App rule found of seconds the stretch does not hold.
    fn add_completeness(&mut self, completeness: Completeness) {
        self.completeness = if self.seconds == 0 {
            completeness
        } else {
            add_completeness(self.completeness, completeness)
        };
    }

    /// Adds `incoming`, Apps in key order, to the stretch's with `add`, writing them into
    /// `spare`, which then trades places with them. Where they come to more than
    /// `STRETCH_APPS_MAX` Apps, the stretch holds none.
    fn add_rows<R: PointRow + Rankable>(
        &mut self,
        incoming: &[R],
        spare: &mut Vec<StretchRow>,
        add: impl Fn(&mut StretchRow, &R),
    ) {
        if self.too_many {
            return;
        }

        let capacity = spare.capacity();
        spare.clear();
        let fits = add_rows_merged(&mut self.rows, incoming, spare, add);
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

    /// Every App of the stretch's seconds, once each, in key order, or `None` where they held
    /// more Apps between them than a stretch holds.
    pub(super) fn apps(&self) -> Option<&[StretchRow]> {
        if self.too_many {
            return None;
        }

        Some(&self.rows)
    }

    /// Stretch `stretch` of `seconds`, the Apps of seconds of that stretch oldest first, added up
    /// and closed as the store closes a stretch.
    #[must_use]
    pub fn of_seconds(stretch: u64, seconds: &[Arc<AppsSecond>]) -> Self {
        assert!(!seconds.is_empty());
        assert!(u64::try_from(seconds.len()).is_ok_and(|count| count <= EARLIER_STRIDE_SECONDS));

        let mut added = Self::with_room();
        let mut spare = Vec::with_capacity(STRETCH_APPS_MAX);
        for second in seconds {
            added.add_second(stretch, second, &mut spare);
        }
        added.close();
        added
    }

    /// The interval the stretch's rates are taken over, the intervals of its seconds added up,
    /// where they measured any.
    #[must_use]
    pub fn elapsed_ns(&self) -> Option<u64> {
        if self.elapsed_overflowed {
            return None;
        }

        (self.elapsed_ns > 0).then_some(self.elapsed_ns)
    }

    /// Whether the stretch holds its Apps: some of its seconds held theirs, and they came to no
    /// more Apps than a stretch holds.
    #[must_use]
    pub fn holds_apps(&self) -> bool {
        if self.too_many {
            return false;
        }

        self.seconds > 0
    }

    /// How completely the readings of the stretch's seconds found the Mac's processes.
    #[must_use]
    pub fn completeness(&self) -> Completeness {
        self.completeness
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

/// Writes into `merged` the Apps of `rows` with those of `incoming` added with `add`, in key
/// order. Returns false where they come to more than `STRETCH_APPS_MAX` Apps.
fn add_rows_merged<R: PointRow + Rankable>(
    rows: &mut Vec<StretchRow>,
    incoming: &[R],
    merged: &mut Vec<StretchRow>,
    add: impl Fn(&mut StretchRow, &R),
) -> bool {
    assert!(merged.is_empty());

    let mut held = rows.drain(..).peekable();
    for app in incoming {
        let key = key_of(app.name());
        while let Some(row) = held.next_if(|row| key_of(&row.name) < key) {
            if merged.len() == STRETCH_APPS_MAX {
                return false;
            }
            merged.push(row);
        }
        // An App out of key order would take a second row of its own.
        if let Some(last) = merged.last() {
            assert!(key_of(&last.name) < key);
        }
        let mut row = match held.next_if(|row| key_of(&row.name) == key) {
            Some(row) => row,
            None => StretchRow::new(app.ranked()),
        };

        add(&mut row, app);
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
            rates: [Amount::default(); RATES.len()],
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
            match elapsed_ns {
                Some(elapsed_ns) => amount.add_rate(&measure, elapsed_ns),
                None => amount.add_members(&measure),
            }
        }
        self.resident.add(&app.measure(Metric::Resident));
        self.membership_complete &= app.membership_complete;
    }

    /// What the App used of `metric`, a rate, over the stretch's seconds that measured it, and
    /// the intervals they measured it over, added up. An App whose sums overflowed counts none
    /// of the stretch, where its seconds one by one would count those that did not.
    pub(super) fn rate_measured(&self, metric: Metric) -> Option<(u64, u64)> {
        let rate = &self.rates[rate_place(metric)];
        let known = rate.measured()?;
        assert!(rate.measured_ns > 0);
        Some((known, rate.measured_ns))
    }

    /// The App's resident memory added up over the stretch's seconds that measured it, and how
    /// many they are.
    pub(super) fn resident_measured(&self) -> Option<(u64, u32)> {
        let known = self.resident.measured()?;

        Some((known, self.resident.measured_seconds))
    }

    /// Adds `other`, the same App's readings over seconds of the stretch it does not hold.
    fn add_row(&mut self, other: &Self) {
        assert_eq!(key_of(&self.name), key_of(&other.name));

        for (amount, other) in self.rates.iter_mut().zip(&other.rates) {
            amount.add_amount(other);
        }
        self.resident.add_amount(&other.resident);
        self.membership_complete &= other.membership_complete;
    }
}

impl Amount {
    /// Adds `measure`, a second's measure of a rate over `elapsed_ns`.
    fn add_rate(&mut self, measure: &Measure, elapsed_ns: u64) {
        assert!(elapsed_ns > 0);
        if measure.measured().is_some() {
            self.add_measured_ns(elapsed_ns);
        }

        self.add(measure);
    }

    /// Adds `measure`, a second's.
    fn add(&mut self, measure: &Measure) {
        self.add_members(measure);
        let Some(known) = measure.measured() else {
            return;
        };

        self.measured_members = self.measured_members.max(measure.measured_members);
        self.measured_seconds += 1;
        match self.known.checked_add(known) {
            Some(known) => self.known = known,
            None => self.overflowed = true,
        }
    }

    /// Adds `other`, what the App used of the resource over other seconds.
    fn add_amount(&mut self, other: &Self) {
        self.add_measured_ns(other.measured_ns);
        self.measured_seconds += other.measured_seconds;
        self.measured_members = self.measured_members.max(other.measured_members);
        self.unavailable_members = self.unavailable_members.max(other.unavailable_members);
        self.overflowed |= other.overflowed;
        match self.known.checked_add(other.known) {
            Some(known) => self.known = known,
            None => self.overflowed = true,
        }
    }

    /// Adds `measured_ns` to the intervals the App was measured over.
    fn add_measured_ns(&mut self, measured_ns: u64) {
        match self.measured_ns.checked_add(measured_ns) {
            Some(sum) => self.measured_ns = sum,
            None => self.overflowed = true,
        }
    }

    /// Adds of `measure`, a second's, only the members that did not measure the resource and
    /// whether it overflowed.
    fn add_members(&mut self, measure: &Measure) {
        self.unavailable_members = self.unavailable_members.max(measure.unavailable_members);
        self.overflowed |= measure.overflowed;
    }

    /// What the App used over the seconds that measured it, where any did and the sums did not
    /// overflow, as a second's measure counts only where it measured the resource.
    fn measured(&self) -> Option<u64> {
        if self.measured_seconds == 0 {
            return None;
        }

        self.measure(self.known).measured()
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
        Self::elapsed_ns(self)
    }

    fn holds_apps(&self) -> bool {
        Self::holds_apps(self)
    }

    fn completeness(&self) -> Completeness {
        Self::completeness(self)
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
        let rate = &self.rates[rate_place(metric)];
        rate.measure(rate.known)
    }

    fn ranked(&self) -> Arc<RankedApp> {
        Arc::clone(&self.name)
    }
}
