//! Overview's chart of resident memory by App drawn from the store's View, point for point as
//! the worker's history of resident memory draws it, so Overview draws it without asking the
//! worker.

use std::sync::Arc;

use viewer::live_app_response::{KeyKind, RankedApp};
use viewer::store::{Cohorts, View};

use super::{History, Measure, Observation, Other, Point, ResidentHistory};
use super::{cohort_labels, same_apps};
use crate::live_projection::{OverviewAppPoint, OverviewApps};

#[cfg(test)]
#[path = "live_resident_store_tests.rs"]
mod tests;

impl ResidentHistory {
    /// The chart of resident memory by App drawn from the store's `view`, drawing on lines of
    /// their own the Apps `cohorts` chose for it.
    pub fn from_view(view: &View, cohorts: &Cohorts) -> Self {
        match History::from_view(view, cohorts, None) {
            Ok(history) => Self::Ready(history),
            Err(error) => Self::Unavailable(error),
        }
    }

    /// The chart of resident memory by App drawn from the store's `view`, drawing on lines of
    /// their own the Apps `cohorts` chose for it, after `older`, the chart of an older View,
    /// which gives it the points of the seconds both Views hold.
    pub fn from_view_after(view: &View, cohorts: &Cohorts, older: Self) -> Self {
        let older = match older {
            Self::Ready(history) => Some(history),
            Self::Unavailable(_) => None,
        };

        match History::from_view(view, cohorts, older) {
            Ok(history) => Self::Ready(history),
            Err(error) => Self::Unavailable(error),
        }
    }
}

impl History {
    /// The chart drawn from the store's `view`, following its newest second, with a point
    /// wherever its machine charts draw one: a second's Apps for each full-detail second, and a
    /// stretch's for each older one. It draws on lines of their own the Apps `cohorts` chose
    /// for it, and takes the points it shares with `older`, the chart of an older View.
    fn from_view(view: &View, cohorts: &Cohorts, older: Option<Self>) -> Result<Self, String> {
        if cohorts.refused {
            return Err("Resident App history holds more Apps than it adds up".into());
        }
        assert_eq!(view.apps.len(), view.seconds.len());
        assert!(cohorts.seconds <= view.seconds.len());
        if cohorts.seconds == 0 {
            return Err("Resident App history is empty".into());
        }
        let Some(drawn) = crate::live_projection::overview_app_points(view)? else {
            unreachable!("the chart's Apps were chosen over seconds the View holds");
        };

        let count = drawn.points.len();
        let mut history = Self {
            points: Vec::with_capacity(count),
            cohort: from_view_labels(&cohorts.resident)?,
            positions: Vec::with_capacity(count),
            window: drawn.window,
            selected_index: 0,
            retention_limited: drawn.points.iter().any(from_view_retention_limited),
            view_cohort: Some(cohorts.resident.clone()),
        };
        let kept = from_view_kept(older, &cohorts.resident);
        from_view_points(&mut history, &drawn.points, &cohorts.resident, kept);
        let Some(selected_index) = history.points.len().checked_sub(1) else {
            unreachable!("the chart draws the View's newest second");
        };
        history.selected_index = selected_index;

        Ok(history)
    }
}

/// Whether `point` shows less than the whole of what its Apps held, because the recording kept
/// none of them there.
fn from_view_retention_limited(point: &OverviewAppPoint) -> bool {
    match point.apps {
        Some(OverviewApps::Second(apps)) => apps.missing.is_some(),
        Some(OverviewApps::Stretch(stretch)) => !stretch.holds_apps(),
        None => true,
    }
}

/// The points of `older`, the chart of an older View, which a chart drawn after it takes for the
/// seconds and stretches both Views hold, where both draw the Apps of `cohort` on lines of their
/// own, and none otherwise.
fn from_view_kept(older: Option<History>, cohort: &[Arc<RankedApp>]) -> Vec<Point> {
    let Some(older) = older else {
        return Vec::new();
    };
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
            None => from_view_point(apps, cohort, observation, segment),
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

/// The point of `apps`, the Apps of a second or of a stretch, the tick `observation` names, as
/// the chart whose own lines draw `cohort` shows it in segment `segment`.
fn from_view_point(
    apps: OverviewApps,
    cohort: &[Arc<RankedApp>],
    observation: Observation,
    segment: u64,
) -> Point {
    let (resident, available, completeness) = match apps {
        OverviewApps::Second(apps) => (
            apps.resident(cohort),
            apps.missing.is_none(),
            apps.completeness(),
        ),
        OverviewApps::Stretch(stretch) => (
            stretch.resident(cohort),
            stretch.holds_apps(),
            stretch.completeness(),
        ),
    };

    Point {
        observation,
        segment,
        stretch: matches!(apps, OverviewApps::Stretch(_)),
        source_available: available,
        membership_complete: available.then_some(completeness.membership_complete),
        ungrouped_members: available.then_some(completeness.live_ungrouped_members),
        usage_refused: available.then_some(completeness.usage_refused),
        usage_gone: available.then_some(completeness.usage_gone),
        apps: resident.apps[..cohort.len()]
            .iter()
            .map(|measure| measure.as_ref().map(Measure::of))
            .collect(),
        other: resident.other.as_ref().map(|other| Other {
            app_count: other.app_count,
            resident_bytes: other.resident.as_ref().map(Measure::of),
        }),
    }
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
