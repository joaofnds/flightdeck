use std::cell::{Cell, RefCell};

use persistence::{Error, Result};
use serde_json::{Value, json};

use super::{BYTES_MAX, Cache, History, Operation, ResidentCache};
use crate::Session;
use crate::app_attribution_history::cache::Point as SavedPoint;
use crate::app_attribution_history::{AppAttributionHistories, AppAttributionMetric, Reduction};
use crate::live::resident_history::Source;
use crate::live_app_response::Selected;

struct Batch<'operation> {
    states: [MetricState<'operation>; 3],
}

enum MetricState<'operation> {
    Ready(Box<Reduction<'operation>>),
    Failed(Error),
}

struct SavedQuery<'query> {
    selected: &'query Selected,
    saved: &'query Session,
    operation: &'query Operation,
    available: usize,
}

struct SavedCoverage {
    committed: usize,
    cached: usize,
    uncached: usize,
    bytes: usize,
}

struct CacheLease<'history> {
    history: &'history History,
    resident: ResidentCache,
    rates: Cache,
}

const METRICS: [AppAttributionMetric; 3] = [
    AppAttributionMetric::Cpu,
    AppAttributionMetric::Power,
    AppAttributionMetric::Disk,
];
const OBSERVATIONS_MAX: usize = 4096;
const _: () = {
    assert!(OBSERVATIONS_MAX == crate::session::OBSERVATIONS_MAX);
    assert!(METRICS.len() == 3);
};

impl History {
    pub(crate) fn attribution_batch_response(
        &self,
        selected: &Selected,
        saved: &Session,
        operation: &Operation,
    ) -> Result<Box<AppAttributionHistories>> {
        let mut batch = Batch::new(selected, operation);
        self.attribution_batch_saved(
            &SavedQuery {
                selected,
                saved,
                operation,
                available: BYTES_MAX,
            },
            &mut batch,
        )
    }

    fn attribution_batch_saved(
        &self,
        query: &SavedQuery<'_>,
        batch: &mut Batch<'_>,
    ) -> Result<Box<AppAttributionHistories>> {
        let mut lease = CacheLease::new(self);
        let space = lease.prepare(query.available, query.operation)?;
        Batch::retain(&mut lease.rates, query, space)?;
        query.saved.validate_attribution_cache(&lease.rates)?;
        query
            .saved
            .with_app_attribution_replay(query.selected, |replay| {
                let uncached =
                    Self::attribution_batch_prepare(query, &mut lease.rates, replay, space)?;
                self.attribution_batch_replay(query, batch, &mut lease, replay, uncached)
            })
    }

    fn attribution_batch_prepare(
        query: &SavedQuery<'_>,
        rates: &mut Cache,
        replay: &mut crate::session::AttributionReplay,
        space: usize,
    ) -> Result<usize> {
        let rates = RefCell::new(rates);
        let missing = Cell::new(0_usize);
        let before = rates.borrow().count();
        query.saved.attribution_replay_plan(replay, |observation| {
            if rates.borrow().contains(observation, query.operation)? {
                return Ok(true);
            }

            missing.set(missing.get() + 1);
            Ok(false)
        })?;
        if !rates.borrow().limited() {
            query.saved.visit_attribution_replay(
                replay,
                |observed, windows| {
                    rates
                        .borrow_mut()
                        .capture(observed, windows, space, query.operation)
                },
                |pin| rates.borrow_mut().pin(pin, space),
            )?;
        }
        let inserted = rates
            .borrow()
            .count()
            .checked_sub(before)
            .ok_or(Error::Bound("batch attribution inserted facts"))?;
        let uncached = missing
            .get()
            .checked_sub(inserted)
            .ok_or(Error::Bound("batch attribution uncached facts"))?;
        if uncached > 0 {
            rates
                .borrow_mut()
                .retain(query.operation, |_observation| Ok(true))?;
        }
        query.saved.attribution_replay_plan(replay, |observation| {
            rates.borrow().contains(observation, query.operation)
        })?;
        Ok(uncached)
    }

