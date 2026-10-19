use persistence::{Error, Result};

use crate::Session;
use crate::app_process_groups_history::AppProcessGroupsHistory;
use crate::app_process_groups_history::cache::Cache;
use crate::app_process_groups_history::reduce::{Phase, Reduction};
use crate::cancellation::Operation;
use crate::live_app_history::{BYTES_MAX, History, ResidentCache, Target};
use crate::live_app_response::Observation;

struct Query<'query, 'key> {
    selected: &'query Observation,
    target: &'query Target<'key>,
    saved: &'query Session,
    context: &'query [crate::cpu_history_sample::Identity],
    operation: &'query Operation,
    available: usize,
}

struct Lease<'history> {
    history: &'history History,
    resident: ResidentCache,
    groups: Option<Box<Cache>>,
    committed: bool,
    available: usize,
}

impl History {
    pub(crate) fn process_groups_response(
        &self,
        selected: &Observation,
        target: &Target<'_>,
        saved: Option<&Session>,
        context: &[crate::cpu_history_sample::Identity],
        operation: &Operation,
    ) -> Result<Box<AppProcessGroupsHistory>> {
        let mut reduction = Reduction::new(selected, target.kind, target.bytes, operation)?;
        self.process_groups_visit(target, &mut reduction, Phase::Observe)?;
        if let Some(saved) = saved {
            return self.process_groups_response_saved(
                &Query {
                    selected,
                    target,
                    saved,
                    context,
                    operation,
                    available: BYTES_MAX - self.bytes,
                },
                reduction,
            );
        }

        self.saved_residents.borrow_mut().take_groups();

        reduction.context(context)?;
        reduction.choose()?;
        self.process_groups_visit(target, &mut reduction, Phase::Project)?;

        reduction.finish()
    }

    fn process_groups_response_saved(
        &self,
        query: &Query<'_, '_>,
        mut reduction: Box<Reduction<'_>>,
    ) -> Result<Box<AppProcessGroupsHistory>> {
        let mut lease = Box::new(Lease {
            history: self,
            resident: self.saved_residents.replace(ResidentCache::new()),
            groups: None,
            committed: false,
            available: 0,
        });

        lease.prepare(query.target, query.available);
        let result = query
            .saved
            .with_process_groups_replay(query.selected, |replay| {
                let cached = self.process_groups_response_saved_restore(
                    query,
                    replay,
                    &mut lease,
                    &mut reduction,
                )?;
                reduction.context(query.context)?;
                reduction.choose()?;
                #[cfg(test)]
                BEFORE_PROJECT.with(|hook| {
                    if let Some(hook) = hook.borrow_mut().take() {
                        hook();
                    }
                });

                query.operation.check()?;
                self.process_groups_visit(query.target, &mut reduction, Phase::Project)?;
                if cached {
                    let cache = lease
                        .groups
                        .as_ref()
                        .ok_or(Error::Bound("process group cache owner"))?;
                    for point in cache.points() {
                        point.visit(&mut reduction, Phase::Project)?;
                    }
                } else {
                    // Overflow may have removed facts used during Observe. Restore the entire plan.
                    query
                        .saved
                        .process_groups_plan(replay, |observation| self.groups_live(observation))?;
                    query
                        .saved
                        .visit_process_groups(replay, |frame, previous, _predecessor| {
                            super::cohort::capture(frame, previous, query.target, query.operation)?
                                .visit(&mut reduction, Phase::Project)
                        })?;
                }

                if let Some(cache) = &lease.groups {
                    query.saved.process_groups_cache_validate(cache)?;
                }

                query.operation.check()?;
                reduction.finish()
            })?;

        query.operation.check()?;
        lease.committed = true;

        Ok(result)
    }

