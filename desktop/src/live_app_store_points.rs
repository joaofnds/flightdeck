//! The points Overview's App charts draw from the store's View: where each breaks its line, and
//! which points of the chart of an older View a newer chart takes rather than draws again.

use std::iter::Peekable;
use std::sync::Arc;
use std::vec::IntoIter;

use viewer::live_app_response::RankedApp;
use viewer::store::MachineSecond;

use crate::live_projection::{OverviewAppPoint, OverviewApps};
use crate::live_resident_history::{Observation, same_apps};

/// A point of an App chart drawn from a View, which the chart of a newer View may take.
pub(crate) trait KeptPoint {
    /// The tick that names the second or the stretch the point stands for.
    fn observation(&self) -> &Observation;
    /// Whether the point stands for a stretch of the older part of the chart rather than for
    /// one second.
    fn stretch(&self) -> bool;
    /// The point, drawn in segment `segment`.
    fn in_segment(self, segment: u64) -> Self;
}

/// The points of an older chart, `points`, which drew the Apps of `older_cohort` on lines of
/// their own, that a chart drawing those of `cohort` takes: all of them where both draw the same
/// Apps, and none otherwise.
pub(crate) fn kept<P: KeptPoint>(
    older_cohort: Option<&[Arc<RankedApp>]>,
    points: Vec<P>,
    cohort: &[Arc<RankedApp>],
) -> Vec<P> {
    let Some(older_cohort) = older_cohort else {
        return Vec::new();
    };
    if !same_apps(older_cohort, cohort) {
        return Vec::new();
    }

    points
}

/// The position and the point of each of `drawn` that shows Apps, breaking the line where the
/// machine charts break theirs, where two seconds are not consecutive, and where a point shows
/// none. A point that `kept`, the points of an older chart, holds takes its point from there,
/// and `make` draws each other from its machine readings, its Apps, the tick that names it and
/// its segment.
pub(crate) fn points<P: KeptPoint>(
    drawn: &[OverviewAppPoint],
    kept: Vec<P>,
    make: impl Fn(&MachineSecond, OverviewApps, Observation, u64) -> P,
) -> (Vec<f32>, Vec<P>) {
    let mut positions = Vec::with_capacity(drawn.len());
    let mut points = Vec::with_capacity(drawn.len());

    let mut kept = Kept::of(kept, drawn);
    let mut segment = 0;
    let mut machine_segment_previous = None;
    for point in drawn {
        let Some(apps) = point.apps else {
            machine_segment_previous = None;
            continue;
        };
        let (observation, follows) = match apps {
            OverviewApps::Second(apps) => (apps.observation, apps.consecutive),
            OverviewApps::Stretch(stretch) => (stretch.observation, true),
        };
        let Some(observation) = &observation else {
            unreachable!("the store names the tick of every second and stretch it takes in");
        };
        if !points.is_empty() {
            if machine_segment_previous != Some(point.segment) || !follows {
                segment += 1;
            }
        }
        machine_segment_previous = Some(point.segment);

        let observation = Observation::of(observation);
        positions.push(point.position);
        let chart_point = match kept.take(apps, &observation) {
            Some(taken) => taken.in_segment(segment),
            None => make(point.machine, apps, observation, segment),
        };
        points.push(chart_point);
    }

    assert_eq!(points.len(), positions.len());
    (positions, points)
}

/// The points of an older chart that a newer one takes, the stretches' and the seconds' apart,
/// each from the one at the newer chart's first of its kind on.
struct Kept<P> {
    stretches: Peekable<IntoIter<P>>,
    seconds: Peekable<IntoIter<P>>,
}

impl<P: KeptPoint> Kept<P> {
    /// The points of `kept` that the chart of `drawn` takes, since a newer chart starts each
    /// kind no earlier than the older chart `kept` came from, and none of a kind where `kept`
    /// does not hold the newer chart's first.
    fn of(kept: Vec<P>, drawn: &[OverviewAppPoint]) -> Self {
        let (stretches, seconds): (Vec<P>, Vec<P>) = kept.into_iter().partition(KeptPoint::stretch);
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
    fn of_from(mut kept: Vec<P>, first: Option<viewer::live_app_response::Observation>) -> Vec<P> {
        let Some(first) = first else {
            return Vec::new();
        };
        let first = Observation::of(&first);
        let Some(start) = kept.iter().position(|point| *point.observation() == first) else {
            return Vec::new();
        };

        kept.drain(..start);
        kept
    }

    /// The kept point of `apps`, the tick `observation` names, where the older chart drew it.
    fn take(&mut self, apps: OverviewApps, observation: &Observation) -> Option<P> {
        let kept = match apps {
            OverviewApps::Second(_) => &mut self.seconds,
            OverviewApps::Stretch(_) => &mut self.stretches,
        };

        kept.next_if(|point| point.observation() == observation)
    }
}
