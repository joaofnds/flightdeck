use std::collections::{BTreeMap, BTreeSet};

use collector::recording::parse_identity;
use persistence::{Error, Result};
use serde_json::{Value, json};

use super::{BYTES_MAX, History, OBSERVATIONS_MAX, Observation, Selection, TracePoint};
use crate::cancellation::Operation;
use crate::cpu_history_sample::Identity;

type PointKey = ([u64; 2], [u64; 2], [u64; 2], u64);
type Points = BTreeMap<PointKey, (TracePoint, bool)>;

pub(crate) struct Merge<'a> {
    pub selected: &'a Observation,
    pub target: &'a dyn Selection,
    pub saved: &'a [TracePoint],
    pub context: &'a [Identity],
    pub operation: &'a Operation,
}

#[derive(Clone, Copy)]
struct Range<'a> {
    selected: &'a Observation,
    observation: &'a Observation,
}

#[derive(Clone, Copy)]
struct Interval<'a> {
    previous: &'a Observation,
    current: &'a Observation,
}

impl History {
    pub(crate) fn response_merged(&self, request: &Merge<'_>) -> Result<Value> {
        request.operation.check()?;
        if request.target.app().bytes.len() > collector::ring::PROGRAM_PATH_BYTES_MAX {
            return Err(Error::Bound("App history key"));
        }

        if request.saved.len() > OBSERVATIONS_MAX {
            return Err(Error::Bound("App history saved points"));
        }

        let mut points = Points::new();
        for point in request.saved {
            request.operation.check()?;
            if in_range(Range {
                selected: request.selected,
                observation: &point.observation,
            })? {
                insert(&mut points, point, false)?;
            }
        }

        for point in &self.points {
            request.operation.check()?;
            if in_range(Range {
                selected: request.selected,
                observation: &point.observation,
            })? {
                let trace = point.trace(request.target);
                if !trace.source_available() {
                    if points.contains_key(&insert_point_key(&trace.observation)) {
                        continue;
                    }
                }
                insert(&mut points, &trace, true)?;
            }
        }

        let missing = if response_merged_context_missing(request, &mut points)? {
            true
        } else {
            points
                .values()
                .any(|(point, _live)| !point.source_available())
        };
        let mut points: Vec<_> = points.into_values().map(|(point, _live)| point).collect();

        points.sort_by_key(|point| {
            (
                point.observation.wall_ns,
                point.observation.stamp.boot,
                point.observation.stamp.run,
                point.observation.stamp.publication,
            )
        });
        let count = points.len();
        let points = response_merged_project(&points, request.operation)?;
        request.operation.check()?;

        let response = crate::live_app_response::bounded(json!({
            "selected": request.selected.json(), "context_seconds": 900, "points": points,
            "coverage": {"retention_limited": missing,
                "live_retention_limited": self.retention_limited,
                "observations_retained": count, "cache_bytes": self.total_bytes(),
                "cache_bytes_max": BYTES_MAX, "committed_observations": request.saved.len()}
        }))?;
        request.operation.check()?;

        Ok(response)
    }
}

fn insert(points: &mut Points, point: &TracePoint, live: bool) -> Result<()> {
    let key = insert_point_key(&point.observation);
    if let Some((existing, existing_live)) = points.get(&key) {
        if *existing_live == live {
            return Err(Error::Bound("duplicate App history observation"));
        }

        if existing.observation != point.observation {
            return Err(Error::Bound("conflicting App history observation"));
        }
    } else {
        if points.len() == OBSERVATIONS_MAX {
            return Err(Error::Bound("App history merged observations"));
        }
    }

    points.insert(key, (*point, live));

    Ok(())
}

fn insert_point_key(observation: &Observation) -> PointKey {
    (
        observation.recording,
        observation.stamp.boot,
        observation.stamp.run,
        observation.stamp.publication,
    )
}