    fn process_groups_response_saved_restore(
        &self,
        query: &Query<'_, '_>,
        replay: &mut crate::session::ProcessGroupsReplay,
        lease: &mut Lease<'_>,
        reduction: &mut Reduction<'_>,
    ) -> Result<bool> {
        let space = lease.space();
        let second = query
            .selected
            .wall_ns
            .ok_or(Error::Bound("process group cache clock"))?
            / 1_000_000_000; // Floor to the containing second.
        let mut cached =
            self.process_groups_response_saved_restore_prepare(query, replay, lease, second)?;

        query.saved.process_groups_plan(replay, |observation| {
            if self.groups_live(observation)? {
                return Ok(true);
            }

            if cached {
                return lease
                    .groups
                    .as_ref()
                    .ok_or(Error::Bound("process group cache owner"))?
                    .contains(observation);
            }

            Ok(false)
        })?;

        if cached {
            Self::process_groups_response_saved_restore_observe(lease, reduction)?;
        }

        query
            .saved
            .visit_process_groups(replay, |frame, previous, predecessor| {
                let snapshot =
                    super::cohort::capture(frame, previous, query.target, query.operation)?;
                snapshot.visit(reduction, Phase::Observe)?;
                if cached {
                    let cache = lease
                        .groups
                        .as_mut()
                        .ok_or(Error::Bound("process group cache owner"))?;
                    if !cache.insert(&snapshot, predecessor, space, query.operation)? {
                        cache.limit(space, second);
                        cached = false;
                    }
                }

                Ok(())
            })?;

        if cached {
            let cache = lease
                .groups
                .as_mut()
                .ok_or(Error::Bound("process group cache owner"))?;
            let pinned = Session::process_groups_cache_pins(query.saved, replay, cache, space)?;
            if !pinned {
                cache.limit(space, second);
                cached = false;
            }
        }

        Ok(cached)
    }

    fn process_groups_response_saved_restore_observe(
        lease: &Lease<'_>,
        reduction: &mut Reduction<'_>,
    ) -> Result<()> {
        let cache = lease
            .groups
            .as_ref()
            .ok_or(Error::Bound("process group cache owner"))?;

        for point in cache.points() {
            point.visit(reduction, Phase::Observe)?;
        }

        Ok(())
    }

    fn process_groups_response_saved_restore_prepare(
        &self,
        query: &Query<'_, '_>,
        replay: &crate::session::ProcessGroupsReplay,
        lease: &mut Lease<'_>,
        second: u64,
    ) -> Result<bool> {
        let space = lease.space();
        let Some(cache) = &mut lease.groups else {
            return Ok(false);
        };

        cache.retain(query.operation, |point| {
            if self.groups_live(&point.observation)? {
                return Ok(false);
            }

            query.saved.process_groups_cache_needed(replay, point)
        })?;

        query.saved.process_groups_cache_validate(cache)?;
        let cached = cache.prepare(space, second);
        if !cached {
            cache.limit(space, second);
        }

        Ok(cached)
    }

    fn groups_live(&self, observation: &Observation) -> Result<bool> {
        for point in &self.points {
            if point.observation.recording != observation.recording {
                continue;
            }

            if point.observation.stamp.boot != observation.stamp.boot {
                continue;
            }

            if point.observation.stamp.run != observation.stamp.run {
                continue;
            }

            if point.observation.stamp.publication != observation.stamp.publication {
                continue;
            }

            if point.observation != *observation {
                return Err(Error::Bound("process group live observation conflict"));
            }

            return Ok(point.groups.is_some());
        }

        Ok(false)
    }
}

impl Lease<'_> {
    fn prepare(&mut self, target: &Target<'_>, available: usize) {
        let previous = self.resident.take_groups();
        // The open page's groups may use the whole budget, since leaving them no room
        // replays every saved minute twice on each reading. Other pages' caches give way
        // when this answer commits.
        self.available = available.min(BYTES_MAX - self.history.bytes);
        self.groups = previous.filter(|cache| cache.matches(target));
        if self.groups.is_none() {
            self.groups = Some(Cache::new(target));
        }
        if self
            .groups
            .as_ref()
            .is_some_and(|cache| cache.bytes() > self.available)
        {
            self.groups = None;
        }
    }

    fn space(&self) -> usize {
        self.available
    }
}

#[cfg(test)]
#[path = "../../live_app_process_groups_history_tests/cache.rs"]
mod tests;

#[cfg(test)]
thread_local! {
    static BEFORE_PROJECT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if self.committed {
            self.resident.install_groups(self.groups.take());
            if self.resident.bytes() > self.available {
                let groups = self.resident.take_groups();
                self.resident = ResidentCache::new();
                self.resident.install_groups(groups);
            }
        }

        assert!(self.history.bytes + self.resident.bytes() <= BYTES_MAX);

        let resident = std::mem::replace(&mut self.resident, ResidentCache::new());
        self.history.saved_residents.replace(resident);
    }
}
