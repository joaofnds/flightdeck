//! What each second shows on the App charts drawn from a View: the reading of each App a chart draws on a
//! line of its own, and the other Apps added up as Other, as the worker's App histories show
//! them.

use std::sync::Arc;

use super::{AppRow, AppsSecond};
use crate::app_attribution_history::AppAttributionMetric;
use crate::live::resident_history::has_resident;
use crate::live_app_response::{Measure, Metric, Rankable, RankedApp};

/// The resources the pages draw an App chart of at a rate, in the order the store adds them up.
pub(super) const METRICS: [AppAttributionMetric; 4] = [
    AppAttributionMetric::Cpu,
    AppAttributionMetric::Power,
    AppAttributionMetric::Disk,
    AppAttributionMetric::Gpu,
];

/// The Apps a chart draws on lines of their own, as the worker's App histories choose them.
pub const COHORT_MAX: usize = 8;

/// Every App of a second added up, so a chart finds what the Apps it does not draw on lines of
/// their own used by taking away the Apps it does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Each of `METRICS` added up over the second's Apps, in their order.
    sums: [Sum; METRICS.len()],
    /// The Apps whose members the App rule did not all find.
    incomplete: u32,
    /// The resident memory of the Apps that hold a reading of it, added up.
    resident: Sum,
    /// The Apps that hold a reading of resident memory, which the worker's history of resident
    /// memory counts, leaving out an App all of whose members it never asked.
    resident_apps: u32,
}

/// One resource added up over some of a second's Apps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Sum {
    known: u128,
    measured_members: u64,
    unavailable_members: u64,
    /// The Apps whose measure of the resource overflowed.
    overflowed: u32,
}

/// What one second shows on one of the App charts drawn from a View.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Attribution {
    /// The reading of each App the chart draws on a line of its own, in the cohort's order,
    /// `None` where the second holds no reading of the App and past the cohort's end.
    pub apps: [Option<AttributionReading>; COHORT_MAX],
    /// The other Apps added up, `None` where the second holds no Apps.
    pub other: Option<OtherReading>,
}

/// One line's reading at one second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttributionReading {
    /// The rate the line draws, `None` where the measure or the second's interval is unknown.
    pub rate: Option<f64>,
    /// What the line's Apps used, `None` for an Other of no Apps at a second whose readings
    /// missed some process, since such an Other may have missed an App.
    pub measure: Option<Measure>,
    /// Whether the App rule found every member of the line's Apps.
    pub membership_complete: bool,
}

/// The Apps a chart does not draw on lines of their own, added up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OtherReading {
    pub app_count: u32,
    pub reading: AttributionReading,
}

/// What one second shows on the chart of resident memory by App drawn from a View.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resident {
    /// The resident memory of each App the chart draws on a line of its own, in the cohort's
    /// order, `None` where the second holds no reading of the App and past the cohort's end.
    pub apps: [Option<Measure>; COHORT_MAX],
    /// The other Apps added up, `None` where the second holds no Apps.
    pub other: Option<ResidentOther>,
}

/// The Apps the chart of resident memory does not draw on lines of their own, added up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResidentOther {
    pub app_count: u32,
    /// What they hold, `None` for an Other of no Apps at a second whose readings missed some
    /// process, since such an Other may have missed an App.
    pub resident: Option<Measure>,
}

impl Totals {
    /// Every App of `apps` added up.
    #[must_use]
    pub fn of(apps: &[AppRow]) -> Self {
        Self::of_rows(apps)
    }

    /// Every App of `rows`, one chart point's, added up.
    pub(super) fn of_rows<R: PointRow>(rows: &[R]) -> Self {
        let mut totals = Self::default();
        for row in rows {
            totals.add(row);
        }

        totals
    }

    fn add<R: PointRow>(&mut self, row: &R) {
        for (sum, metric) in self.sums.iter_mut().zip(METRICS) {
            sum.add(&row.measure(ranked_metric(metric)));
        }
        if !row.membership_complete() {
            self.incomplete += 1;
        }
        let resident = row.measure(Metric::Resident);
        if has_resident(&resident) {
            self.resident.add(&resident);
            self.resident_apps += 1;
        }
    }
}

impl Sum {
    fn add(&mut self, measure: &Measure) {
        self.known += u128::from(measure.known);
        self.measured_members += u64::from(measure.measured_members);
        self.unavailable_members += u64::from(measure.unavailable_members);
        if measure.overflowed {
            self.overflowed += 1;
        }
    }

