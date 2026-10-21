//! The older tail of the scalar context: one reading in every stretch of seconds from before the
//! full-detail context, so a chart can span the whole loaded hour without the viewer holding or
//! sending every second of it.

use persistence::Result;
use serde_json::Value;

use super::{NS, Observation, Session};
use crate::metrics;

/// The seconds of history the viewer loads in all, which is the longest range a chart shows.
pub const LOADED_SECONDS: u64 = 3600;
/// The seconds of full detail the context holds, which the older tail ends before.
pub(crate) const DETAIL_SECONDS: u64 = 900;
/// The seconds after a selected second its context holds, where the recording has them, so a
/// step to a later second shows readings the page already holds. The context still holds
/// `DETAIL_SECONDS` in all, so the rest lie before the selected second.
pub const CONTEXT_AFTER_SECONDS: u64 = 450;
/// The seconds one older reading stands for.
pub const EARLIER_STRIDE_SECONDS: u64 = 10;
/// Older segments are numbered above any the full-detail context counts, so a segment of the
/// tail is never mistaken for one of the recent readings.
const SEGMENT_BASE: u64 = 1 << 32;
const _: () = {
    assert!(DETAIL_SECONDS < LOADED_SECONDS);
    assert!(CONTEXT_AFTER_SECONDS < DETAIL_SECONDS);
    assert!(EARLIER_STRIDE_SECONDS > 0);
    assert!(LOADED_SECONDS <= 65_535);
};

/// The thinned readings before `limit_second`, numbered in segments of their own.
pub(crate) struct Tail {
    pub points: Vec<Value>,
}

impl Session {
    /// The readings from `first_second` up to but not including `limit_second`, one in every
    /// stretch of seconds. A segment that reaches `limit_second` unbroken takes the number of the
    /// full-detail context's first segment, so a chart draws one line through the join.
    pub(crate) fn earlier_tail(&self, first_second: u64, limit_second: u64) -> Result<Tail> {
        if limit_second <= first_second {
            return Ok(Tail { points: Vec::new() });
        }

        self.operation.attach(&self.connection)?;
        self.operation
            .run(|| self.earlier_tail_inner(first_second, limit_second))
    }

    fn earlier_tail_inner(&self, first_second: u64, limit_second: u64) -> Result<Tail> {
        let mut observations = self.observations(first_second * NS, limit_second * NS)?;
        observations.sort_by_key(|value| (value.wall, value.window, value.publication));
        let segments = segment_numbers(&observations);
        let kept = thin(&observations);
        let chosen: Vec<Observation> = kept
            .iter()
            .map(|index| observations[*index].clone())
            .collect();
        let mut points = Vec::new();
        self.context_points(&chosen, &mut points)?;
        for point in &mut points {
            let wall = point["wall_ns"].as_u64();
            let publication = point["publication"].as_u64();
            let index = kept.iter().position(|index| {
                Some(observations[*index].wall) == wall
                    && Some(observations[*index].publication) == publication
            });
            if let Some(index) = index {
                point["segment"] = (SEGMENT_BASE + segments[kept[index]]).into();
            }
        }
        if let Some(last) = observations.last() {
            let after = self.observations(
                limit_second * NS,
                (limit_second + 2 * EARLIER_STRIDE_SECONDS) * NS,
            )?;
            let next = after
                .iter()
                .min_by_key(|value| (value.wall, value.publication));
            if next.is_some_and(|next| metrics::compatible(last, next)) {
                let joined = SEGMENT_BASE + segments[segments.len() - 1];
                for point in &mut points {
                    if point["segment"] == joined {
                        point["segment"] = 1.into();
                    }
                }
            }
        }

        Ok(Tail { points })
    }
}

/// The index of the first observation of each stretch of seconds, from observations in time
/// order.
fn thin(observations: &[Observation]) -> Vec<usize> {
    let mut kept = Vec::new();
    let mut stretch = None;
    for (index, observation) in observations.iter().enumerate() {
        let current = observation.wall / NS / EARLIER_STRIDE_SECONDS;
        if stretch != Some(current) {
            kept.push(index);
            stretch = Some(current);
        }
    }

    kept
}

/// The segment each observation belongs to, counted from one, where a segment ends at any break
/// in the readings.
fn segment_numbers(observations: &[Observation]) -> Vec<u64> {
    let mut numbers = Vec::with_capacity(observations.len());
    let mut segment = 0;
    for (index, observation) in observations.iter().enumerate() {
        let unbroken = index
            .checked_sub(1)
            .is_some_and(|previous| metrics::compatible(&observations[previous], observation));
        if !unbroken {
            segment += 1;
        }
        numbers.push(segment);
    }

    numbers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(second: u64, publication: u64) -> Observation {
        Observation {
            window: 0,
            publication,
            run_tick: publication,
            wall: second * NS + 500,
            continuous: Some(publication * NS),
        }
    }

    #[test]
    fn thinning_keeps_the_first_observation_of_each_stretch() {
        // Goal: an older tail keeps one reading per ten seconds, even where seconds are missing.
        // Method: thin observations that fill one stretch, skip one, and start another.
        let observations: Vec<_> = [100, 101, 109, 110, 119, 140, 141]
            .iter()
            .zip(1..)
            .map(|(second, publication)| observation(*second, publication))
            .collect();

        assert_eq!(thin(&observations), [0, 3, 5]);
        assert_eq!(thin(&[]), Vec::<usize>::new());
    }

    #[test]
    fn segments_break_where_the_readings_do() {
        // Goal: the tail draws no line across a missed stretch of readings.
        // Method: number consecutive observations around a gap of missing publications.
        let observations = [
            observation(100, 1),
            observation(101, 2),
            observation(102, 3),
            observation(200, 40),
            observation(201, 41),
        ];

        assert_eq!(segment_numbers(&observations), [1, 1, 1, 2, 2]);
        assert!(segment_numbers(&[]).is_empty());
    }
}
