//! Overview's chart of resident memory by App drawn from the store's View, point for point as
//! the worker's history of resident memory draws it, so Overview draws it without asking the
//! worker.

use std::sync::Arc;

use viewer::live_app_response::{KeyKind, RankedApp};
use viewer::store::{Cohorts, View};

use super::cohort_labels;
use super::{History, Measure, Observation, Other, Point, ResidentHistory};
use crate::live_app_store_points::{self, KeptPoint};
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
        let drawn = crate::live_projection::overview_app_points(view)?;

        let cohort = &cohorts.resident;
        let kept = older.map_or_else(Vec::new, |older| {
            live_app_store_points::kept(older.view_cohort.as_deref(), older.points, cohort)
        });
        let (positions, points) =
            live_app_store_points::points(&drawn.points, kept, |_, apps, observation, segment| {
                from_view_point(apps, cohort, observation, segment)
            });
        let Some(selected_index) = points.len().checked_sub(1) else {
            unreachable!("the chart draws the View's newest second");
        };

        Ok(Self {
            points,
            cohort: from_view_labels(cohort)?,
            positions,
            window: drawn.window,
            detail_first: Some(drawn.detail_first),
            selected_index,
            retention_limited: drawn.points.iter().any(OverviewAppPoint::retention_limited),
            view_cohort: Some(cohort.clone()),
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