    /// The measure of the Apps `self` adds up and `taken` does not, where `taken` adds up some
    /// of them. Marks it overflowed where its sums pass what a measure holds.
    fn without(&self, taken: &Self) -> Measure {
        assert!(taken.known <= self.known);
        assert!(taken.overflowed <= self.overflowed);

        let known = u64::try_from(self.known - taken.known);
        let measured_members = u32::try_from(self.measured_members - taken.measured_members);
        let unavailable_members =
            u32::try_from(self.unavailable_members - taken.unavailable_members);
        let overflowed = self.overflowed > taken.overflowed;
        match (known, measured_members, unavailable_members) {
            (Ok(known), Ok(measured_members), Ok(unavailable_members)) => Measure {
                known,
                measured_members,
                unavailable_members,
                overflowed,
            },
            (known, measured_members, unavailable_members) => Measure {
                known: known.unwrap_or(u64::MAX),
                measured_members: measured_members.unwrap_or(u32::MAX),
                unavailable_members: unavailable_members.unwrap_or(u32::MAX),
                overflowed: true,
            },
        }
    }
}

/// One App's readings at one chart point, one second's or one stretch's.
pub(super) trait PointRow: Rankable {
    fn name(&self) -> &RankedApp;
    /// Whether the App rule found every member of the App at each second of the point.
    fn membership_complete(&self) -> bool;
}

/// The Apps one chart point shows, one second's or one stretch's.
pub(super) trait PointApps {
    type Row: PointRow;

    /// Every App of the point, once each, in key order.
    fn rows(&self) -> &[Self::Row];
    /// Every App of `rows` added up, as `Totals::of` adds them.
    fn totals(&self) -> &Totals;
    /// The interval the point's rates are taken over, where it is known.
    fn elapsed_ns(&self) -> Option<u64>;
    /// Whether the point holds its Apps.
    fn holds_apps(&self) -> bool;
    /// How completely the point's readings found the Mac's processes.
    fn completeness(&self) -> Completeness;
}

/// How completely one chart point's readings found the Mac's processes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Completeness {
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    /// The ungrouped members live at the point, which the chart of resident memory counts,
    /// since an exited process holds no memory.
    pub live_ungrouped_members: u32,
    pub usage_refused: u32,
    pub usage_gone: u32,
}

impl AppsSecond {
    /// What the second shows on the App chart of `metric` whose own lines draw `cohort`.
    #[must_use]
    pub fn attribution(
        &self,
        metric: AppAttributionMetric,
        cohort: &[Arc<RankedApp>],
    ) -> Attribution {
        attribution(self, metric, cohort)
    }

    /// What the second shows on the chart of resident memory by App whose own lines draw
    /// `cohort`.
    #[must_use]
    pub fn resident(&self, cohort: &[Arc<RankedApp>]) -> Resident {
        resident(self, cohort)
    }

    /// How completely the second's readings found the Mac's processes.
    #[must_use]
    pub fn completeness(&self) -> Completeness {
        Completeness {
            membership_complete: self.membership_complete,
            ungrouped_members: self.ungrouped_members,
            live_ungrouped_members: self.live_ungrouped_members,
            usage_refused: self.usage_refused,
            usage_gone: self.usage_gone,
        }
    }
}

impl PointApps for AppsSecond {
    type Row = AppRow;

    fn rows(&self) -> &[AppRow] {
        &self.apps
    }

    fn totals(&self) -> &Totals {
        &self.totals
    }

    fn elapsed_ns(&self) -> Option<u64> {
        self.elapsed_ns
    }

    fn holds_apps(&self) -> bool {
        self.missing.is_none()
    }

    fn completeness(&self) -> Completeness {
        Self::completeness(self)
    }
}

impl PointRow for AppRow {
    fn name(&self) -> &RankedApp {
        &self.name
    }

    fn membership_complete(&self) -> bool {
        self.membership_complete
    }
}

/// What `point` shows on the App chart of `metric` whose own lines draw `cohort`.
pub(super) fn attribution<P: PointApps>(
    point: &P,
    metric: AppAttributionMetric,
    cohort: &[Arc<RankedApp>],
) -> Attribution {
    assert!(cohort.len() <= COHORT_MAX);
    let Some(place) = METRICS.iter().position(|each| *each == metric) else {
        unreachable!("the store adds up every resource an App chart draws at a rate");
    };

    let mut attribution = Attribution {
        apps: [None; COHORT_MAX],
        other: None,
    };
    if !point.holds_apps() {
        return attribution;
    }

    let mut taken = Totals::default();
    let mut found = 0;
    for (reading, name) in attribution.apps.iter_mut().zip(cohort) {
        let Some(row) = row(point, name) else {
            continue;
        };
        let measure = row.measure(ranked_metric(metric));

        *reading = Some(AttributionReading {
            rate: attribution_rate(metric, &measure, point.elapsed_ns()),
            measure: Some(measure),
            membership_complete: row.membership_complete(),
        });
        taken.add(row);
        found += 1;
    }
    attribution.other = Some(attribution_other(point, metric, place, &taken, found));

    attribution
}

