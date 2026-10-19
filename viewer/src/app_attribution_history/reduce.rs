use persistence::{Error, Result};
use serde_json::{Value, json};

use super::AppAttributionMetric;
use super::means::{Key, Means};
use crate::apps::AppKey;
use crate::cancellation::Operation;
use crate::live::resident_history::{Capture as Coverage, Interval, Source, consecutive, identity};
use crate::live_app_response::{Measure, Observation};

pub(crate) struct Reduction<'operation> {
    selected: Observation,
    metric: AppAttributionMetric,
    operation: &'operation Operation,
    means: Means,
    cohort: Vec<Key>,
    points: Vec<Point>,
    missing: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct Capture {
    pub coverage: Coverage,
    pub elapsed_ns: Option<u64>,
}

#[derive(Clone, Copy)]
pub(crate) struct Reading<'key> {
    pub key: AppKey<'key>,
    pub measure: Measure,
    pub members: u32,
    pub membership_complete: bool,
}

struct Point {
    capture: Capture,
    apps: [Option<App>; 8],
    other: App,
    other_count: u32,
    source: Source,
    projected: bool,
}

#[derive(Clone, Copy)]
struct App {
    measure: Measure,
    members: u32,
    membership_complete: bool,
}

pub(super) const WORKING_BYTES_MAX: usize = super::means::BYTES_MAX
    + 4096 * size_of::<Point>()
    + 8 * (size_of::<Key>() + collector::ring::PROGRAM_PATH_BYTES_MAX)
    + size_of::<Reduction<'static>>();

const EMPTY: Measure = Measure {
    known: 0,
    measured_members: 0,
    unavailable_members: 0,
    overflowed: false,
};
const _: () = {
    assert!(size_of::<Point>() <= 640);
    assert!(4096 * size_of::<Point>() <= 3 * 1024 * 1024);
};

impl<'operation> Reduction<'operation> {
    pub(crate) fn new(
        selected: &Observation,
        metric: AppAttributionMetric,
        operation: &'operation Operation,
    ) -> Result<Self> {
        if selected.wall_ns.is_none() {
            return Err(Error::Bound("attribution selected clock"));
        }
        Ok(Self {
            selected: *selected,
            metric,
            operation,
            means: Means::new(),
            cohort: Vec::with_capacity(8),
            points: Vec::with_capacity(4096),
            missing: false,
        })
    }

    pub(crate) fn contains_live(&self, observation: &Observation) -> Result<bool> {
        match self
            .points
            .binary_search_by_key(&identity(observation), |point| {
                identity(&point.capture.coverage.observation)
            }) {
            Ok(index) => {
                if self.points[index].capture.coverage.observation != *observation {
                    return Err(Error::Bound("conflicting attribution observation"));
                }
                Ok(self.points[index].source == Source::Live)
            }
            Err(_) => Ok(false),
        }
    }

