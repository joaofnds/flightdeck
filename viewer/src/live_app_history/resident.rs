use persistence::Result;
use serde_json::{Value, json};

use super::{BYTES_MAX, History, ResidentCache};
use crate::Session;
use crate::cancellation::Operation;
use crate::live::resident_history::{Reduction, Source};
use crate::live_app_response::Selected;

impl History {
    pub(crate) fn resident_response(
        &self,
        selected: &Selected,
        saved: &Session,
        operation: &Operation,
    ) -> Result<Value> {
        self.resident_response_within(selected, saved, operation, BYTES_MAX)
    }

    /// The resident response whose points from the recording take at most `available` bytes
    /// of the cache.
    pub(crate) fn resident_response_within(
        &self,
        selected: &Selected,
        saved: &Session,
        operation: &Operation,
        available: usize,
    ) -> Result<Value> {
        assert!(available <= BYTES_MAX);

        let mut reduction = Reduction::new(selected, operation);
        let mut cache = self.saved_residents.replace(ResidentCache::new());
        if cache.bytes() > available {
            cache = ResidentCache::new();
        }
        let response = saved.with_app_resident_replay(selected, |replay| {
            let cached = saved.prepare_resident_cache(replay, &mut cache, available)?;
            if cached {
                for point in cache.points() {
                    reduction.observe(&point.capture, point.apps(), Source::Saved)?;
                }
            } else {
                replay.reset();
                saved.visit_app_resident(replay, |saved| reduction.observe_saved(saved))?;
            }
            reduction.choose()?;
            if cached {
                for point in cache.points() {
                    reduction.project(&point.capture, point.apps(), Source::Saved)?;
                }
            } else {
                saved.visit_app_resident(replay, |saved| reduction.project_saved(saved))?;
            }
            saved.resident_cache_validate(&cache)?;
            resident_response_finish(reduction, replay.count(), cache.bytes())
        })?;
        operation.check()?;
        assert!(cache.bytes() <= BYTES_MAX);
        self.saved_residents.replace(cache);

        Ok(response)
    }
}

fn resident_response_finish(
    reduction: Reduction<'_>,
    committed: usize,
    saved_bytes: usize,
) -> Result<Value> {
    let mut response = reduction.response()?;
    let count = response["points"].as_array().map_or(0, Vec::len);
    let coverage = &mut response["coverage"];
    coverage["observations_retained"] = json!(count);
    coverage["committed_observations"] = json!(committed);
    coverage["cache_bytes"] = json!(saved_bytes);
    coverage["cache_bytes_max"] = json!(BYTES_MAX);

    crate::live_app_response::bounded(response)
}
