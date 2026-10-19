use persistence::{Error, Result};
use serde_json::{Value, json};

use super::means::{APPS_MAX, KEY_BYTES_MAX, Means};
use crate::apps::AppKey;
use crate::cancellation::Operation;
use crate::live_app_response::{Measure, Observation};
use crate::session::OBSERVATIONS_MAX;

pub(crate) struct Reduction<'operation> {
    selected: Observation,
    operation: &'operation Operation,
    means: Means,
    cohort: Vec<u16>,
    points: Vec<Point>,
    missing: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct Capture {
    pub observation: Observation,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    pub usage_refused: u32,
    pub usage_gone: u32,
}

pub(crate) struct AppReading<'key> {
    pub key: AppKey<'key>,
    pub resident: Measure,
}

struct Point {
    capture: Capture,
    apps: [Option<Measure>; 8],
    other: Measure,
    other_count: u32,
    projected: bool,
    source: Source,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Live,
    Saved,
    Missing,
}

type Identity = ([u64; 2], [u64; 2], [u64; 2], u64);

pub(crate) struct Interval<'observation> {
    pub before: &'observation Observation,
    pub after: &'observation Observation,
}

const POINT_BYTES_MAX: usize = 2 * 1024 * 1024;
const EMPTY: Measure = Measure {
    known: 0,
    measured_members: 0,
    unavailable_members: 0,
    overflowed: false,
};
const _: () = {
    assert!(size_of::<Capture>() <= 128);
    assert!(size_of::<Point>() <= 496);
    assert!(OBSERVATIONS_MAX * size_of::<Point>() + size_of::<Vec<Point>>() <= POINT_BYTES_MAX);
};

impl<'operation> Reduction<'operation> {
    pub(crate) fn contains_live(&self, observation: &Observation) -> Result<bool> {
        if let Ok(index) = self
            .points
            .binary_search_by_key(&identity(observation), |point| {
                identity(&point.capture.observation)
            })
        {
            let point = &self.points[index];
            if point.capture.observation != *observation {
                return Err(Error::Bound("conflicting resident history observation"));
            }
            return Ok(point.source == Source::Live);
        }
        Ok(false)
    }
    pub(crate) fn new(selected: &Observation, operation: &'operation Operation) -> Result<Self> {
        if selected.wall_ns.is_none() {
            return Err(Error::Bound("resident history selected clock"));
        }

        Ok(Self {
            selected: *selected,
            operation,
            means: Means::new(),
            cohort: Vec::with_capacity(8),
            points: Vec::with_capacity(OBSERVATIONS_MAX),
            missing: false,
        })
    }

    pub(crate) fn observe<'key>(
        &mut self,
        capture: &Capture,
        apps: impl Iterator<Item = AppReading<'key>>,
        source: Source,
    ) -> Result<()> {
        self.operation.check()?;
        if !self.in_range(&capture.observation)? {
            return Ok(());
        }

        let index = self
            .points
            .binary_search_by_key(&identity(&capture.observation), |point| {
                identity(&point.capture.observation)
            });
        let index = match index {
            Ok(index) => {
                let point = &self.points[index];
                if point.capture.observation != capture.observation {
                    return Err(Error::Bound("conflicting resident history observation"));
                }
                if source == Source::Saved {
                    if point.source == Source::Live {
                        return Ok(());
                    }
                }

                return Err(Error::Bound("duplicate resident history observation"));
            }
            Err(index) => index,
        };
        if self.points.len() == OBSERVATIONS_MAX {
            return Err(Error::Bound("resident history observations"));
        }

        self.points.insert(
            index,
            Point {
                capture: *capture,
                apps: [None; 8],
                other: EMPTY,
                other_count: 0,
                projected: false,
                source,
            },
        );
        for (index, app) in apps.enumerate() {
            self.operation.check()?;
            if index == APPS_MAX {
                return Err(Error::Bound("resident history observation Apps"));
            }

            self.means.observe(&app.key, &app.resident)?;
        }

        Ok(())
    }

    pub(crate) fn choose(&mut self) -> Result<()> {
        self.cohort = self.means.choose(self.operation)?;
        Ok(())
    }

    pub(crate) fn project<'key>(
        &mut self,
        capture: &Capture,
        apps: impl Iterator<Item = AppReading<'key>>,
        source: Source,
    ) -> Result<()> {
        self.operation.check()?;
        if !self.in_range(&capture.observation)? {
            return Ok(());
        }

        let index = self
            .points
            .binary_search_by_key(&identity(&capture.observation), |point| {
                identity(&point.capture.observation)
            })
            .map_err(|_| Error::Bound("resident history projection observation"))?;
        let point = &mut self.points[index];
        if point.capture.observation != capture.observation {
            return Err(Error::Bound("resident history changed observation"));
        }
        if point.source != source {
            return Ok(());
        }
        if point.projected {
            return Err(Error::Bound("duplicate resident history projection"));
        }

        for (index, app) in apps.enumerate() {
            self.operation.check()?;
            if index == APPS_MAX {
                return Err(Error::Bound("resident history projection Apps"));
            }

            if let Some(index) = self
                .cohort
                .iter()
                .position(|index| self.means.key(*index) == app.key)
            {
                point.apps[index] = Some(app.resident);
            } else {
                point.other_count += 1;
                add(&mut point.other, &app.resident)?;
            }
        }
        point.projected = true;

        Ok(())
    }

    pub(crate) fn response(mut self) -> Result<Value> {
        self.operation.check()?;
        self.points.sort_by_key(|point| {
            (
                point.capture.observation.wall_ns,
                identity(&point.capture.observation),
            )
        });
        let cohort: Vec<_> = self
            .cohort
            .iter()
            .map(|index| self.means.json(*index))
            .collect();
        let mut points = Vec::with_capacity(self.points.len());
        let mut previous = None;
        let mut segment = 0_u64;
        for point in &self.points {
            self.operation.check()?;
            if !point.projected {
                return Err(Error::Bound("resident history unprojected observation"));
            }
            if let Some(before) = previous {
                if !consecutive(&Interval {
                    before,
                    after: &point.capture.observation,
                }) {
                    segment += 1;
                }
            }

            points.push(point.json(self.cohort.len(), segment));
            previous = Some(&point.capture.observation);
        }

        crate::live_app_response::bounded(json!({"selected":self.selected.json(),
            "context_seconds":900,"cohort":cohort,"points":points,"coverage":{
                "retention_limited":self.missing,
                "context_app_count":self.means.count(),"context_app_count_max":APPS_MAX,
                "context_key_bytes":self.means.key_bytes(),"context_key_bytes_max":KEY_BYTES_MAX}}))
    }

    pub(crate) fn retain_metadata(&mut self, observation: &Observation) -> Result<()> {
        self.operation.check()?;
        if !self.in_range(observation)? {
            return Ok(());
        }

        match self
            .points
            .binary_search_by_key(&identity(observation), |point| {
                identity(&point.capture.observation)
            }) {
            Ok(index) => {
                if self.points[index].capture.observation != *observation {
                    return Err(Error::Bound("resident history metadata conflict"));
                }
            }
            Err(index) => {
                if self.points.len() == OBSERVATIONS_MAX {
                    return Err(Error::Bound("resident history metadata observations"));
                }

                self.points.insert(
                    index,
                    Point {
                        capture: Capture {
                            observation: *observation,
                            membership_complete: false,
                            ungrouped_members: 0,
                            usage_refused: 0,
                            usage_gone: 0,
                        },
                        apps: [None; 8],
                        other: EMPTY,
                        other_count: 0,
                        projected: true,
                        source: Source::Missing,
                    },
                );
                self.missing = true;
            }
        }

        Ok(())
    }

    fn in_range(&self, observation: &Observation) -> Result<bool> {
        if self.selected.recording != observation.recording {
            return Err(Error::Bound("resident history recording"));
        }

        let second = self
            .selected
            .wall_ns
            .ok_or(Error::Bound("resident history selected clock"))?
            / 1_000_000_000;
        let wall = observation
            .wall_ns
            .ok_or(Error::Bound("resident history point clock"))?
            / 1_000_000_000;

        Ok((second.saturating_sub(899)..=second).contains(&wall))
    }
}