    pub(crate) fn observe<'key>(
        &mut self,
        capture: &Capture,
        apps: impl Iterator<Item = Reading<'key>>,
        source: Source,
    ) -> Result<()> {
        self.operation.check()?;
        if !self.in_range(&capture.coverage.observation)? {
            return Ok(());
        }
        let index = self
            .points
            .binary_search_by_key(&identity(&capture.coverage.observation), |point| {
                identity(&point.capture.coverage.observation)
            });
        let index = match index {
            Ok(index) => {
                if self.points[index].capture.coverage.observation != capture.coverage.observation {
                    return Err(Error::Bound("conflicting attribution observation"));
                }
                if source == Source::Saved {
                    if self.points[index].source == Source::Live {
                        return Ok(());
                    }
                }
                return Err(Error::Bound("duplicate attribution observation"));
            }
            Err(index) => index,
        };
        if self.points.len() == 4096 {
            return Err(Error::Bound("attribution observations"));
        }
        self.points.insert(index, Point::new(capture, source));
        for (index, app) in apps.enumerate() {
            self.operation.check()?;
            if index == 24_000 {
                return Err(Error::Bound("attribution observation Apps"));
            }
            self.means.observe(&app, capture.elapsed_ns)?;
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
        apps: impl Iterator<Item = Reading<'key>>,
        source: Source,
    ) -> Result<()> {
        self.operation.check()?;
        if !self.in_range(&capture.coverage.observation)? {
            return Ok(());
        }
        let index = self
            .points
            .binary_search_by_key(&identity(&capture.coverage.observation), |point| {
                identity(&point.capture.coverage.observation)
            })
            .map_err(|_| Error::Bound("attribution projection observation"))?;
        let point = &mut self.points[index];
        if point.capture.coverage.observation != capture.coverage.observation {
            return Err(Error::Bound("attribution changed observation"));
        }
        if point.source != source {
            return Ok(());
        }
        if point.projected {
            return Err(Error::Bound("duplicate attribution projection"));
        }
        for (index, app) in apps.enumerate() {
            self.operation.check()?;
            if index == 24_000 {
                return Err(Error::Bound("attribution projection Apps"));
            }
            let value = App {
                measure: app.measure,
                members: app.members,
                membership_complete: app.membership_complete,
            };
            if let Some(index) = self.cohort.iter().position(|key| key.view() == app.key) {
                point.apps[index] = Some(value);
            } else {
                point.other_count += 1;
                point.other.add(&value)?;
            }
        }
        point.projected = true;
        Ok(())
    }

    pub(crate) fn retain_metadata(&mut self, observation: &Observation) -> Result<()> {
        self.operation.check()?;
        if !self.in_range(observation)? {
            return Ok(());
        }
        match self
            .points
            .binary_search_by_key(&identity(observation), |point| {
                identity(&point.capture.coverage.observation)
            }) {
            Ok(index) => {
                if self.points[index].capture.coverage.observation != *observation {
                    return Err(Error::Bound("attribution metadata conflict"));
                }
            }
            Err(index) => {
                if self.points.len() == 4096 {
                    return Err(Error::Bound("attribution metadata"));
                }
                let mut point = Point::new(
                    &Capture {
                        coverage: Coverage {
                            observation: *observation,
                            membership_complete: false,
                            ungrouped_members: 0,
                            usage_refused: 0,
                            usage_gone: 0,
                        },
                        elapsed_ns: None,
                    },
                    Source::Missing,
                );
                point.projected = true;
                self.points.insert(index, point);
                self.missing = true;
            }
        }
        Ok(())
    }

    pub(crate) fn response(mut self) -> Result<Value> {
        self.points.sort_by_key(|point| {
            (
                point.capture.coverage.observation.wall_ns,
                identity(&point.capture.coverage.observation),
            )
        });
        let mut points = Vec::with_capacity(self.points.len());
        let mut previous = None;
        let mut segment = 0_u64;
        for point in &self.points {
            self.operation.check()?;
            if !point.projected {
                return Err(Error::Bound("unprojected attribution observation"));
            }
            if let Some(before) = previous {
                if !consecutive(&Interval {
                    before,
                    after: &point.capture.coverage.observation,
                }) {
                    segment += 1;
                }
            }
            points.push(point.json(self.metric, self.cohort.len(), segment));
            previous = Some(&point.capture.coverage.observation);
        }
        let cohort: Vec<_> = self.cohort.iter().map(Key::json).collect();
        // The json! macro copies each embedded value, so the built values move in afterwards.
        let mut response = json!({"metric":self.metric.name(),
            "selected":null,"context_seconds":900,"cohort":null,"points":null,
            "coverage":{"retention_limited":self.missing,"context_app_count":self.means.count(),
                "context_app_count_max":24000,"context_key_bytes":self.means.bytes(),
                "context_key_bytes_max":8_388_608}});
        response["selected"] = self.selected.json();
        response["cohort"] = Value::Array(cohort);
        response["points"] = Value::Array(points);

        // The caller checks the bound once it has added its own coverage.
        Ok(response)
    }

    fn in_range(&self, observation: &Observation) -> Result<bool> {
        if self.selected.recording != observation.recording {
            return Err(Error::Bound("attribution recording"));
        }
        let selected = self
            .selected
            .wall_ns
            .ok_or(Error::Bound("attribution selected clock"))?
            / 1_000_000_000;
        let second = observation
            .wall_ns
            .ok_or(Error::Bound("attribution point clock"))?
            / 1_000_000_000;
        Ok((selected.saturating_sub(899)..=selected).contains(&second))
    }
}

