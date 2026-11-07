#[path = "attribution_batch.rs"]
mod batch;

use std::cell::{Cell, RefCell};

use persistence::Result;
use serde_json::{Value, json};

use super::{BYTES_MAX, History, ResidentCache};
use crate::AppAttributionMetric;
use crate::Session;
use crate::app_attribution_history::Reduction;
use crate::app_attribution_history::cache::Observed;
use crate::app_attribution_history::cache::{Cache, Point as SavedPoint};
use crate::cancellation::Operation;
use crate::live::resident_history::Source;
use crate::live_app_response::Selected;

struct SavedQuery<'query> {
    selected: &'query Selected,
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
        selected: &Selected,
        metric: AppAttributionMetric,
        saved: &Session,
        operation: &Operation,
    ) -> Result<Value> {
        self.attribution_saved(
            &SavedQuery {
                selected,
                metric,
                saved,
                operation,
            },
            Reduction::new(selected, metric, operation),
            BYTES_MAX,
        )
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
        let restored = Self::attribution_prepare(saved, selected, &mut rates, space, operation)?;
        for point in rates.points() {
            reduction.observe_summed(&point.capture, Source::Saved)?;
        }
        reduction.merge(rates.totals(metric))?;
        if restored.uncached > 0 {
            let reduction = RefCell::new(&mut reduction);
            attribution_stream(query, &rates, &reduction, |point, reduction| {
                reduction.observe(&point.capture, point.apps(metric), Source::Saved)
            })?;
        }
        reduction.choose()?;
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
        let mut response = attribution_finish(reduction, restored.committed, cache.bytes())?;
        response["coverage"]["saved_cache_limited"] = json!(restored.uncached > 0);
        response["coverage"]["saved_observations_cached"] = json!(cached);
        response["coverage"]["saved_observations_uncached"] = json!(restored.uncached);
        operation.check()?;
        assert!(cache.bytes() <= BYTES_MAX);
        self.saved_residents.replace(cache);
        crate::live_app_response::bounded(response)
    }

    fn attribution_prepare(
        saved: &Session,
        selected: &Selected,
        rates: &mut Cache,
        available: usize,
        operation: &Operation,
    ) -> Result<Restored> {
        rates.retain(operation, |observation| {
            operation.check()?;
            Ok(observation
                .wall_ns
                .is_some_and(|wall| selected.holds(wall / 1_000_000_000)))
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
                if rates.borrow().contains(observation, operation)? {
                    return Ok(true);
                }
                uncached.set(uncached.get() + 1);
                Ok(rates.borrow().limited())
            },
            |frame, previous, windows| {
                rates.borrow_mut().capture(
                    Observed::Replayed { frame, previous },
                    windows,
                    available,
                    operation,
                )
            },
            |pin| rates.borrow_mut().pin(pin, available),
        )?;
        saved.validate_attribution_cache(&rates.borrow())?;
        let inserted = rates.borrow().count() - before;
        if uncached.get() > inserted {
            rates
                .borrow_mut()
                .retain(operation, |_observation| Ok(true))?;
            saved.validate_attribution_cache(&rates.borrow())?;
        }
        Ok(Restored {
            committed,
            uncached: uncached.get() - inserted,
        })
    }
}

fn attribution_finish(
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
    Ok(response)
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
        |observation| rates.contains(observation, operation),
        |frame, previous, windows| {
            let point = SavedPoint::capture(
                Observed::Replayed { frame, previous },
                None,
                windows,
                BYTES_MAX,
                operation,
            )?;
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
