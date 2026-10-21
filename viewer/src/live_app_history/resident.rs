use persistence::Result;
use serde_json::{Value, json};

use super::{BYTES_MAX, History, Point, ResidentCache};
use crate::Session;
use crate::cancellation::Operation;
use crate::live::resident_history::{AppReading, Capture, Reduction, Source};
use crate::live_app_response::Selected;

impl History {
    pub(crate) fn resident_response(
        &self,
        selected: &Selected,
        saved: Option<&Session>,
        operation: &Operation,
    ) -> Result<Value> {
        let mut reduction = Reduction::new(selected, operation);
        for point in &self.points {
            operation.check()?;
            reduction.observe(
                &point.resident_capture(),
                point.resident_apps(),
                Source::Live,
            )?;
        }
        if let Some(saved) = saved {
            let mut cache = self.saved_residents.replace(ResidentCache::new());
            let available = BYTES_MAX - self.bytes - size_of::<ResidentCache>();
            if cache.bytes() > available {
                cache = ResidentCache::new();
            }
            let response = saved.with_app_resident_replay(selected, |replay| {
                let cached =
                    saved.prepare_resident_cache(replay, &mut cache, available, |observation| {
                        reduction.contains_live(observation)
                    })?;
                if cached {
                    for point in cache.points() {
                        reduction.observe(&point.capture, point.apps(), Source::Saved)?;
                    }
                } else {
                    replay.reset();
                    replay.exclude(operation, |observation| {
                        reduction.contains_live(observation)
                    })?;
                    saved.visit_app_resident(replay, |frame| reduction.observe_frame(frame))?;
                }
                reduction.choose()?;
                self.resident_project(&mut reduction, operation)?;
                if cached {
                    for point in cache.points() {
                        reduction.project(&point.capture, point.apps(), Source::Saved)?;
                    }
                } else {
                    saved.visit_app_resident(replay, |frame| reduction.project_frame(frame))?;
                }
                saved.resident_cache_validate(&cache)?;
                self.resident_response_finish(reduction, replay.count(), cache.bytes())
            })?;
            operation.check()?;
            assert!(self.bytes + cache.bytes() <= BYTES_MAX);
            self.saved_residents.replace(cache);
            return Ok(response);
        }

        drop(self.saved_residents.replace(ResidentCache::new()));
        reduction.choose()?;
        self.resident_project(&mut reduction, operation)?;
        self.resident_response_finish(reduction, 0, 0)
    }

    fn resident_project(&self, reduction: &mut Reduction<'_>, operation: &Operation) -> Result<()> {
        for point in &self.points {
            operation.check()?;
            reduction.project(
                &point.resident_capture(),
                point.resident_apps(),
                Source::Live,
            )?;
        }

        Ok(())
    }

    fn resident_response_finish(
        &self,
        mut reduction: Reduction<'_>,
        committed: usize,
        saved_bytes: usize,
    ) -> Result<Value> {
        for observation in &self.observations {
            reduction.retain_metadata(observation)?;
        }

        let mut response = reduction.response()?;
        let count = response["points"].as_array().map_or(0, Vec::len);
        let coverage = &mut response["coverage"];
        coverage["live_retention_limited"] = json!(self.retention_limited);
        coverage["observations_retained"] = json!(count);
        coverage["committed_observations"] = json!(committed);
        coverage["cache_bytes"] = json!(self.bytes + saved_bytes);
        coverage["cache_bytes_max"] = json!(BYTES_MAX);

        crate::live_app_response::bounded(response)
    }
}

impl Point {
    pub(super) fn resident_capture(&self) -> Capture {
        Capture {
            observation: self.observation,
            membership_complete: self.membership_complete,
            ungrouped_members: self.resident_ungrouped_members,
            usage_refused: self.usage_refused,
            usage_gone: self.usage_gone,
        }
    }

    fn resident_apps(&self) -> impl Iterator<Item = AppReading<'_>> {
        self.apps
            .iter()
            .map(|app| AppReading {
                key: app.key.view(),
                resident: app.resident_bytes.measure(),
            })
            .filter(|app| crate::live::resident_history::has_resident(&app.resident))
    }
}
