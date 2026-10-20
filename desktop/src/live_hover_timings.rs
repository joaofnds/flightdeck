use std::collections::VecDeque;
use std::time::Duration;

use serde_json::{Value, json};

#[cfg(test)]
#[path = "live_hover_timings_tests.rs"]
pub(crate) mod tests;

/// The hovers the report keeps, more than one scrub across a chart moves through.
pub const HOVERS_MAX: usize = 64;

/// The latest hovers of the page's charts and how long each took, which the app measures
/// itself, since a reader outside it waits for the app's tick to see a hover at all.
pub struct HoverTimings {
    /// Oldest first.
    latest: VecDeque<HoverTiming>,
    /// Every hover recorded since the app started, so a reader can tell how many it missed.
    count: u64,
}

/// One hover of a chart by the pointer.
pub struct HoverTiming {
    pub route: String,
    pub second: u64,
    /// From the app handling the pointer move to the page holding the hovered second's complete
    /// readings, or none where the hover ended, or its read failed, first.
    pub complete: Option<Duration>,
}

impl HoverTimings {
    pub fn new() -> Self {
        Self {
            latest: VecDeque::with_capacity(HOVERS_MAX),
            count: 0,
        }
    }

    pub fn push(&mut self, timing: HoverTiming) {
        if self.latest.len() == HOVERS_MAX {
            assert!(self.latest.pop_front().is_some());
        }
        self.latest.push_back(timing);
        assert!(self.latest.len() <= HOVERS_MAX);
        self.count += 1;
    }

    /// The latest hovers, oldest first, with each time in milliseconds.
    pub fn report(&self) -> Value {
        let latest: Vec<Value> = self
            .latest
            .iter()
            .map(|timing| {
                let complete_ms = timing
                    .complete
                    .map(|complete| complete.as_secs_f64() * 1000.0);
                json!({"route": timing.route, "second": timing.second, "complete_ms": complete_ms})
            })
            .collect();
        json!({"count": self.count, "latest": latest})
    }
}