impl Point {
    fn json(&self, count: usize, segment: u64) -> Value {
        let apps: Vec<_> = self.apps[..count]
            .iter()
            .map(|app| app.map(|app| app.json()))
            .collect();
        if self.source == Source::Missing {
            return json!({"observation":self.capture.observation.json(),
                "segment":segment.to_string(),"source_available":false,
                "membership_complete":null,"ungrouped_members":null,
                "usage_refused":null,"usage_gone":null,"apps":apps,"other":null});
        }

        let other = if self.other_count > 0 {
            Some(self.other.json())
        } else {
            if self.capture.membership_complete {
                if self.capture.ungrouped_members == 0 {
                    if self.capture.usage_refused == 0 {
                        if self.capture.usage_gone == 0 {
                            Some(EMPTY.json())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };

        json!({"observation":self.capture.observation.json(),"segment":segment.to_string(),
            "source_available":true,"membership_complete":self.capture.membership_complete,
            "ungrouped_members":self.capture.ungrouped_members,
            "usage_refused":self.capture.usage_refused,"usage_gone":self.capture.usage_gone,
            "apps":apps,"other":{"app_count":self.other_count,"resident_bytes":other}})
    }
}

pub(crate) fn identity(observation: &Observation) -> Identity {
    (
        observation.recording,
        observation.stamp.boot,
        observation.stamp.run,
        observation.stamp.publication,
    )
}

fn add(total: &mut Measure, value: &Measure) -> Result<()> {
    if let Some(known) = total.known.checked_add(value.known) {
        total.known = known;
    } else {
        total.overflowed = true;
    }
    total.overflowed |= value.overflowed;
    total.measured_members = total
        .measured_members
        .checked_add(value.measured_members)
        .ok_or(Error::Bound("resident history measured members"))?;
    total.unavailable_members = total
        .unavailable_members
        .checked_add(value.unavailable_members)
        .ok_or(Error::Bound("resident history unavailable members"))?;

    Ok(())
}

pub(crate) fn consecutive(interval: &Interval<'_>) -> bool {
    let Interval { before, after } = interval;
    if before.recording != after.recording {
        return false;
    }
    if before.stamp.boot != after.stamp.boot {
        return false;
    }
    if before.stamp.run != after.stamp.run {
        return false;
    }
    if before.stamp.publication.checked_add(1) != Some(after.stamp.publication) {
        return false;
    }
    if before.run_tick.checked_add(1) != Some(after.run_tick) {
        return false;
    }
    let (Some(first), Some(last)) = (before.wall_ns, after.wall_ns) else {
        return false;
    };
    if first >= last {
        return false;
    }
    if last / 1_000_000_000 > first / 1_000_000_000 + 1 {
        return false;
    }
    let (Some(first), Some(last)) = (before.stamp.continuous_ns, after.stamp.continuous_ns) else {
        return false;
    };
    if first >= last {
        return false;
    }
    let (Some(first), Some(last)) = (before.stamp.awake_ns, after.stamp.awake_ns) else {
        return false;
    };

    first <= last
}
