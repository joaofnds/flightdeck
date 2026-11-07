//! Publication admission joins independently prepared facets before advancing the source.
use std::path::Path;
use std::sync::Arc;

use super::apps::TickApps;
use super::native::{NativeRead, PrepareError, Prepared};
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
) -> Result<NativeRead, Failure> {
    let budget = Arc::clone(
        held.payload_budget
            .as_ref()
            .unwrap_or_else(|| unreachable!("only complete Store reads are cancellable")),
    );
    let mut progress = NativeRead::Idle;
    for _ in 0..super::ring::TICKS_PER_READ_MAX {
        held.release_views();
        let previous = held.apps.newest_held().map(Arc::clone);
        let read = ring.read_prepared(&budget, operation, previous.as_deref(), |run, prepared| {
            take(held, run, prepared)
        });
        match read.map_err(read_failure)? {
            NativeRead::Accepted => progress = NativeRead::Accepted,
            NativeRead::Missed => progress = NativeRead::Missed,
            NativeRead::Idle => return Ok(progress),
            NativeRead::Blocked => return Ok(NativeRead::Blocked),
        }
    }

    Ok(progress)
}

fn take(held: &mut Held, run: &collector::ring::RunIdentity, prepared: &mut Prepared) -> bool {
    match &prepared.machine {
        Some(machine) => held.take(run, machine, TickApps::Loaded(&mut prepared.apps)),
        None => true,
    }
}

fn read_failure(error: PrepareError) -> Failure {
    match error {
        PrepareError::Admission(_) => Failure::Budget,
        PrepareError::Source(persistence::Error::Io(error)) => Failure::Ring(error.kind()),
        PrepareError::Source(
            persistence::Error::Bound(_)
            | persistence::Error::Engine(_)
            | persistence::Error::SecondExpired,
        ) => Failure::Refused,
    }
}
