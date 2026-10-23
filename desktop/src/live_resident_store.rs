//! Overview's chart of resident memory by App drawn from the store's View, point for point as
//! the worker's history of resident memory draws it, so Overview draws it without asking the
//! worker.

use std::sync::Arc;

use viewer::live_app_response::{KeyKind, RankedApp};
use viewer::store::{AppsSecond, View};

use super::{History, Measure, NS, Observation, Other, Point, ResidentHistory};
use super::{cohort_labels, position};

#[cfg(test)]
#[path = "live_resident_store_tests.rs"]
mod tests;

impl ResidentHistory {
    /// The chart of resident memory by App drawn from the store's `view`.
    pub fn from_view(view: &View) -> Self {
        match History::from_view(view) {
            Ok(history) => Self::Ready(history),
            Err(error) => Self::Unavailable(error),
        }
    }
}

impl History {
    /// The chart over the newest seconds of `view` that the store chose the chart's Apps over,
    /// selecting the newest, as the worker's history of that second draws it.
    fn from_view(view: &View) -> Result<Self, String> {
        let cohorts = &view.cohorts;
        if cohorts.refused {
            return Err("Resident App history holds more Apps than it adds up".into());
        }
        assert_eq!(view.apps.len(), view.seconds.len());
        assert!(cohorts.seconds <= view.seconds.len());
        let count = cohorts.seconds;
        if count == 0 {
            return Err("Resident App history is empty".into());
        }

        let apps = &view.apps[view.apps.len() - count..];
        let Some(newest) = view.seconds.last() else {
            unreachable!("the chart draws at least one second");
        };
        let last = newest.wall_ns / NS;
        let mut history = Self {
            points: Vec::with_capacity(count),
            cohort: from_view_labels(&cohorts.resident)?,
            positions: Vec::with_capacity(count),
            window: crate::chart_axes::TimeWindow::charted(last),
            selected_index: count - 1,
            retention_limited: apps.iter().any(|second| second.missing.is_some()),
        };
        from_view_points(&mut history, apps, &cohorts.resident, last)?;

        Ok(history)
    }
}

/// Adds to `history` a point for each of `apps`, as the chart whose own lines draw `cohort`
/// shows it, breaking the line where two seconds are not consecutive.
fn from_view_points(
    history: &mut History,
    apps: &[Arc<AppsSecond>],
    cohort: &[Arc<RankedApp>],
    last: u64,
) -> Result<(), String> {
    assert!(history.points.is_empty());

    let mut segment = 0;
    for (index, apps) in apps.iter().enumerate() {
        let Some(observation) = &apps.observation else {
            unreachable!("the store names the tick of every second it takes in");
        };
        if index > 0 {
            if !apps.consecutive {
                segment += 1;
            }
        }

        let resident = apps.resident(cohort);
        let available = apps.missing.is_none();
        history.positions.push(position(observation.wall_ns, last)?);
        history.points.push(Point {
            observation: Observation::of(observation),
            segment,
            source_available: available,
            membership_complete: available.then_some(apps.membership_complete),
            ungrouped_members: available.then_some(apps.live_ungrouped_members),
            usage_refused: available.then_some(apps.usage_refused),
            usage_gone: available.then_some(apps.usage_gone),
            apps: resident.apps[..cohort.len()]
                .iter()
                .map(|measure| measure.as_ref().map(Measure::of))
                .collect(),
            other: resident.other.as_ref().map(|other| Other {
                app_count: other.app_count,
                resident_bytes: other.resident.as_ref().map(Measure::of),
            }),
        });
    }
    assert_eq!(history.points.len(), apps.len());

    Ok(())
}

/// The label of each of `cohort`'s Apps, as the worker's history of resident memory names
/// them: a bundle by the last part of its path, an executable by its whole name.
fn from_view_labels(cohort: &[Arc<RankedApp>]) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = cohort
        .iter()
        .map(|app| {
            let display = app.display();
            match app.kind {
                KeyKind::Bundle => display
                    .rsplit_once('/')
                    .map_or(display.as_str(), |(_, base)| base)
                    .into(),
                KeyKind::Executable => display,
            }
        })
        .collect();

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
