//! The exact observation order shared by retained queries and the Store's display View.
use std::cmp::Ordering;

use crate::live_app_response::Observation;

pub(super) fn compare(before: &Observation, after: &Observation) -> Ordering {
    (
        before.wall_ns,
        before.stamp.run,
        before.stamp.boot,
        before.stamp.publication,
    )
        .cmp(&(
            after.wall_ns,
            after.stamp.run,
            after.stamp.boot,
            after.stamp.publication,
        ))
}

pub(super) fn at_second(observation: &Observation, second: u64) -> bool {
    observation.wall_ns.map(|wall| wall / 1_000_000_000) == Some(second)
}
