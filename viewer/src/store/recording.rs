//! The store's recording adapter. At launch it reads the sealed minutes of the loaded hour, so a
//! window that opens draws the hour before the ticks the ring still holds.

use std::io;
use std::mem;
use std::path::{Path, PathBuf};

use collector::recording::{Registry, RegistryBuffer, read_identity};
use collector::ring::{Identity, RunIdentity};
use persistence::layout::{self, Kind, WINDOWS_PER_DAY, WindowKey};

use super::{Failure, MachineSecond, Reading, readings};
use crate::recording::{MachineMinute, Refused, SealedMinute, read_machine};
use crate::session::{LEAD_IN_SECONDS, LOADED_SECONDS};

/// The sealed minutes the store reads at launch at most. The hour and its lead-in span 61
/// minutes, and each stop or recovery of a collector seals a shorter one.
const MINUTES_MAX: usize = 128;

/// How far before the newest sealed second the loaded hour and its lead-in reach.
const LOADED_NS: u64 = (LOADED_SECONDS + LEAD_IN_SECONDS) * 1_000_000_000;

/// A sealed minute of the loaded hour, by its run and its marker.
struct Found {
    run: RunIdentity,
    marker: PathBuf,
}

/// Hands `take` the run and the second of each tick the loaded hour's sealed minutes hold,
/// oldest first. The hour ends at the newest second the recording sealed. Returns why the store
/// holds less of the hour than the recording sealed, where it does.
pub(super) fn load(
    root: &Path,
    mut take: impl FnMut(&RunIdentity, &MachineSecond),
) -> Option<Failure> {
    let mut found = Vec::with_capacity(MINUTES_MAX);
    if let Err(failure) = find(root, &mut found) {
        return Some(failure);
    }

    let mut left_out = None;
    let mut current = MachineMinute::default();
    let mut previous = MachineMinute::default();
    let mut previous_run = None;
    for minute in found.iter().rev() {
        if decode(root, &minute.marker, &mut current).is_err() {
            left_out = Some(Failure::Refused);
            previous_run = None;
            continue;
        }

        // Rates never cross a run, so a minute's first second follows only its own run's.
        let before = if previous_run == Some(minute.run) {
            readings(&previous).last()
        } else {
            None
        };
        load_minute(&current, before, &minute.run, &mut take);

        mem::swap(&mut current, &mut previous);
        previous_run = Some(minute.run);
    }

    left_out
}

/// Hands `take` the second of each tick of `minute` of run `run`, its rates over the interval
/// since the reading before it, `before` for the minute's first.
fn load_minute(
    minute: &MachineMinute,
    before: Option<Reading<'_>>,
    run: &RunIdentity,
    take: &mut impl FnMut(&RunIdentity, &MachineSecond),
) {
    let mut previous = before;
    for reading in readings(minute) {
        if let Some(second) = MachineSecond::of(&reading, previous.as_ref()) {
            take(run, &second);
        }
        previous = Some(reading);
    }
}

/// Verifies the minute the marker at `marker` seals and decodes its machine readings into
/// `minute`.
fn decode(root: &Path, marker: &Path, minute: &mut MachineMinute) -> Result<(), Refused> {
    let sealed = SealedMinute::open(root, marker)?;
    read_machine(&sealed, minute)
}

/// Finds the sealed minutes of the loaded hour into `found`, newest first, from the newest run
/// the recording registers back.
fn find(root: &Path, found: &mut Vec<Found>) -> Result<(), Failure> {
    let recording = read_identity(root).map_err(|error| Failure::Recording(error.kind()))?;
    let mut buffer: RegistryBuffer = [0; _];
    let registry = Registry::read_into(&root.join("runs"), &mut buffer)
        .map_err(|error| Failure::Registry(error.kind()))?;
    let mut runs = Vec::new();
    for run in registry.runs() {
        let run = run.map_err(|error| Failure::Registry(error.kind()))?;
        runs.push(run.identity);
    }

    let mut oldest_ns = None;
    for run in runs.iter().rev() {
        let finding = FindRun {
            root,
            recording,
            run,
        };
        let more = finding
            .find(&mut oldest_ns, found)
            .map_err(|error| Failure::Commits(error.kind()))?;
        if !more {
            return Ok(());
        }
    }

    Ok(())
}

/// The search for the sealed minutes of one run of the recording.
struct FindRun<'a> {
    root: &'a Path,
    recording: Identity,
    run: &'a RunIdentity,
}

impl FindRun<'_> {
    /// Finds the run's sealed minutes newer than `oldest_ns` into `found`, newest first, where
    /// the first minute found sets `oldest_ns`. Returns false once a minute older than that or
    /// a full `found` ends the search.
    fn find(&self, oldest_ns: &mut Option<u64>, found: &mut Vec<Found>) -> io::Result<bool> {
        let run_path = layout::run(self.root, Kind::Commits, self.run.run);
        let days = match newest_days(&run_path) {
            Ok(days) => days,
            Err(error) => {
                // A run that sealed no minute yet has no commits.
                if error.kind() == io::ErrorKind::NotFound {
                    return Ok(true);
                }
                return Err(error);
            }
        };

        let mut windows = Vec::with_capacity(WINDOWS_PER_DAY_COUNT);
        for day in days.into_iter().flatten() {
            windows.clear();
            find_windows(&run_path.join(format!("{day:020}")), &mut windows)?;
            windows.sort_unstable_by(|left, right| right.cmp(left));

            for window in &windows {
                let marker = WindowKey {
                    run: self.run.run,
                    window: *window,
                }
                .marker(self.root);
                let manifest = persistence::list_committed(self.root, &marker, self.recording)?;
                // A minute of unknown wall time has no place on a chart.
                let Some(wall_ns) = manifest.wall_ns else {
                    continue;
                };

                let oldest = *oldest_ns.get_or_insert(wall_ns.max.saturating_sub(LOADED_NS));
                if wall_ns.max < oldest {
                    return Ok(false);
                }
                if found.len() == MINUTES_MAX {
                    return Ok(false);
                }
                found.push(Found {
                    run: *self.run,
                    marker,
                });
            }
        }

        Ok(true)
    }
}

/// The windows a day of a run holds at most.
const WINDOWS_PER_DAY_COUNT: usize = 1_440;

const _: () = assert!(WINDOWS_PER_DAY == 1_440);

/// The two newest days of the run whose commits are at `run_path`, newest first. The loaded
/// hour spans at most two, since a minute of a run takes at least a minute of the wall clock.
fn newest_days(run_path: &Path) -> io::Result<[Option<u64>; 2]> {
    let mut days = [None, None];
    layout::visit_names(run_path, |name| -> io::Result<()> {
        let day = Some(layout::validate_day(name)?);
        if day > days[0] {
            days[1] = days[0];
            days[0] = day;
        } else {
            if day > days[1] {
                days[1] = day;
            }
        }
        Ok(())
    })?;

    Ok(days)
}

/// The window of each minute sealed in the day at `day_path`, into `windows`.
fn find_windows(day_path: &Path, windows: &mut Vec<u64>) -> io::Result<()> {
    layout::visit_names(day_path, |name| -> io::Result<()> {
        if windows.len() == WINDOWS_PER_DAY_COUNT {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        // A marker's name is its window's number as the persister names a day, as its own
        // walk of the markers reads it.
        let Some(window) = name.strip_suffix(".seal") else {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        };
        windows.push(layout::validate_day(window)?);
        Ok(())
    })
}
