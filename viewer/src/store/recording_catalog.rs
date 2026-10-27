//! Complete bounded catalog discovery for the opt-in process-payload adapter.

use std::io;
use std::path::Path;

use collector::recording::{Registry, RegistryBuffer, read_identity};
use collector::ring::RunIdentity;
use persistence::{Error, Manifest};

use super::{Found, LoadWindow, MINUTES_MAX, find_unread};
use crate::cancellation::Operation;
use crate::session::{LEAD_IN_SECONDS, LOADED_SECONDS};
use crate::store::Failure;

pub(super) fn find(
    root: &Path,
    found: &mut Vec<Found>,
    window: &LoadWindow<'_>,
    operation: &Operation,
) -> Result<(), Failure> {
    assert!(window.seconds > 0);
    assert!(window.seconds <= LOADED_SECONDS);
    let recording = read_identity(root).map_err(|error| Failure::Recording(error.kind()))?;
    if let Some(since) = window.since {
        if since.run.recording != recording {
            return Err(Failure::ForeignRecording);
        }
    }
    let mut buffer: RegistryBuffer = [0; _];
    let registry = Registry::read_into(&root.join("runs"), &mut buffer)
        .map_err(|error| Failure::Registry(error.kind()))?;
    let runs = registry
        .runs()
        .map(|run| run.map(|run| run.identity))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|error| Failure::Registry(error.kind()))?;
    let mut catalog = Vec::new();
    let base = persistence::layout::base(root, persistence::layout::Kind::Commits);
    if !base
        .try_exists()
        .map_err(|error| Failure::Commits(error.kind()))?
    {
        find_checkpoint(window, &runs, &catalog)?;
        return Ok(());
    }
    crate::session::discover(root, recording, &mut catalog, operation).map_err(find_failure)?;
    find_checkpoint(window, &runs, &catalog)?;
    let first_ns = find_newest(&catalog, &runs)
        .map(|newest| newest.saturating_sub((window.seconds + LEAD_IN_SECONDS) * 1_000_000_000));
    find_range(root, &catalog, &runs, first_ns, found)?;
    found.sort_unstable_by_key(|minute| {
        let order = runs
            .iter()
            .position(|run| *run == minute.run)
            .map_or(0, |index| index + 1);
        (order, minute.run.run.0, minute.publication_end)
    });
    found.reverse();
    if window.unread {
        if let Some(since) = window.since {
            find_unread(found, since);
        }
    }
    Ok(())
}

fn find_checkpoint(
    window: &LoadWindow<'_>,
    runs: &[RunIdentity],
    catalog: &[Manifest],
) -> Result<(), Failure> {
    if let Some(since) = window.since {
        if !runs.contains(&since.run) {
            if !catalog
                .iter()
                .any(|manifest| manifest.identity.run == since.run)
            {
                return Err(Failure::ChangedIdentity);
            }
        }
    }
    Ok(())
}

/// Uses the latest sealed window of the newest registered run that has one.
/// Unknown bounds there cannot borrow an older clock. When no registered run has a sealed
/// window, the catalog's known maximum is the fallback.
fn find_newest(catalog: &[Manifest], runs: &[RunIdentity]) -> Option<u64> {
    for run in runs.iter().rev() {
        let latest = catalog
            .iter()
            .filter(|manifest| manifest.identity.run.run == run.run)
            .max_by_key(|manifest| manifest.identity.window);
        if let Some(latest) = latest {
            return latest.wall_ns.map(|wall| wall.max);
        }
    }

    catalog
        .iter()
        .filter_map(|manifest| manifest.wall_ns.map(|wall| wall.max))
        .max()
}

fn find_range(
    root: &Path,
    catalog: &[Manifest],
    runs: &[RunIdentity],
    first_ns: Option<u64>,
    found: &mut Vec<Found>,
) -> Result<(), Failure> {
    for manifest in catalog {
        if let Some(first_ns) = first_ns {
            if let Some(wall) = manifest.wall_ns {
                if wall.max < first_ns {
                    continue;
                }
            }
        }
        if found.len() == MINUTES_MAX {
            return Err(Failure::Commits(io::ErrorKind::InvalidData));
        }
        let run = match runs.iter().find(|run| run.run == manifest.identity.run.run) {
            Some(run) => *run,
            None => manifest.identity.run,
        };
        found.push(Found {
            run,
            marker: persistence::layout::WindowKey::from(&manifest.identity).marker(root),
            wall_ns_max: manifest.wall_ns.map(|wall| wall.max),
            publication_end: manifest.publication_end,
        });
    }
    Ok(())
}

fn find_failure(error: Error) -> Failure {
    let kind = match error {
        Error::Io(error) => error.kind(),
        Error::Bound(_) | Error::Engine(_) => io::ErrorKind::InvalidData,
    };
    Failure::Commits(kind)
}
