//! Overview's App charts drawn from the store's View, point for point as the worker's App
//! histories draw them, so Overview draws them without asking the worker.

use std::sync::Arc;

use viewer::live_app_response::{KeyKind, RankedApp};
use viewer::store::{AppsSecond, Attribution, AttributionReading, MachineSecond, View};

use super::machine_reference::MachineReference;
use super::{History, Measure, Metric, Other, Point, Reading};
use crate::live_resident_history::{Observation, cohort_labels, position, same_apps};

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

#[cfg(test)]
#[path = "live_app_attribution_store_tests.rs"]
mod tests;

impl History {
    /// The chart of `metric` over the newest seconds of `view` that the store chose the chart's
    /// Apps over, selecting the newest, as the worker's history of that second draws it. The
    /// chart takes the points of the seconds it shares with `older`, the chart of an older View.
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
        let count = cohorts.seconds;
        if count == 0 {
            return Err("App rate history is empty".into());
        }

        let first = view.seconds.len() - count;
        let seconds = &view.seconds[first..];
        let apps = &view.apps[first..];
        let Some(newest) = seconds.last() else {
            unreachable!("the chart draws at least one second");
        };
        let last = newest.wall_ns / NANOSECONDS_PER_SECOND;
        let mut history = Self {
            metric,
            points: Vec::with_capacity(count),
            cohort: from_view_labels(cohort)?,
            positions: Vec::with_capacity(count),
            window: crate::chart_axes::TimeWindow::charted(last),
            selected_index: count - 1,
            retention_limited: apps.iter().any(|second| second.missing.is_some()),
            view_cohort: Some(cohort.to_vec()),
        };
        let kept = from_view_kept(older, metric, cohort);
        from_view_points(&mut history, seconds, apps, cohort, last, kept)?;

        Ok(history)
    }
}

/// The points of `older`, the chart of `metric` drawn from an older View, which a chart drawn
/// after it takes for the seconds both Views hold, where both draw the Apps of `cohort` on lines
/// of their own, and none otherwise.
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

/// Adds to `history` a point for each of `seconds`, whose Apps `apps` holds, as the chart whose
/// own lines draw `cohort` shows it, breaking the line where two seconds are not consecutive.
/// A second that `kept`, the points of an older chart, holds takes its point from there.
fn from_view_points(
    history: &mut History,
    seconds: &[Arc<MachineSecond>],
    apps: &[Arc<AppsSecond>],
    cohort: &[Arc<RankedApp>],
    last: u64,
    kept: Vec<Point>,
) -> Result<(), String> {
    assert_eq!(seconds.len(), apps.len());
    assert!(history.points.is_empty());

    let mut kept = from_view_points_kept(kept, apps).into_iter().peekable();
    let mut segment = 0;
    for (index, (second, apps)) in seconds.iter().zip(apps).enumerate() {
        let Some(observation) = &apps.observation else {
            unreachable!("the store names the tick of every second it takes in");
        };
        if index > 0 {
            if !apps.consecutive {
                segment += 1;
            }
        }

        let observation = Observation::of(observation);
        history.positions.push(position(observation.wall_ns, last)?);
        let point = match kept.next_if(|point| point.observation == observation) {
            Some(point) => Point { segment, ..point },
            None => from_view_point(
                history.metric,
                &FromViewSecond {
                    machine: second,
                    apps,
                },
                cohort,
                observation,
                segment,
            ),
        };
        history.points.push(point);
    }
    assert_eq!(history.points.len(), seconds.len());

    Ok(())
}

/// The points of `kept` from the one at the first of `apps` on, since a newer chart starts no
/// earlier than the older chart `kept` came from, or none where `kept` does not hold that second.
fn from_view_points_kept(mut kept: Vec<Point>, apps: &[Arc<AppsSecond>]) -> Vec<Point> {
    let Some(first) = apps.first().and_then(|apps| apps.observation.as_ref()) else {
        return Vec::new();
    };
    let first = Observation::of(first);
    let Some(start) = kept.iter().position(|point| point.observation == first) else {
        return Vec::new();
    };

    kept.drain(..start);
    kept
}

/// One second of a View: its machine readings and its Apps.
struct FromViewSecond<'view> {
    machine: &'view MachineSecond,
    apps: &'view AppsSecond,
}

/// The point of `second`, the tick `observation` names, as the chart of `metric` whose own lines
/// draw `cohort` shows it in segment `segment`.
fn from_view_point(
    metric: Metric,
    second: &FromViewSecond,
    cohort: &[Arc<RankedApp>],
    observation: Observation,
    segment: u64,
) -> Point {
    let apps = second.apps;
    let attribution = apps.attribution(metric.source(), cohort);
    let available = apps.missing.is_none();

    Point {
        observation,
        segment,
        elapsed_ns: apps.elapsed_ns,
        source_available: available,
        membership_complete: available.then_some(apps.membership_complete),
        ungrouped_members: available.then_some(apps.ungrouped_members),
        usage_refused: available.then_some(apps.usage_refused),
        usage_gone: available.then_some(apps.usage_gone),
        apps: from_view_readings(&attribution, cohort.len()),
        other: attribution.other.as_ref().map(|other| Other {
            app_count: other.app_count,
            reading: Reading::of(&other.reading),
        }),
        reference: MachineReference::of(metric, second.machine),
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
