//! Complete bounded catalog discovery for the opt-in process-payload adapter.

use std::io;
use std::path::Path;

use collector::recording::{Registry, RegistryBuffer, read_identity};
use collector::ring::{Identity, RunIdentity};
use persistence::canonical::ReadWork;
use persistence::canonical::catalog::Catalog;
use persistence::{Error, Manifest};

use super::{CheckedCatalog, Found, LoadWindow, MINUTES_MAX, find_predecessor};
use crate::session::registered::{catalog_retry, first_retained_ns};
use crate::session::{LEAD_IN_SECONDS, LOADED_SECONDS};
use crate::store::Failure;

pub(super) fn find(
    root: &Path,
    found: &mut Vec<Found>,
    window: &LoadWindow<'_>,
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
    let catalog = super::find_window_catalog(root)?;
    find_windows(root, recording, &runs, &catalog, found, window)
}

/// Orders `found` newest first by run and keeps what a load continuing from `window.since` reads.
fn find_order(found: &mut Vec<Found>, runs: &[RunIdentity], window: &LoadWindow<'_>) {
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
}

/// The clock range comes from the newest saved window, and only the windows of that range are read.
fn find_windows(
    root: &Path,
    recording: Identity,
    runs: &[RunIdentity],
    catalog: &Catalog,
    found: &mut Vec<Found>,
    window: &LoadWindow<'_>,
) -> Result<CheckedCatalog, Failure> {
    let lookup = |read: &mut dyn FnMut() -> persistence::Result<_>| {
        catalog_retry(read).map_err(find_failure)
    };
    if let Some(since) = window.since {
        if !runs.contains(&since.run) {
            let saved = lookup(&mut || {
                Ok(catalog.window_at_or_before(&since.run, u64::MAX, &mut ReadWork::default())?)
            })?;
            if saved.is_none() {
                return Err(Failure::ChangedIdentity);
            }
        }
    }
    let mut newest = None;
    for run in runs.iter().rev() {
        newest = lookup(&mut || {
            Ok(catalog.window_at_or_before(run, u64::MAX, &mut ReadWork::default())?)
        })?;
        if newest.is_some() {
            break;
        }
    }
    let ends = find_windows_ends(root, catalog)?;
    let newest_ns = match newest {
        Some(manifest) => manifest.wall_ns.map(|wall| wall.max),
        None => ends.map(|(_first, last)| last),
    };
    let first_ns = newest_ns
        .map(|newest| newest.saturating_sub((window.seconds + LEAD_IN_SECONDS) * 1_000_000_000));
    let mut minutes = catalog_retry(|| {
        catalog.windows_in(first_ns.unwrap_or(0), u64::MAX, &mut ReadWork::default())
    })
    .map_err(find_failure)?;
    let unknown_clock_ticks =
        catalog_retry(|| Ok(catalog.unknown_count_within_page(&mut ReadWork::default())?))
            .map_err(find_failure)?;
    if unknown_clock_ticks != Some(0) {
        minutes.extend(
            catalog_retry(|| catalog.windows_without_clock(&mut ReadWork::default()))
                .map_err(find_failure)?,
        );
    }
    find_windows_range(root, catalog, &minutes, runs, found)?;
    find_order(found, runs, window);

    Ok(CheckedCatalog {
        recording,
        newest_run: runs.last().copied(),
        first_ns,
        windows: u32::try_from(minutes.len())
            .map_err(|_| Failure::Commits(io::ErrorKind::InvalidData))?,
        first_known_second: ends.map(|(first, _last)| first / 1_000_000_000),
        last_known_second: ends.map(|(_first, last)| last / 1_000_000_000),
        unknown_clock_ticks,
        // With no clock to start from, the range took in every window the recording holds.
        decoded_is_whole: first_ns.is_none(),
    })
}

/// The clocks of the recording's oldest and newest readings.
fn find_windows_ends(root: &Path, catalog: &Catalog) -> Result<Option<(u64, u64)>, Failure> {
    catalog_retry(|| {
        let oldest = catalog.oldest_known(&mut ReadWork::default())?;
        let newest = catalog.newest_known(&mut ReadWork::default())?;
        let (Some(oldest), Some(newest)) = (oldest, newest) else {
            return Ok(None);
        };
        let first_ns = first_retained_ns(root, catalog, &oldest, newest.wall_ns)?;

        Ok(Some((first_ns, newest.wall_ns)))
    })
    .map_err(find_failure)
}

fn find_windows_range(
    root: &Path,
    catalog: &Catalog,
    minutes: &[Manifest],
    runs: &[RunIdentity],
    found: &mut Vec<Found>,
) -> Result<(), Failure> {
    for manifest in minutes {
        if found.len() == MINUTES_MAX {
            return Err(Failure::Commits(io::ErrorKind::InvalidData));
        }
        let run = match runs.iter().find(|run| run.run == manifest.identity.run.run) {
            Some(run) => *run,
            None => manifest.identity.run,
        };
        let previous = match manifest.publication_first.checked_sub(1) {
            Some(before) => catalog_retry(|| {
                Ok(catalog.window_at_or_before(
                    &manifest.identity.run,
                    before,
                    &mut ReadWork::default(),
                )?)
            })
            .map_err(find_failure)?,
            None => None,
        };
        found.push(Found {
            run,
            marker: persistence::commitment_path(root, manifest),
            wall_ns_max: manifest.wall_ns.map(|wall| wall.max),
            publication_end: manifest.publication_end,
            previous_source: previous.map(|previous| super::BoundaryKey {
                run: previous.identity.run,
                marker: persistence::commitment_path(root, &previous),
                publication_end: previous.publication_end,
            }),
        });
    }

    Ok(())
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

fn find_failure(error: Error) -> Failure {
    let kind = match error {
        Error::Io(error) => error.kind(),
        Error::Bound(_) | Error::Engine(_) => io::ErrorKind::InvalidData,
    };
    Failure::Commits(kind)
}
