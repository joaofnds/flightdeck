//! Compact App facts retained independently of the bounded raw process cache.
use std::collections::VecDeque;

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::apps::{self, AppKey};
use crate::cancellation::Operation;
use crate::live_app_response::{KeyKind, Measure, Observation, Sample};
use crate::session::OBSERVATIONS_MAX;

const BYTES_MAX: usize = 32 * 1024 * 1024;
const _: () = {
    assert!(OBSERVATIONS_MAX > 0);
    assert!(
        BYTES_MAX
            > crate::live_app_response::SAMPLE_BYTES_MAX
                + OBSERVATIONS_MAX * size_of::<Box<Point>>()
                + size_of::<History>()
    );
};

pub(super) struct History {
    points: VecDeque<Box<Point>>,
    bytes: usize,
    retention_limited: bool,
}

pub(super) struct Point {
    observation: Observation,
    segment: u64,
    elapsed_ns: Option<u64>,
    apps: Vec<App>,
}

struct App {
    kind: KeyKind,
    key: Vec<u8>,
    members: u32,
    membership_complete: bool,
    cpu_ns: Measure,
    energy_nj: Measure,
    resident_bytes: Measure,
    footprint_bytes: Measure,
}

pub(super) struct Target<'bytes> {
    pub kind: KeyKind,
    pub bytes: &'bytes [u8],
}

impl History {
    pub(super) fn new() -> Self {
        let points = VecDeque::with_capacity(OBSERVATIONS_MAX);
        let bytes = size_of::<Self>() + points.capacity() * size_of::<Box<Point>>();
        assert!(bytes < BYTES_MAX);

        Self {
            points,
            bytes,
            retention_limited: false,
        }
    }

    pub(super) fn push(&mut self, point: Box<Point>, first: u64) {
        let bytes = point.bytes();
        for _attempt in 0..=OBSERVATIONS_MAX {
            let expired = self
                .points
                .front()
                .is_some_and(|point| point.observation.wall_ns.is_some_and(|wall| wall < first));
            let count_limited = self.points.len() == OBSERVATIONS_MAX;
            let bytes_limited = self.bytes + bytes > BYTES_MAX;
            if !expired {
                if !count_limited {
                    if !bytes_limited {
                        self.bytes += bytes;
                        self.points.push_back(point);
                        assert!(self.bytes <= BYTES_MAX);
                        return;
                    }
                }
            }
            let removed = self
                .points
                .pop_front()
                .unwrap_or_else(|| unreachable!("one bounded App point fits the empty cache"));
            self.bytes -= removed.bytes();
            if !expired {
                self.retention_limited = true;
            }
        }

        unreachable!("a bounded cache can evict all prior points");
    }

    pub(super) fn response(
        &self,
        selected: &Observation,
        target: &Target<'_>,
        operation: &Operation,
    ) -> Result<Value> {
        let second = selected
            .wall_ns
            .ok_or(Error::Bound("live App history clock"))?
            / 1_000_000_000;
        if target.bytes.len() > collector::ring::PROGRAM_PATH_BYTES_MAX {
            return Err(Error::Bound("live App history key"));
        }

        let mut points = Vec::with_capacity(self.points.len());
        for point in &self.points {
            operation.check()?;
            let wall = point
                .observation
                .wall_ns
                .ok_or(Error::Bound("live App history clock"))?;
            if (second.saturating_sub(899)..=second).contains(&(wall / 1_000_000_000)) {
                points.push(point);
            }
        }
        points.sort_by_key(|point| {
            (
                point.observation.wall_ns,
                point.observation.stamp.boot,
                point.observation.stamp.run,
                point.observation.stamp.publication,
            )
        });
        let points: Vec<_> = points.into_iter().map(|point| point.json(target)).collect();
        operation.check()?;

        Ok(
            json!({"selected": selected.json(), "context_seconds": 900, "points": points,
            "coverage": {"retention_limited": self.retention_limited,
                "observations_retained": self.points.len(), "cache_bytes": self.bytes,
                "cache_bytes_max": BYTES_MAX}}),
        )
    }
}

impl Point {
    pub(super) fn capture(sample: &Sample, segment: u64, output: &mut Option<Box<Self>>) {
        *output = None;
        let mut point = Box::new(Self {
            observation: sample.selected,
            segment,
            elapsed_ns: sample.elapsed_ns,
            apps: Vec::with_capacity(sample.apps().len()),
        });
        for app in sample.apps() {
            point.apps.push(App {
                kind: app.key.kind,
                key: app.key.bytes.clone(),
                members: app.members,
                membership_complete: app.membership_complete,
                cpu_ns: app.cpu_ns,
                energy_nj: app.energy_nj,
                resident_bytes: app.resident_bytes,
                footprint_bytes: app.footprint_bytes,
            });
        }
        assert!(point.bytes() <= crate::live_app_response::SAMPLE_BYTES_MAX);
        *output = Some(point);
    }

    fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.apps.capacity() * size_of::<App>()
            + self
                .apps
                .iter()
                .map(|app| app.key.capacity())
                .sum::<usize>()
    }

    fn json(&self, target: &Target<'_>) -> Value {
        let target = key(target.kind, target.bytes);
        let app = match self
            .apps
            .binary_search_by(|app| key(app.kind, &app.key).cmp(&target))
        {
            Ok(index) => Some(&self.apps[index]),
            Err(_) => None,
        };
        let mut point = json!({"observation": self.observation.json(),
            "segment": self.segment.to_string(),
            "elapsed_ns": self.elapsed_ns.map(|value| value.to_string()),
            "members": null, "membership_complete": false,
            "cpu_ns": null, "energy_nj": null, "resident_bytes": null, "footprint_bytes": null,
            "cpu_percent_one_core": null, "energy_joules": null, "energy_watts": null});
        if let Some(app) = app {
            point["members"] = json!(app.members);
            point["membership_complete"] = json!(app.membership_complete);
            point["cpu_ns"] = app.cpu_ns.json();
            point["energy_nj"] = app.energy_nj.json();
            point["resident_bytes"] = app.resident_bytes.json();
            point["footprint_bytes"] = app.footprint_bytes.json();
            point["cpu_percent_one_core"] = json!(app.cpu_ns.measured().and_then(|cpu_ns| {
                apps::cpu_percent_one_core(apps::CpuRate {
                    cpu_ns,
                    elapsed_ns: self.elapsed_ns?,
                })
            }));
            point["energy_joules"] = json!(app.energy_nj.measured().map(apps::energy_joules));
            point["energy_watts"] = json!(app.energy_nj.measured().and_then(|energy_nj| {
                apps::energy_watts(apps::EnergyRate {
                    energy_nj,
                    elapsed_ns: self.elapsed_ns?,
                })
            }));
        }

        point
    }
}

fn key(kind: KeyKind, bytes: &[u8]) -> AppKey<'_> {
    match kind {
        KeyKind::Bundle => AppKey::Bundle(bytes),
        KeyKind::Executable => AppKey::Executable(bytes),
    }
}

#[cfg(test)]
mod tests;