    fn attribution_batch_replay(
        &self,
        query: &SavedQuery<'_>,
        batch: &mut Batch<'_>,
        lease: &mut CacheLease<'_>,
        replay: &mut crate::session::AttributionReplay,
        uncached: usize,
    ) -> Result<Box<AppAttributionHistories>> {
        batch.observe_summed(&lease.rates, query.operation)?;
        if uncached > 0 {
            Self::attribution_batch_stream(query, replay, |point| {
                batch.observe_saved(point);
                #[cfg(test)]
                probe_observed();
            })?;
        }
        batch.choose(query.operation)?;
        #[cfg(test)]
        probe_before_project();
        query.operation.check()?;
        for point in lease.rates.points() {
            query.operation.check()?;
            batch.project_saved(point);
        }
        if uncached > 0 {
            Self::attribution_batch_stream(query, replay, |point| {
                batch.project_saved(point);
                #[cfg(test)]
                probe_projected();
            })?;
        }
        query.operation.check()?;
        let coverage = SavedCoverage {
            committed: replay.count(),
            cached: lease.rates.count(),
            uncached,
            bytes: lease.bytes(),
        };
        let response = batch.finish(self, Some(&coverage), query.operation)?;
        query.saved.validate_attribution_cache(&lease.rates)?;
        query.operation.check()?;

        Ok(response)
    }

    fn attribution_batch_stream(
        query: &SavedQuery<'_>,
        replay: &mut crate::session::AttributionReplay,
        mut consume: impl FnMut(&SavedPoint),
    ) -> Result<()> {
        query.saved.visit_attribution_replay(
            replay,
            |observed, windows| {
                let point =
                    SavedPoint::capture(observed, None, windows, BYTES_MAX, query.operation)?;
                consume(&point);
                Ok(())
            },
            |pin| {
                drop(pin);
                Ok(())
            },
        )
    }
}

impl<'operation> Batch<'operation> {
    fn new(selected: &Selected, operation: &'operation Operation) -> Self {
        Self {
            states: [
                MetricState::Ready(Box::new(Reduction::new(selected, METRICS[0], operation))),
                MetricState::Ready(Box::new(Reduction::new(selected, METRICS[1], operation))),
                MetricState::Ready(Box::new(Reduction::new(selected, METRICS[2], operation))),
            ],
        }
    }

    fn retain(rates: &mut Cache, query: &SavedQuery<'_>, available: usize) -> Result<()> {
        rates.retain(query.operation, |observation| {
            query.operation.check()?;
            Ok(observation
                .wall_ns
                .is_some_and(|wall| query.selected.holds(wall / 1_000_000_000)))
        })?;
        rates.resume(available);

        Ok(())
    }

    fn observe_summed(&mut self, rates: &Cache, operation: &Operation) -> Result<()> {
        for point in rates.points() {
            operation.check()?;
            for state in &mut self.states {
                state.apply(|reduction| reduction.observe_summed(&point.capture, Source::Saved));
            }
        }
        for (metric, state) in METRICS.into_iter().zip(&mut self.states) {
            state.apply(|reduction| reduction.merge(rates.totals(metric)));
        }

        Ok(())
    }

    fn observe_saved(&mut self, point: &SavedPoint) {
        for (metric, state) in METRICS.into_iter().zip(&mut self.states) {
            state.apply(|reduction| {
                reduction.observe(&point.capture, point.apps(metric), Source::Saved)
            });
        }
    }

    fn choose(&mut self, operation: &Operation) -> Result<()> {
        operation.check()?;
        for state in &mut self.states {
            state.apply(Reduction::choose);
        }
        operation.check()
    }

    fn project_saved(&mut self, point: &SavedPoint) {
        for (metric, state) in METRICS.into_iter().zip(&mut self.states) {
            state.apply(|reduction| {
                reduction.project(&point.capture, point.apps(metric), Source::Saved)
            });
        }
    }

