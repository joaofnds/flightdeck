use std::cell::{Cell, RefCell};

use persistence::{Error, Result};
use serde_json::{Value, json};

use super::{BYTES_MAX, History, Observation, Point, ResidentCache};
use crate::AppAttributionMetric;
use crate::Session;
use crate::app_attribution_history::cache::{Cache, Point as SavedPoint};
use crate::app_attribution_history::{Capture, Reading, Reduction};
use crate::cancellation::Operation;
use crate::live::resident_history::Source;

struct SavedQuery<'query> {
    selected: &'query Observation,
    metric: AppAttributionMetric,
    saved: &'query Session,
    operation: &'query Operation,
}

struct Restored {
    committed: usize,
    uncached: usize,
}

impl History {
    pub(crate) fn attribution_response(
        &self,
        selected: &Observation,
        metric: AppAttributionMetric,
        saved: Option<&Session>,
        operation: &Operation,
    ) -> Result<Value> {
        let mut reduction = Reduction::new(selected, metric, operation)?;
        self.attribution_observe(&mut reduction, metric)?;
        if let Some(saved) = saved {
            return self.attribution_saved(
                &SavedQuery {
                    selected,
                    metric,
                    saved,
                    operation,
                },
                reduction,
                BYTES_MAX.saturating_sub(self.bytes),
            );
        }
        drop(self.saved_residents.replace(ResidentCache::new()));
        reduction.choose()?;
        self.attribution_project(&mut reduction, metric)?;
        self.attribution_finish(reduction, 0, 0)
    }

    fn attribution_saved(
        &self,
        query: &SavedQuery<'_>,
        mut reduction: Reduction<'_>,
        available: usize,
    ) -> Result<Value> {
        let SavedQuery {
            selected,
            metric,
            saved,
            operation,
        } = *query;
        let mut cache = self.saved_residents.replace(ResidentCache::new());
        let mut rates = cache.take_attribution();
        if cache.bytes() > available {
            cache = ResidentCache::new();
        }
        let space = available - cache.bytes();
        rates.trim(space, operation)?;
        let restored =
            Self::attribution_prepare(saved, selected, &reduction, &mut rates, space, operation)?;
        for point in rates.points() {
            reduction.observe(&point.capture, point.apps(metric), Source::Saved)?;
        }
        if restored.uncached > 0 {
            let reduction = RefCell::new(&mut reduction);
            attribution_stream(query, &rates, &reduction, |point, reduction| {
                reduction.observe(&point.capture, point.apps(metric), Source::Saved)
            })?;
        }
        reduction.choose()?;
        self.attribution_project(&mut reduction, metric)?;
        for point in rates.points() {
            reduction.project(&point.capture, point.apps(metric), Source::Saved)?;
        }
        saved.validate_attribution_cache(&rates)?;
        if restored.uncached > 0 {
            let reduction = RefCell::new(&mut reduction);
            attribution_stream(query, &rates, &reduction, |point, reduction| {
                reduction.project(&point.capture, point.apps(metric), Source::Saved)
            })?;
        }
        let cached = rates.count();
        if rates.points().next().is_some() {
            cache.install_attribution(rates);
        }
        let mut response = self.attribution_finish(reduction, restored.committed, cache.bytes())?;
        response["coverage"]["saved_cache_limited"] = json!(restored.uncached > 0);
        response["coverage"]["saved_observations_cached"] = json!(cached);
        response["coverage"]["saved_observations_uncached"] = json!(restored.uncached);
        operation.check()?;
        assert!(self.bytes + cache.bytes() <= BYTES_MAX);
        self.saved_residents.replace(cache);
        crate::live_app_response::bounded(response)
    }

    fn attribution_prepare(
        saved: &Session,
        selected: &Observation,
        reduction: &Reduction<'_>,
        rates: &mut Cache,
        available: usize,
        operation: &Operation,
    ) -> Result<Restored> {
        let second = selected
            .wall_ns
            .ok_or(Error::Bound("attribution selected clock"))?
            / 1_000_000_000;
        rates.retain(operation, |observation| {
            operation.check()?;
            if reduction.contains_live(observation)? {
                return Ok(false);
            }
            Ok(observation.wall_ns.is_some_and(|wall| {
                (second.saturating_sub(899)..=second).contains(&(wall / 1_000_000_000))
            }))
        })?;
        rates.resume(available);
        saved.validate_attribution_cache(rates)?;
        let before = rates.count();
        let uncached = Cell::new(0_usize);
        let rates = RefCell::new(rates);
        let committed = saved.visit_app_attribution(
            selected,
            |observation| {
                operation.check()?;
                if reduction.contains_live(observation)? {
                    return Ok(true);
                }
                if rates.borrow().contains(observation, operation)? {
                    return Ok(true);
                }
                uncached.set(uncached.get() + 1);
                Ok(rates.borrow().limited())
            },
            |frame, previous, windows| {
                rates
                    .borrow_mut()
                    .capture(frame, previous, windows, available, operation)
            },
            |pin| rates.borrow_mut().pin(pin, available),
        )?;
        saved.validate_attribution_cache(&rates.borrow())?;
        let inserted = rates.borrow().count() - before;
        Ok(Restored {
            committed,
            uncached: uncached.get() - inserted,
        })
    }

    fn attribution_project(
        &self,
        reduction: &mut Reduction<'_>,
        metric: AppAttributionMetric,
    ) -> Result<()> {
        for point in &self.points {
            reduction.project(
                &point.attribution_capture(),
                point.attribution_apps(metric),
                Source::Live,
            )?;
        }
        Ok(())
    }

    fn attribution_observe(
        &self,
        reduction: &mut Reduction<'_>,
        metric: AppAttributionMetric,
    ) -> Result<()> {
        for point in &self.points {
            reduction.observe(
                &point.attribution_capture(),
                point.attribution_apps(metric),
                Source::Live,
            )?;
        }
        Ok(())
    }

    fn attribution_finish(
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
    fn attribution_capture(&self) -> Capture {
        let mut coverage = self.resident_capture();
        coverage.ungrouped_members = self.ungrouped_members;
        Capture {
            coverage,
            elapsed_ns: self.elapsed_ns,
        }
    }

    fn attribution_apps(&self, metric: AppAttributionMetric) -> impl Iterator<Item = Reading<'_>> {
        self.apps.iter().map(move |app| Reading {
            key: app.key.view(),
            measure: match metric {
                AppAttributionMetric::Cpu => app.cpu_ns.measure(),
                AppAttributionMetric::Power => app.energy_nj.measure(),
            },
            members: u32::from(app.members),
            membership_complete: app.membership_complete,
        })
    }
}

fn attribution_stream(
    query: &SavedQuery<'_>,
    rates: &Cache,
    reduction: &RefCell<&mut Reduction<'_>>,
    mut consume: impl FnMut(&SavedPoint, &mut Reduction<'_>) -> Result<()>,
) -> Result<usize> {
    let SavedQuery {
        selected,
        saved,
        operation,
        ..
    } = *query;
    saved.visit_app_attribution(
        selected,
        |observation| {
            if reduction.borrow().contains_live(observation)? {
                return Ok(true);
            }
            rates.contains(observation, operation)
        },
        |frame, previous, windows| {
            let point = SavedPoint::capture(frame, previous, None, windows, BYTES_MAX, operation)?;
            consume(&point, &mut reduction.borrow_mut())
        },
        |pin| {
            drop(pin);
            Ok(())
        },
    )
}

#[cfg(test)]
#[path = "attribution_tests.rs"]
mod tests;