impl Point {
    fn new(capture: &Capture, source: Source) -> Self {
        Self {
            capture: *capture,
            apps: [None; 8],
            other: App {
                measure: EMPTY,
                members: 0,
                membership_complete: true,
            },
            other_count: 0,
            source,
            projected: false,
        }
    }

    fn json(&self, metric: AppAttributionMetric, count: usize, segment: u64) -> Value {
        let apps: Vec<_> = self.apps[..count]
            .iter()
            .map(|app| app.map_or(Value::Null, |app| app.json(metric, self.capture.elapsed_ns)))
            .collect();
        let capture = &self.capture.coverage;
        // The json! macro copies each embedded value, so the built values move in afterwards.
        if self.source == Source::Missing {
            let mut point = json!({"observation":null,"segment":segment.to_string(),
                "elapsed_ns":null,"source_available":false,"membership_complete":null,
                "ungrouped_members":null,"usage_refused":null,"usage_gone":null,
                "apps":null,"other":null});
            point["observation"] = capture.observation.json();
            point["apps"] = Value::Array(apps);
            return point;
        }
        let mut other = self.other.json(metric, self.capture.elapsed_ns);
        other["app_count"] = json!(self.other_count);
        if self.other_count == 0 {
            let complete = [
                capture.membership_complete,
                capture.ungrouped_members == 0,
                capture.usage_refused == 0,
                capture.usage_gone == 0,
            ]
            .iter()
            .all(|value| *value);
            if complete {
                other[metric.rate_name()] = json!(metric.rate(0, self.capture.elapsed_ns));
            } else {
                other[metric.amount()] = Value::Null;
                other[metric.rate_name()] = Value::Null;
            }
        }
        let mut point = json!({"observation":null,"segment":segment.to_string(),
            "elapsed_ns":self.capture.elapsed_ns.map(|value| value.to_string()),
            "source_available":true,"membership_complete":capture.membership_complete,
            "ungrouped_members":capture.ungrouped_members,"usage_refused":capture.usage_refused,
            "usage_gone":capture.usage_gone,"apps":null,"other":null});
        point["observation"] = capture.observation.json();
        point["apps"] = Value::Array(apps);
        point["other"] = other;

        point
    }
}

impl App {
    fn json(&self, metric: AppAttributionMetric, elapsed: Option<u64>) -> Value {
        let mut value = json!({"members":self.members,
            "membership_complete":self.membership_complete});
        value[metric.amount()] = self.measure.json();
        value[metric.rate_name()] = json!(
            self.measure
                .measured()
                .and_then(|amount| metric.rate(amount, elapsed))
        );
        value
    }

    fn add(&mut self, app: &Self) -> Result<()> {
        if let Some(known) = self.measure.known.checked_add(app.measure.known) {
            self.measure.known = known;
        } else {
            self.measure.overflowed = true;
        }
        self.measure.overflowed |= app.measure.overflowed;
        self.measure.measured_members = self
            .measure
            .measured_members
            .checked_add(app.measure.measured_members)
            .ok_or(Error::Bound("attribution members"))?;
        self.measure.unavailable_members = self
            .measure
            .unavailable_members
            .checked_add(app.measure.unavailable_members)
            .ok_or(Error::Bound("attribution members"))?;
        self.members = self
            .members
            .checked_add(app.members)
            .ok_or(Error::Bound("attribution total members"))?;
        self.membership_complete &= app.membership_complete;
        Ok(())
    }
}