    fn finish(
        &mut self,
        history: &History,
        coverage: Option<&SavedCoverage>,
        operation: &Operation,
    ) -> Result<Box<AppAttributionHistories>> {
        let [cpu, power, disk] = std::mem::replace(
            &mut self.states,
            METRICS.map(|_metric| MetricState::Failed(Error::Bound("finished attribution batch"))),
        );

        let response = Box::new(AppAttributionHistories {
            cpu: cpu.finish(history, coverage),
            power: power.finish(history, coverage),
            disk: disk.finish(history, coverage),
        });
        operation.check()?;

        Ok(response)
    }
}

impl<'operation> MetricState<'operation> {
    fn apply(&mut self, action: impl FnOnce(&mut Reduction<'operation>) -> Result<()>) {
        if let Self::Ready(reduction) = self {
            if let Err(error) = action(reduction) {
                *self = Self::Failed(error);
            }
        }
    }

    fn finish(self, _history: &History, coverage: Option<&SavedCoverage>) -> Result<Value> {
        let reduction = match self {
            Self::Ready(reduction) => reduction,
            Self::Failed(error) => return Err(error),
        };
        let (committed, bytes) = coverage.map_or((0, 0), |value| (value.committed, value.bytes));
        let mut response = super::attribution_finish(*reduction, committed, bytes)?;
        if let Some(coverage) = coverage {
            response["coverage"]["saved_cache_limited"] = json!(coverage.uncached > 0);
            response["coverage"]["saved_observations_cached"] = json!(coverage.cached);
            response["coverage"]["saved_observations_uncached"] = json!(coverage.uncached);
        }

        crate::live_app_response::bounded(response)
    }
}

impl<'history> CacheLease<'history> {
    fn new(history: &'history History) -> Box<Self> {
        let mut resident = history.saved_residents.replace(ResidentCache::new());
        let rates = resident.take_attribution();

        Box::new(Self {
            history,
            resident,
            rates,
        })
    }

    fn prepare(&mut self, available: usize, operation: &Operation) -> Result<usize> {
        assert!(available <= BYTES_MAX);
        if self.resident.bytes() > available {
            self.resident = ResidentCache::new();
        }
        let space = available - self.resident.bytes();
        self.rates.trim(space, operation)?;

        Ok(space)
    }

    fn bytes(&self) -> usize {
        if self.rates.points().next().is_some() {
            return self.resident.bytes() + self.rates.bytes();
        }

        self.resident.bytes()
    }
}

impl Drop for CacheLease<'_> {
    fn drop(&mut self) {
        if self.rates.points().next().is_some() {
            let rates = std::mem::replace(&mut self.rates, Cache::new());
            self.resident.install_attribution(rates);
        }
        assert!(self.resident.bytes() <= BYTES_MAX);
        let resident = std::mem::replace(&mut self.resident, ResidentCache::new());
        self.history.saved_residents.replace(resident);
    }
}

#[cfg(test)]
#[derive(Default)]
struct Probe {
    observed: u32,
    projected: u32,
    before_project: Option<Box<dyn FnOnce()>>,
}

#[cfg(test)]
thread_local! {
    static PROBE: RefCell<Probe> = const {
        RefCell::new(Probe { observed: 0, projected: 0, before_project: None })
    };
}

#[cfg(test)]
fn probe_observed() {
    PROBE.with(|probe| probe.borrow_mut().observed += 1);
}

#[cfg(test)]
fn probe_projected() {
    PROBE.with(|probe| probe.borrow_mut().projected += 1);
}

#[cfg(test)]
fn probe_before_project() {
    let checkpoint = PROBE.with(|probe| probe.borrow_mut().before_project.take());
    if let Some(checkpoint) = checkpoint {
        checkpoint();
    }
}

#[cfg(test)]
#[path = "attribution_batch_tests.rs"]
mod tests;
