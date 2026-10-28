//! Overview's App charts drawn from the store's View, point for point as the worker's App
//! histories draw them, so Overview draws them without asking the worker.

use std::sync::Arc;

use viewer::live_app_response::{KeyKind, RankedApp};
use viewer::store::{Attribution, AttributionReading, MachineSecond, View};

use super::machine_reference::MachineReference;
use super::{History, Measure, Metric, Other, Point, Reading};
use crate::live_projection::{OverviewAppPoint, OverviewApps};
use crate::live_resident_history::{Observation, cohort_labels, same_apps};

#[cfg(test)]
#[path = "live_app_attribution_store_tests.rs"]
mod tests;

impl History {
    /// The chart of `metric` drawn from the store's `view`, following its newest second, with a
    /// point wherever its machine charts draw one: a second's Apps for each full-detail second,
    /// and a stretch's for each older one. The chart takes the points it shares with `older`,
    /// the chart of an older View.
    pub(super) fn from_view(
        metric: Metric,
        view: &View,
        older: Option<Self>,
    ) -> Result<Self, String> {
        let cohorts = &view.cohorts;
        if cohorts.refused {
            return Err("App rate history holds more Apps than it adds up".into());
        }
        let Some(cohort) = cohorts.of(metric.source()) else {
            unreachable!("Overview draws no App chart of GPU time");
        };
        assert_eq!(view.apps.len(), view.seconds.len());
        assert!(cohorts.seconds <= view.seconds.len());
        if cohorts.seconds == 0 {
            return Err("App rate history is empty".into());
        }
        let Some(drawn) = crate::live_projection::overview_app_points(view)? else {
            unreachable!("the store chose the chart's Apps over seconds the View holds");
        };

        let count = drawn.points.len();
        let mut history = Self {
            metric,
            points: Vec::with_capacity(count),
            cohort: from_view_labels(cohort)?,
            positions: Vec::with_capacity(count),
            window: drawn.window,
            selected_index: 0,
            retention_limited: drawn.points.iter().any(from_view_retention_limited),
            view_cohort: Some(cohort.to_vec()),
        };
        let kept = from_view_kept(older, metric, cohort);
        from_view_points(&mut history, &drawn.points, cohort, kept);
        let Some(selected_index) = history.points.len().checked_sub(1) else {
            unreachable!("the chart draws the View's newest second");
        };
        history.selected_index = selected_index;

        Ok(history)
    }
}

/// Whether `point` shows less than the whole of what its Apps used, because the recording kept
/// none of them there.
fn from_view_retention_limited(point: &OverviewAppPoint) -> bool {
    match point.apps {
        Some(OverviewApps::Second(apps)) => apps.missing.is_some(),
        Some(OverviewApps::Stretch(stretch)) => !stretch.holds_apps(),
        None => true,
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
    let Some(drawn) = &older.view_cohort else {
        return Vec::new();
    };
    if !same_apps(drawn, cohort) {
        return Vec::new();
    }

    older.points
}

/// Adds to `history` a point for each of `drawn` that shows Apps, as the chart whose own lines
/// draw `cohort` shows it, breaking the line where the machine charts break theirs, where two
/// seconds are not consecutive, and where a point shows none. A point that `kept`, the points of
/// an older chart, holds takes its point from there.
fn from_view_points(
    history: &mut History,
    drawn: &[OverviewAppPoint],
    cohort: &[Arc<RankedApp>],
    kept: Vec<Point>,
) {
    assert!(history.points.is_empty());

    let mut kept = Kept::of(kept, drawn);
    let mut segment = 0;
    let mut previous = None;
    for point in drawn {
        let Some(apps) = point.apps else {
            previous = None;
            continue;
        };
        let (observation, follows) = match apps {
            OverviewApps::Second(apps) => (apps.observation, apps.consecutive),
            OverviewApps::Stretch(stretch) => (stretch.observation, true),
        };
        let Some(observation) = &observation else {
            unreachable!("the store names the tick of every second and stretch it takes in");
        };
        if !history.points.is_empty() {
            if previous != Some(point.segment) || !follows {
                segment += 1;
            }
        }
        previous = Some(point.segment);

        let observation = Observation::of(observation);
        history.positions.push(point.position);
        let made = match kept.take(apps, &observation) {
            Some(made) => Point { segment, ..made },
            None => from_view_point(
                history.metric,
                point.machine,
                apps,
                cohort,
                observation,
                segment,
            ),
        };
        history.points.push(made);
    }
    assert_eq!(history.points.len(), history.positions.len());
}

/// The points of an older chart that a newer one takes, the stretches' and the seconds' apart,
/// each from the one at the newer chart's first of its kind on.
struct Kept {
    stretches: std::iter::Peekable<std::vec::IntoIter<Point>>,
    seconds: std::iter::Peekable<std::vec::IntoIter<Point>>,
}

impl Kept {
    /// The points of `kept` that the chart of `drawn` takes, since a newer chart starts each
    /// kind no earlier than the older chart `kept` came from, and none of a kind where `kept`
    /// does not hold the newer chart's first.
    fn of(kept: Vec<Point>, drawn: &[OverviewAppPoint]) -> Self {
        let (stretches, seconds): (Vec<Point>, Vec<Point>) =
            kept.into_iter().partition(|point| point.stretch);
        let first = |stretch: bool| {
            drawn.iter().find_map(|point| match point.apps? {
                OverviewApps::Second(apps) if !stretch => apps.observation,
                OverviewApps::Stretch(held) if stretch => held.observation,
                _ => None,
            })
        };

        Self {
            stretches: Self::of_from(stretches, first(true)).into_iter().peekable(),
            seconds: Self::of_from(seconds, first(false)).into_iter().peekable(),
        }
    }

    /// The points of `kept` from the one at `first` on, or none where `kept` does not hold it.
    fn of_from(
        mut kept: Vec<Point>,
        first: Option<viewer::live_app_response::Observation>,
    ) -> Vec<Point> {
        let Some(first) = first else {
            return Vec::new();
        };
        let first = Observation::of(&first);
        let Some(start) = kept.iter().position(|point| point.observation == first) else {
            return Vec::new();
        };

        kept.drain(..start);
        kept
    }

    /// The kept point of `apps`, the tick `observation` names, where the older chart drew it.
    fn take(&mut self, apps: OverviewApps, observation: &Observation) -> Option<Point> {
        let kept = match apps {
            OverviewApps::Second(_) => &mut self.seconds,
            OverviewApps::Stretch(_) => &mut self.stretches,
        };

        kept.next_if(|point| point.observation == *observation)
    }
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
