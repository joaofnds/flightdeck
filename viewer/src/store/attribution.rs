//! What each second shows on Overview's App charts: the reading of each App a chart draws on a
//! line of its own, and the other Apps added up as Other, as the worker's App histories show
//! them.

use std::sync::Arc;

use super::{AppRow, AppsSecond};
use crate::app_attribution_history::AppAttributionMetric;
use crate::live::resident_history::has_resident;
use crate::live_app_response::{Measure, Metric, Rankable, RankedApp};

/// The resources Overview draws an App chart of, in the order the store adds them up.
pub(super) const METRICS: [AppAttributionMetric; 3] = [
    AppAttributionMetric::Cpu,
    AppAttributionMetric::Power,
    AppAttributionMetric::Disk,
];

/// The Apps a chart draws on lines of their own, as the worker's App histories choose them.
pub const COHORT_MAX: usize = 8;

/// Every App of a second added up, so a chart finds what the Apps it does not draw on lines of
/// their own used by taking away the Apps it does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Each of `METRICS` added up over the second's Apps, in their order.
    sums: [Sum; 3],
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

/// What one second shows on one of Overview's App charts.
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

/// What one second shows on Overview's chart of resident memory by App.
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
        let mut totals = Self::default();
        for row in apps {
            totals.add(row);
        }

        totals
    }

    fn add(&mut self, row: &AppRow) {
        for (sum, metric) in self.sums.iter_mut().zip(METRICS) {
            sum.add(&row.measure(ranked_metric(metric)));
        }
        if !row.membership_complete {
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

impl AppsSecond {
    /// What the second shows on the App chart of `metric` whose own lines draw `cohort`.
    #[must_use]
    pub fn attribution(
        &self,
        metric: AppAttributionMetric,
        cohort: &[Arc<RankedApp>],
    ) -> Attribution {
        assert!(cohort.len() <= COHORT_MAX);
        let Some(place) = METRICS.iter().position(|each| *each == metric) else {
            unreachable!("Overview draws no App chart of GPU time");
        };

        let mut attribution = Attribution {
            apps: [None; COHORT_MAX],
            other: None,
        };
        if self.missing.is_some() {
            return attribution;
        }

        let mut taken = Totals::default();
        let mut found = 0;
        for (reading, name) in attribution.apps.iter_mut().zip(cohort) {
            let Some(row) = self.row(name) else {
                continue;
            };
            let measure = row.measure(ranked_metric(metric));

            *reading = Some(AttributionReading {
                rate: attribution_rate(metric, &measure, self.elapsed_ns),
                measure: Some(measure),
                membership_complete: row.membership_complete,
            });
            taken.add(row);
            found += 1;
        }
        attribution.other = Some(self.attribution_other(metric, place, &taken, found));

        attribution
    }

    /// The Other of the chart of `metric`, the resource at `place` among `METRICS`, where the
    /// chart's own lines draw the `found` Apps `taken` adds up.
    fn attribution_other(
        &self,
        metric: AppAttributionMetric,
        place: usize,
        taken: &Totals,
        found: usize,
    ) -> OtherReading {
        assert!(found <= self.apps.len());
        let Ok(app_count) = u32::try_from(self.apps.len() - found) else {
            unreachable!("a second holds at most `APPS_PER_SECOND_MAX` Apps");
        };

        if app_count == 0 {
            return OtherReading {
                app_count,
                reading: self.attribution_other_empty(metric),
            };
        }
        let measure = self.totals.sums[place].without(&taken.sums[place]);
        OtherReading {
            app_count,
            reading: AttributionReading {
                rate: attribution_rate(metric, &measure, self.elapsed_ns),
                measure: Some(measure),
                membership_complete: self.totals.incomplete == taken.incomplete,
            },
        }
    }

    /// The Other of a chart whose own lines draw every App of the second, which used nothing
    /// where the second's readings found every process, and is unknown where they missed one.
    fn attribution_other_empty(&self, metric: AppAttributionMetric) -> AttributionReading {
        if !self.found_every_process(self.ungrouped_members) {
            return AttributionReading {
                rate: None,
                measure: None,
                membership_complete: true,
            };
        }

        AttributionReading {
            rate: metric.rate(0, self.elapsed_ns),
            measure: Some(Measure {
                known: 0,
                measured_members: 0,
                unavailable_members: 0,
                overflowed: false,
            }),
            membership_complete: true,
        }
    }

    /// What the second shows on the chart of resident memory by App whose own lines draw
    /// `cohort`.
    #[must_use]
    pub fn resident(&self, cohort: &[Arc<RankedApp>]) -> Resident {
        assert!(cohort.len() <= COHORT_MAX);

        let mut resident = Resident {
            apps: [None; COHORT_MAX],
            other: None,
        };
        if self.missing.is_some() {
            return resident;
        }

        let mut taken = Totals::default();
        for (reading, name) in resident.apps.iter_mut().zip(cohort) {
            let Some(row) = self.row(name) else {
                continue;
            };
            let measure = row.measure(Metric::Resident);
            if !has_resident(&measure) {
                continue;
            }

            *reading = Some(measure);
            taken.add(row);
        }
        resident.other = Some(self.resident_other(&taken));

        resident
    }

    /// The Other of the chart of resident memory, where its own lines draw the Apps `taken` adds
    /// up.
    fn resident_other(&self, taken: &Totals) -> ResidentOther {
        assert!(taken.resident_apps <= self.totals.resident_apps);
        let app_count = self.totals.resident_apps - taken.resident_apps;

        if app_count == 0 {
            let found_every_process = self.found_every_process(self.live_ungrouped_members);
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
            resident: Some(self.totals.resident.without(&taken.resident)),
        }
    }

    /// The row of the App `name` names, where the second holds it.
    fn row(&self, name: &RankedApp) -> Option<&AppRow> {
        let key = name.kind.key(&name.bytes);
        let index = self
            .apps
            .binary_search_by(|row| row.name.kind.key(&row.name.bytes).cmp(&key))
            .ok()?;

        Some(&self.apps[index])
    }

    /// Whether the second's readings found every process, where `ungrouped_members` of them
    /// joined no App.
    fn found_every_process(&self, ungrouped_members: u32) -> bool {
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
