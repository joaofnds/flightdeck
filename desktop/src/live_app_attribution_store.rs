//! Overview's App charts drawn from the store's View, point for point as the worker's App
//! histories draw them, so Overview draws them without asking the worker.

use std::sync::Arc;

use viewer::live_app_response::{KeyKind, RankedApp};
use viewer::store::{AppsSecond, Attribution, AttributionReading, MachineSecond, View};

use super::machine_reference::MachineReference;
use super::{History, Measure, Metric, Other, Point, Reading};
use crate::live_resident_history::{Observation, cohort_labels, position};

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

#[cfg(test)]
#[path = "live_app_attribution_store_tests.rs"]
mod tests;

impl History {
    /// The chart of `metric` over the newest seconds of `view` that the store chose the chart's
    /// Apps over, selecting the newest, as the worker's history of that second draws it.
    pub(super) fn from_view(metric: Metric, view: &View) -> Result<Self, String> {
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
        };
        from_view_points(&mut history, seconds, apps, cohort, last)?;

        Ok(history)
    }
}

/// Adds to `history` a point for each of `seconds`, whose Apps `apps` holds, as the chart whose
/// own lines draw `cohort` shows it, breaking the line where two seconds are not consecutive.
fn from_view_points(
    history: &mut History,
    seconds: &[Arc<MachineSecond>],
    apps: &[Arc<AppsSecond>],
    cohort: &[Arc<RankedApp>],
    last: u64,
) -> Result<(), String> {
    assert_eq!(seconds.len(), apps.len());
    assert!(history.points.is_empty());

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

        let attribution = apps.attribution(history.metric.source(), cohort);
        let available = apps.missing.is_none();
        history.positions.push(position(observation.wall_ns, last)?);
        history.points.push(Point {
            observation: Observation::of(observation),
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
            reference: MachineReference::of(history.metric, second),
        });
    }
    assert_eq!(history.points.len(), seconds.len());

    Ok(())
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