fn in_range(range: Range<'_>) -> Result<bool> {
    let Range {
        selected,
        observation,
    } = range;
    if selected.recording != observation.recording {
        return Err(Error::Bound("App history recording identity"));
    }

    let second = selected
        .wall_ns
        .ok_or(Error::Bound("App history selected clock"))?
        / 1_000_000_000;
    let wall = observation
        .wall_ns
        .ok_or(Error::Bound("App history point clock"))?
        / 1_000_000_000;

    Ok((second.saturating_sub(899)..=second).contains(&wall))
}

fn response_merged_context_missing(request: &Merge<'_>, points: &mut Points) -> Result<bool> {
    if request.context.len() > OBSERVATIONS_MAX {
        return Err(Error::Bound("App history context observations"));
    }

    let mut seen = BTreeSet::new();
    let mut missing = false;
    for identity in request.context {
        request.operation.check()?;
        let key = (
            request.selected.recording,
            parse_identity(&identity.boot)?.0,
            parse_identity(&identity.run)?.0,
            identity.observation.publication,
        );
        if !seen.insert(key) {
            return Err(Error::Bound("duplicate App history context"));
        }

        if let Some((point, _live)) = points.get(&key) {
            let observation = &identity.observation;
            if point.observation.run_tick != observation.run_tick {
                return Err(Error::Bound("App history context run tick"));
            }

            if point.observation.wall_ns != Some(observation.wall) {
                return Err(Error::Bound("App history context wall clock"));
            }

            if point.observation.stamp.continuous_ns != observation.continuous {
                return Err(Error::Bound("App history context continuous clock"));
            }
        } else {
            let observation = Observation {
                recording: request.selected.recording,
                stamp: crate::apps::Stamp {
                    boot: key.1,
                    run: key.2,
                    publication: key.3,
                    continuous_ns: identity.observation.continuous,
                    awake_ns: None,
                },
                run_tick: identity.observation.run_tick,
                wall_ns: Some(identity.observation.wall),
            };
            if !in_range(Range {
                selected: request.selected,
                observation: &observation,
            })? {
                return Err(Error::Bound("App history context range"));
            }

            insert(points, &TracePoint::unavailable(&observation), false)?;
            missing = true;
        }
    }

    Ok(missing)
}

fn response_merged_project(points: &[TracePoint], operation: &Operation) -> Result<Vec<Value>> {
    assert!(points.len() <= OBSERVATIONS_MAX);

    let mut output = Vec::with_capacity(points.len());
    let mut previous = None;
    let mut segment = 0_u64;
    for point in points {
        operation.check()?;
        if let Some(previous) = previous {
            if !response_merged_project_consecutive(Interval {
                previous,
                current: &point.observation,
            }) {
                segment += 1;
            }
        }

        output.push(point.json(segment));
        previous = Some(&point.observation);
    }

    Ok(output)
}

fn response_merged_project_consecutive(interval: Interval<'_>) -> bool {
    let Interval { previous, current } = interval;
    if previous.recording != current.recording {
        return false;
    }

    if previous.stamp.boot != current.stamp.boot {
        return false;
    }

    if previous.stamp.run != current.stamp.run {
        return false;
    }

    if previous.stamp.publication.checked_add(1) != Some(current.stamp.publication) {
        return false;
    }

    if previous.run_tick.checked_add(1) != Some(current.run_tick) {
        return false;
    }

    let (Some(before), Some(after)) = (previous.wall_ns, current.wall_ns) else {
        return false;
    };

    if before >= after {
        return false;
    }

    if after / 1_000_000_000 > before / 1_000_000_000 + 1 {
        return false;
    }

    let (Some(before), Some(after)) = (previous.stamp.continuous_ns, current.stamp.continuous_ns)
    else {
        return false;
    };

    if before >= after {
        return false;
    }

    let (Some(before), Some(after)) = (previous.stamp.awake_ns, current.stamp.awake_ns) else {
        return false;
    };

    before <= after
}
