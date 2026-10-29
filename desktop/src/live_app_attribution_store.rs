//! Overview's App charts drawn from the store's View, point for point as the worker's App
//! histories draw them, so Overview draws them without asking the worker.

use std::sync::Arc;

use viewer::live_app_response::{KeyKind, RankedApp};
use viewer::store::{Attribution, AttributionReading, Cohorts, MachineSecond, View};

use super::machine_reference::MachineReference;
use super::{History, Measure, Metric, Other, Point, Reading};
use crate::live_app_store_points::{self, KeptPoint};
use crate::live_projection::{OverviewAppPoint, OverviewApps};
use crate::live_resident_history::{Observation, cohort_labels};

#[cfg(test)]
#[path = "live_app_attribution_store_tests.rs"]
mod tests;

impl History {
    /// The chart of `metric` drawn from the store's `view`, following its newest second, with a
    /// point wherever its machine charts draw one: a second's Apps for each full-detail second,
    /// and a stretch's for each older one. It draws on lines of their own the Apps `cohorts`
    /// chose for it, and takes the points it shares with `older`, the chart of an older View.
    pub(super) fn from_view(
        metric: Metric,
        view: &View,
        cohorts: &Cohorts,
        older: Option<Self>,
    ) -> Result<Self, String> {
        if cohorts.refused {
            return Err("App rate history holds more Apps than it adds up".into());
        }
        let cohort = cohorts.of(metric.source());
        assert_eq!(view.apps.len(), view.seconds.len());
        assert!(cohorts.seconds <= view.seconds.len());
        if cohorts.seconds == 0 {
            return Err("App rate history is empty".into());
        }
        let drawn = crate::live_projection::overview_app_points(view)?;

        let kept = from_view_kept(older, metric, cohort);
        let (positions, points) = live_app_store_points::points(
            &drawn.points,
            kept,
            |machine, apps, observation, segment| {
                from_view_point(metric, machine, apps, cohort, observation, segment)
            },
        );
        let Some(selected_index) = points.len().checked_sub(1) else {
            unreachable!("the chart draws the View's newest second");
        };

        Ok(Self {
            metric,
            points,
            cohort: from_view_labels(cohort)?,
            positions,
            window: drawn.window,
            selected_index,
            retention_limited: drawn.points.iter().any(OverviewAppPoint::retention_limited),
            view_cohort: Some(cohort.to_vec()),
        })
    }
}

impl KeptPoint for Point {
    fn observation(&self) -> &Observation {
        &self.observation
    }

    fn stretch(&self) -> bool {
        self.stretch
    }

    fn in_segment(self, segment: u64) -> Self {
        Self { segment, ..self }
    }
}

/// The points of `older`, the chart of `metric` drawn from an older View, which a chart drawn
/// after it takes for the seconds and stretches both Views hold, where both draw the Apps of
/// `cohort` on lines of their own, and none otherwise.
fn from_view_kept(older: Option<History>, metric: Metric, cohort: &[Arc<RankedApp>]) -> Vec<Point> {
    let Some(older) = older else {
        return Vec::new();
    };
    assert_eq!(older.metric, metric);

    live_app_store_points::kept(older.view_cohort.as_deref(), older.points, cohort)
}

/// The point of `apps`, the Apps of a second or of a stretch whose machine readings `machine`
/// holds, the tick `observation` names, as the chart of `metric` whose own lines draw `cohort`
/// shows it in segment `segment`.
fn from_view_point(
    metric: Metric,
    machine: &MachineSecond,
    apps: OverviewApps,
    cohort: &[Arc<RankedApp>],
    observation: Observation,
    segment: u64,
) -> Point {
    let (attribution, elapsed_ns, available, completeness) = match apps {
        OverviewApps::Second(apps) => (
            apps.attribution(metric.source(), cohort),
            apps.elapsed_ns,
            apps.missing.is_none(),
            apps.completeness(),
        ),
        OverviewApps::Stretch(stretch) => (
            stretch.attribution(metric.source(), cohort),
            stretch.elapsed_ns(),
            stretch.holds_apps(),
            stretch.completeness(),
        ),
    };

    Point {
        observation,
        segment,
        stretch: matches!(apps, OverviewApps::Stretch(_)),
        elapsed_ns,
        source_available: available,
        membership_complete: available.then_some(completeness.membership_complete),
        ungrouped_members: available.then_some(completeness.ungrouped_members),
        usage_refused: available.then_some(completeness.usage_refused),
        usage_gone: available.then_some(completeness.usage_gone),
        apps: from_view_readings(&attribution, cohort.len()),
        other: attribution.other.as_ref().map(|other| Other {
            app_count: other.app_count,
            reading: Reading::of(&other.reading),
        }),
        reference: MachineReference::of(metric, machine),
    }
}

/// The reading of each of the `count` Apps a chart draws on lines of their own, as
/// `attribution` gives them.
fn from_view_readings(attribution: &Attribution, count: usize) -> Vec<Option<Reading>> {
    attribution.apps[..count]
        .iter()
        .map(|reading| reading.as_ref().map(Reading::of))
        .collect()
}

/// The label of each of `cohort`'s Apps, as the worker's history names them.
fn from_view_labels(cohort: &[Arc<RankedApp>]) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = cohort.iter().map(|app| app.display_name.clone()).collect();

    cohort_labels(
        &mut names,
        |index| Ok(cohort[index].display()),
        |index| {
            let kind = match cohort[index].kind {
                KeyKind::Bundle => "bundle",
                KeyKind::Executable => "executable",
            };
            Ok(kind.into())
        },
    )?;
    Ok(names)
}

impl Reading {
    fn of(reading: &AttributionReading) -> Self {
        Self {
            rate: reading.rate,
            measure: reading.measure.as_ref().map(Measure::of),
            membership_complete: reading.membership_complete,
        }
    }
}

impl Measure {
    fn of(measure: &viewer::live_app_response::Measure) -> Self {
        Self {
            known: measure.known,
            measured_members: measure.measured_members,
            unavailable_members: measure.unavailable_members,
            overflowed: measure.overflowed,
        }
    }
}
