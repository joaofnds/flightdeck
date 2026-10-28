//! Complete bounded catalog discovery for the opt-in process-payload adapter.

use std::io;
use std::path::Path;

use collector::recording::{Registry, RegistryBuffer, read_identity};
use collector::ring::{Identity, RunIdentity};
use persistence::{Error, Manifest};

use super::{CheckedCatalog, Found, LoadWindow, MINUTES_MAX, find_predecessor};
use crate::cancellation::Operation;
use crate::session::{LEAD_IN_SECONDS, LOADED_SECONDS};
use crate::store::Failure;

pub(super) fn find(
    root: &Path,
    found: &mut Vec<Found>,
    window: &LoadWindow<'_>,
    operation: &Operation,
) -> Result<CheckedCatalog, Failure> {
    assert!(window.seconds > 0);
    assert!(window.seconds <= LOADED_SECONDS);
    let recording = read_identity(root).map_err(|error| Failure::Recording(error.kind()))?;
    if let Some(since) = window.since {
        if since.run.recording != recording {
            return Err(Failure::ForeignRecording);
        }
    }
    let runs = find_runs(root, recording)?;
    let mut catalog = Vec::new();
    let base = persistence::layout::base(root, persistence::layout::Kind::Commits);
    if !base
        .try_exists()
        .map_err(|error| Failure::Commits(error.kind()))?
    {
        find_checkpoint(window, &runs, &catalog)?;
        return metadata(recording, &runs, &catalog, None);
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
            if let Some(predecessor) = find_predecessor(found, since) {
                found.truncate(predecessor + 1);
            }
        }
    }
    metadata(recording, &runs, &catalog, first_ns)
}

fn metadata(
    recording: Identity,
    runs: &[RunIdentity],
    catalog: &[Manifest],
    first_ns: Option<u64>,
) -> Result<CheckedCatalog, Failure> {
    let windows =
        u32::try_from(catalog.len()).map_err(|_| Failure::Commits(io::ErrorKind::InvalidData))?;
    Ok(CheckedCatalog {
        recording,
        newest_run: runs.last().copied(),
        first_ns,
        windows,
        first_known_second: catalog
            .iter()
            .filter_map(|minute| minute.wall_ns)
            .map(|wall| wall.min / 1_000_000_000)
            .min(),
        last_known_second: catalog
            .iter()
            .filter_map(|minute| minute.wall_ns)
            .map(|wall| wall.max / 1_000_000_000)
            .max(),
        unknown_clock_ticks: catalog.is_empty().then_some(0),
    })
}

pub(super) fn revalidate(root: &Path, checked: &CheckedCatalog) -> Result<(), Failure> {
    let recording = read_identity(root).map_err(|error| Failure::Recording(error.kind()))?;
    if recording != checked.recording {
        return Err(Failure::ForeignRecording);
    }
    let runs = find_runs(root, recording)?;
    if runs.last().copied() != checked.newest_run {
        return Err(Failure::ChangedIdentity);
    }

    Ok(())
}

fn find_runs(root: &Path, recording: Identity) -> Result<Vec<RunIdentity>, Failure> {
    let mut buffer: RegistryBuffer = [0; _];
    let registry = Registry::read_into(&root.join("runs"), &mut buffer)
        .map_err(|error| Failure::Registry(error.kind()))?;
    let runs = registry
        .runs()
        .map(|run| run.map(|run| run.identity))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|error| Failure::Registry(error.kind()))?;
    for run in &runs {
        if run.recording != recording {
            return Err(Failure::ForeignRecording);
        }
    }

    Ok(runs)
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
            previous_source: catalog
                .iter()
                .filter(|previous| {
                    if previous.identity.run != manifest.identity.run {
                        return false;
                    }

                    previous.publication_end <= manifest.publication_first
                })
                .max_by_key(|previous| previous.publication_end)
                .map(|previous| super::BoundaryKey {
                    run: previous.identity.run,
                    marker: persistence::layout::WindowKey::from(&previous.identity).marker(root),
                    publication_end: previous.publication_end,
                }),
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
