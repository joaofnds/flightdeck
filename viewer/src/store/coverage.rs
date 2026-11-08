//! Query coverage comes from checked catalog input and the drained source, never a View version.
use super::query::CheckedCoverage;
use super::recording::CheckedCatalog;
use super::{Failure, Held, Ring, SourceGeneration};
use crate::session::ContextRange;

#[cfg(test)]
#[path = "coverage_tests.rs"]
mod tests;

pub(super) fn accept_catalog(
    held: &mut Held,
    catalog: Option<CheckedCatalog>,
) -> persistence::Result<()> {
    held.source_generation = held
        .source_generation
        .checked_add(1)
        .ok_or(persistence::Error::Bound("Store source generations"))?;
    held.checked_catalog = catalog;

    Ok(())
}

pub(super) fn source_changed(held: &Held, ring: &Ring) -> bool {
    let Some(catalog) = &held.checked_catalog else {
        return false;
    };
    let Some(identity) = ring.identity() else {
        return false;
    };

    catalog.newest_run != Some(identity)
}

pub(super) fn checked(held: &Held, ring: &mut Ring) -> Result<Option<CheckedCoverage>, Failure> {
    let Some(catalog) = &held.checked_catalog else {
        return Ok(None);
    };
    if source_changed(held, ring) {
        return Ok(None);
    }
    // A load that left out the newest run's sealed minute has no saved reading to prove the ring
    // begins from, and it already says what it lacks. Whatever older minute the load left out, a
    // ring that follows on from a saved minute proves where it begins, and so does the ring of a
    // run that has sealed no minute, which begins at its run's first reading or loses seconds.
    let requires_ring = catalog.newest_run.is_some()
        && (held.left_out.is_none() || ring.follows_saved() || !catalog.newest_run_sealed);
    if requires_ring {
        if !ring.complete(&catalog.recording) {
            return Ok(None);
        }
    }
    if !ring.caught_up()? {
        return Ok(None);
    }
    let source = SourceGeneration {
        recording: catalog.recording,
        generation: held.source_generation,
    };
    let Some(wall) = held.span.held().map(|second| second.wall_ns).max() else {
        return Ok(Some(CheckedCoverage::checked(source, None, requires_ring)));
    };
    let Some(end_ns) = (wall / 1_000_000_000)
        .checked_add(1)
        .and_then(|second| second.checked_mul(1_000_000_000))
    else {
        return Ok(None);
    };
    let first_ns = catalog.first_ns.unwrap_or(0);
    if first_ns >= end_ns {
        return Ok(None);
    }

    Ok(Some(CheckedCoverage::checked(
        source,
        Some(ContextRange { first_ns, end_ns }),
        requires_ring,
    )))
}
