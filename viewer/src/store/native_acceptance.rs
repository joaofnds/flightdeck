//! Publication admission joins independently prepared facets before advancing the source.
use std::path::Path;
use std::sync::Arc;

use super::apps::TickApps;
use super::native::{NativeRead, PrepareError, Prepared};
use super::raw_cache::RawCache;
use super::{ByteBudget, Failure, Held, Ring};
use crate::cancellation::Operation;

#[cfg(test)]
#[path = "native_acceptance_tests.rs"]
mod tests;

pub(super) fn make_ring(root: &Path, budget: Option<&Arc<ByteBudget>>) -> Ring {
    match budget {
        Some(budget) => Ring::with_budget(root.to_owned(), Arc::clone(budget)),
        None => Ring::new(root.to_owned()),
    }
}

pub(super) fn read_native(
    held: &mut Held,
    ring: &mut Ring,
    operation: &Operation,
) -> Result<(), Failure> {
    let budget = Arc::clone(
        held.payload_budget
            .as_ref()
            .unwrap_or_else(|| unreachable!("only complete Store reads are cancellable")),
    );
    if held.raw.is_none() {
        held.raw = Some(RawCache::new(&budget).map_err(|_| Failure::Budget)?);
    }
    for _ in 0..super::ring::TICKS_PER_READ_MAX {
        let previous = held.apps.newest_held().map(Arc::clone);
        let mut refused = false;
        let read = ring.read_prepared(&budget, operation, previous.as_deref(), |run, prepared| {
            take(held, run, prepared, &mut refused)
        });
        if refused {
            return Err(Failure::Refused);
        }
        match read.map_err(read_failure)? {
            NativeRead::Accepted | NativeRead::Missed => {}
            NativeRead::Idle | NativeRead::Blocked => return Ok(()),
        }
    }

    Ok(())
}

fn take(
    held: &mut Held,
    run: &collector::ring::RunIdentity,
    prepared: &mut Prepared,
    refused: &mut bool,
) -> bool {
    let raw = match prepared.raw() {
        Ok(raw) => {
            let cache = held
                .raw
                .as_ref()
                .unwrap_or_else(|| unreachable!("raw retention"));
            if cache.admit(&prepared.observation, raw).is_err() {
                *refused = true;
                return false;
            }

            Some(Arc::clone(raw))
        }
        Err(_) => None,
    };
    if let Some(machine) = &prepared.machine {
        if !held.take(run, machine, TickApps::Loaded(&mut prepared.apps)) {
            return false;
        }
    }
    if let Some(raw) = raw {
        let cache = held
            .raw
            .as_mut()
            .unwrap_or_else(|| unreachable!("raw retention"));
        assert!(cache.insert(prepared.observation, raw).is_ok());
    }

    true
}

fn read_failure(error: PrepareError) -> Failure {
    match error {
        PrepareError::Admission(_) => Failure::Budget,
        PrepareError::Source(persistence::Error::Io(error)) => Failure::Ring(error.kind()),
        PrepareError::Source(persistence::Error::Bound(_) | persistence::Error::Engine(_)) => {
            Failure::Refused
        }
    }
}
