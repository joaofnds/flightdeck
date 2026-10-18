//! Compact App facts retained independently of the bounded raw process cache.
use std::collections::VecDeque;
use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::apps::{self, AppKey};
use crate::cancellation::Operation;
use crate::live_app_response::{KeyKind, Observation, Sample};
use crate::session::OBSERVATIONS_MAX;

mod compact;
mod merge;
pub(crate) use merge::Merge;
mod trace;
use compact::{CompactMeasure, HistoryKey, count};
pub(crate) use trace::TracePoint;

const BYTES_MAX: usize = 64 * 1024 * 1024;
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
    key: Arc<HistoryKey>,
    members: u16,
    membership_complete: bool,
    cpu_ns: CompactMeasure,
    energy_nj: CompactMeasure,
    disk_read_bytes: CompactMeasure,
    disk_written_bytes: CompactMeasure,
    disk_io_bytes: CompactMeasure,
    resident_bytes: CompactMeasure,
    footprint_bytes: CompactMeasure,
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
        // Incoming references keep reused key allocations alive while older points are evicted.
        let bytes = point.bytes() + point.unshared_key_bytes();
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
            self.bytes -= removed.bytes() + removed.unshared_key_bytes();
            if !expired {
                self.retention_limited = true;
            }
        }

        unreachable!("a bounded cache can evict all prior points");
    }

    pub(super) fn capture(&self, sample: &Sample, segment: u64, output: &mut Option<Box<Point>>) {
        Point::capture(sample, segment, self.points.back().map(Box::as_ref), output);
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

        let response = crate::live_app_response::bounded(
            json!({"selected": selected.json(), "context_seconds": 900, "points": points,
            "coverage": {"retention_limited": self.retention_limited,
                "observations_retained": self.points.len(), "cache_bytes": self.bytes,
                "cache_bytes_max": BYTES_MAX}}),
        )?;
        operation.check()?;

        Ok(response)
    }
}

impl Point {
    fn capture(
        sample: &Sample,
        segment: u64,
        previous: Option<&Self>,
        output: &mut Option<Box<Self>>,
    ) {
        *output = None;
        let previous = previous.map_or(&[][..], |point| &point.apps);
        let mut previous_index = 0;
        let mut point = Box::new(Self {
            observation: sample.selected,
            segment,
            elapsed_ns: sample.elapsed_ns,
            apps: Vec::with_capacity(sample.apps().len()),
        });

        for app in sample.apps() {
            point.apps.push(App {
                key: compact::capture_key(&app.key, previous, &mut previous_index),
                members: count(app.members),
                membership_complete: app.membership_complete,
                cpu_ns: CompactMeasure::capture(&app.cpu_ns),
                energy_nj: CompactMeasure::capture(&app.energy_nj),
                disk_read_bytes: CompactMeasure::capture(&app.disk_read_bytes),
                disk_written_bytes: CompactMeasure::capture(&app.disk_written_bytes),
                disk_io_bytes: CompactMeasure::capture(&app.disk_io_bytes),
                resident_bytes: CompactMeasure::capture(&app.resident_bytes),
                footprint_bytes: CompactMeasure::capture(&app.footprint_bytes),
            });
        }

        assert!(
            point.bytes() + point.unshared_key_bytes()
                <= crate::live_app_response::SAMPLE_BYTES_MAX
        );

        *output = Some(point);
    }

    fn bytes(&self) -> usize {
        size_of::<Self>() + self.apps.capacity() * size_of::<App>()
    }

    fn unshared_key_bytes(&self) -> usize {
        self.apps
            .iter()
            .filter(|app| Arc::strong_count(&app.key) == 1)
            .map(|app| app.key.bytes())
            .sum()
    }

    fn trace(&self, target: &Target<'_>) -> TracePoint {
        let target = key(target.kind, target.bytes);
        let app = match self
            .apps
            .binary_search_by(|app| app.key.view().cmp(&target))
        {
            Ok(index) => Some(&self.apps[index]),
            Err(_) => None,
        };

        TracePoint::from_live(&self.observation, self.elapsed_ns, app)
    }

    fn json(&self, target: &Target<'_>) -> Value {
        self.trace(target).json(self.segment)
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

#[cfg(test)]
mod merge_tests;