/// The Other of `point`'s chart of `metric`, the resource at `place` among `METRICS`, where the
/// chart's own lines draw the `found` Apps `taken` adds up.
fn attribution_other<P: PointApps>(
    point: &P,
    metric: AppAttributionMetric,
    place: usize,
    taken: &Totals,
    found: usize,
) -> OtherReading {
    assert!(found <= point.rows().len());
    let Ok(app_count) = u32::try_from(point.rows().len() - found) else {
        unreachable!("a point holds at most `APPS_PER_SECOND_MAX` Apps");
    };

    if app_count == 0 {
        return OtherReading {
            app_count,
            reading: attribution_other_empty(point, metric),
        };
    }
    let totals = point.totals();
    let measure = totals.sums[place].without(&taken.sums[place]);
    OtherReading {
        app_count,
        reading: AttributionReading {
            rate: attribution_rate(metric, &measure, point.elapsed_ns()),
            measure: Some(measure),
            membership_complete: totals.incomplete == taken.incomplete,
        },
    }
}

/// The Other of a chart whose own lines draw every App of `point`, which used nothing where the
/// point's readings found every process, and is unknown where they missed one.
fn attribution_other_empty<P: PointApps>(
    point: &P,
    metric: AppAttributionMetric,
) -> AttributionReading {
    let completeness = point.completeness();
    if !completeness.found_every_process(completeness.ungrouped_members) {
        return AttributionReading {
            rate: None,
            measure: None,
            membership_complete: true,
        };
    }

    AttributionReading {
        rate: metric.rate(0, point.elapsed_ns()),
        measure: Some(Measure {
            known: 0,
            measured_members: 0,
            unavailable_members: 0,
            overflowed: false,
        }),
        membership_complete: true,
    }
}

/// What `point` shows on the chart of resident memory by App whose own lines draw `cohort`.
pub(super) fn resident<P: PointApps>(point: &P, cohort: &[Arc<RankedApp>]) -> Resident {
    assert!(cohort.len() <= COHORT_MAX);

    let mut resident = Resident {
        apps: [None; COHORT_MAX],
        other: None,
    };
    if !point.holds_apps() {
        return resident;
    }

    let mut taken = Totals::default();
    for (reading, name) in resident.apps.iter_mut().zip(cohort) {
        let Some(row) = row(point, name) else {
            continue;
        };
        let measure = row.measure(Metric::Resident);
        if !has_resident(&measure) {
            continue;
        }

        *reading = Some(measure);
        taken.add(row);
    }
    resident.other = Some(resident_other(point, &taken));

    resident
}

/// The Other of `point`'s chart of resident memory, where its own lines draw the Apps `taken`
/// adds up.
fn resident_other<P: PointApps>(point: &P, taken: &Totals) -> ResidentOther {
    let totals = point.totals();
    assert!(taken.resident_apps <= totals.resident_apps);
    let app_count = totals.resident_apps - taken.resident_apps;

    if app_count == 0 {
        let completeness = point.completeness();
        let found_every_process =
            completeness.found_every_process(completeness.live_ungrouped_members);
        return ResidentOther {
            app_count,
            resident: found_every_process.then_some(Measure {
                known: 0,
                measured_members: 0,
                unavailable_members: 0,
                overflowed: false,
            }),
        };
    }
    ResidentOther {
        app_count,
        resident: Some(totals.resident.without(&taken.resident)),
    }
}

/// The row of the App `name` names, where `point` holds it.
fn row<'point, P: PointApps>(point: &'point P, name: &RankedApp) -> Option<&'point P::Row> {
    let key = name.kind.key(&name.bytes);
    let rows = point.rows();
    let index = rows
        .binary_search_by(|row| row.name().kind.key(&row.name().bytes).cmp(&key))
        .ok()?;

    Some(&rows[index])
}

impl Completeness {
    /// Whether the readings found every process, where `ungrouped_members` of them joined no
    /// App.
    fn found_every_process(self, ungrouped_members: u32) -> bool {
        let complete = [
            self.membership_complete,
            ungrouped_members == 0,
            self.usage_refused == 0,
            self.usage_gone == 0,
        ];

        !complete.contains(&false)
    }
}

/// The rate `measure` comes to over `elapsed_ns`, where both are known.
fn attribution_rate(
    metric: AppAttributionMetric,
    measure: &Measure,
    elapsed_ns: Option<u64>,
) -> Option<f64> {
    metric.rate(measure.measured()?, elapsed_ns)
}

/// The resource `metric` charts, as a ranking names it.
pub(super) fn ranked_metric(metric: AppAttributionMetric) -> Metric {
    match metric {
        AppAttributionMetric::Cpu => Metric::Cpu,
        AppAttributionMetric::Power => Metric::Power,
        AppAttributionMetric::Disk => Metric::Disk,
        AppAttributionMetric::Gpu => Metric::Gpu,
    }
}
