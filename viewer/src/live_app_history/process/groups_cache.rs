use persistence::{Error, Result};

use crate::Session;
use crate::app_process_groups_history::AppProcessGroupsHistory;
use crate::app_process_groups_history::cache::Cache;
use crate::app_process_groups_history::reduce::{Phase, Reduction};
use crate::cancellation::Operation;
use crate::live_app_history::{BYTES_MAX, History, ResidentCache, Target};
use crate::live_app_response::Selected;

struct Query<'query, 'key> {
    selected: &'query Selected,
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
        selected: &Selected,
        target: &Target<'_>,
        saved: &Session,
        context: &[crate::cpu_history_sample::Identity],
        operation: &Operation,
    ) -> Result<Box<AppProcessGroupsHistory>> {
        let reduction = Reduction::new(selected, target.kind, target.bytes, operation)?;
        self.process_groups_response_saved(
            &Query {
                selected,
                target,
                saved,
                context,
                operation,
                available: BYTES_MAX,
            },
            reduction,
        )
    }

    fn process_groups_response_saved(
        &self,
        query: &Query<'_, '_>,
        reduction: Box<Reduction<'_>>,
    ) -> Result<Box<AppProcessGroupsHistory>> {
        let mut lease = Box::new(Lease {
            history: self,
            resident: self.saved_residents.replace(ResidentCache::new()),
            groups: None,
            committed: false,
            available: 0,
        });

        lease.prepare(query.target, query.available);
        let result =
            query
                .saved
                .with_process_groups_replay(query.selected, query.target, |replay| {
                    Self::process_groups_response_saved_replay(query, replay, &mut lease, reduction)
                })?;

        query.operation.check()?;
        lease.committed = true;

        Ok(result)
    }

    /// The groups of the selected App, observed and then projected from the groups cache, the
    /// replay of the saved minutes and the parts sealed windows keep.
    fn process_groups_response_saved_replay(
        query: &Query<'_, '_>,
        replay: &mut crate::session::ProcessGroupsReplay,
        lease: &mut Lease<'_>,
        mut reduction: Box<Reduction<'_>>,
    ) -> Result<Box<AppProcessGroupsHistory>> {
        let cached =
            Self::process_groups_response_saved_restore(query, replay, lease, &mut reduction)?;
        Self::process_groups_response_saved_kept(query, replay, &mut reduction, Phase::Observe)?;
        reduction.context(query.context)?;
        reduction.choose()?;
        #[cfg(test)]
        BEFORE_PROJECT.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });

        query.operation.check()?;
        if cached {
            Self::process_groups_response_saved_cached(lease, &mut reduction, Phase::Project)?;
        } else {
            // Overflow may have removed facts used during Observe. Restore the entire plan.
            query
                .saved
                .process_groups_plan(replay, |_observation| Ok(false))?;
            query
                .saved
                .visit_process_groups(replay, |frame, previous, _predecessor| {
                    super::cohort::capture(frame, previous, query.target, query.operation)?
                        .visit(&mut reduction, Phase::Project)
                })?;
        }
        Self::process_groups_response_saved_kept(query, replay, &mut reduction, Phase::Project)?;

        if let Some(cache) = &lease.groups {
            query.saved.process_groups_cache_validate(cache)?;
        }

        query.operation.check()?;
        reduction.finish()
    }

    fn process_groups_response_saved_restore(
        query: &Query<'_, '_>,
        replay: &mut crate::session::ProcessGroupsReplay,
        lease: &mut Lease<'_>,
        reduction: &mut Reduction<'_>,
    ) -> Result<bool> {
        let space = lease.space();
        let mut cached = Self::process_groups_response_saved_restore_prepare(query, replay, lease)?;

        query.saved.process_groups_plan(replay, |observation| {
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
            Self::process_groups_response_saved_cached(lease, reduction, Phase::Observe)?;
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
                        cache.limit(space, query.selected.context_last_second());
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
                cache.limit(space, query.selected.context_last_second());
                cached = false;
            }
        }

        Ok(cached)
    }

    /// Visits in `phase` the groups of the selected App at each second of the windows whose
    /// parts keep them.
    fn process_groups_response_saved_kept(
        query: &Query<'_, '_>,
        replay: &crate::session::ProcessGroupsReplay,
        reduction: &mut Reduction<'_>,
        phase: Phase,
    ) -> Result<()> {
        query
            .saved
            .visit_process_groups_kept(replay, query.target, |part| {
                super::cohort::capture_part(part, query.operation)?.visit(reduction, phase)
            })
    }

    fn process_groups_response_saved_cached(
        lease: &Lease<'_>,
        reduction: &mut Reduction<'_>,
        phase: Phase,
    ) -> Result<()> {
        let cache = lease
            .groups
            .as_ref()
            .ok_or(Error::Bound("process group cache owner"))?;

        for point in cache.points() {
            point.visit(reduction, phase)?;
        }

        Ok(())
    }

    fn process_groups_response_saved_restore_prepare(
        query: &Query<'_, '_>,
        replay: &crate::session::ProcessGroupsReplay,
        lease: &mut Lease<'_>,
    ) -> Result<bool> {
        let space = lease.space();
        let Some(cache) = &mut lease.groups else {
            return Ok(false);
        };

        cache.retain(query.operation, |point| {
            query.saved.process_groups_cache_needed(replay, point)
        })?;

        query.saved.process_groups_cache_validate(cache)?;
        let cached = cache.prepare(space, query.selected.context_last_second());
        if !cached {
            cache.limit(space, query.selected.context_last_second());
        }

        Ok(cached)
    }
}

impl Lease<'_> {
    fn prepare(&mut self, target: &Target<'_>, available: usize) {
        let previous = self.resident.take_groups();
        // The open page's groups may use the whole budget, since leaving them no room
        // replays every saved minute twice on each reading. Other pages' caches give way
        // when this answer commits.
        self.available = available.min(BYTES_MAX);
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

        assert!(self.resident.bytes() <= BYTES_MAX);

        let resident = std::mem::replace(&mut self.resident, ResidentCache::new());
        self.history.saved_residents.replace(resident);
    }
}
