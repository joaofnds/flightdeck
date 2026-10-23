//! The store's recording adapter. At launch it reads the sealed minutes of the loaded hour, so a
//! window that opens draws the hour before the ticks the ring still holds, and the Apps of the
//! minutes of the live span.

use std::io;
use std::mem;
use std::panic;
use std::path::{Path, PathBuf};
use std::thread;

use collector::recording::{Registry, RegistryBuffer, read_identity};
use collector::ring::{Identity, RunIdentity};
use persistence::layout::{self, Kind, WINDOWS_PER_DAY, WindowKey};

use super::apps::{Kept, TickApps};
use super::{AppsSecond, Failure, MachineSecond, Missing, Reading, readings};
use crate::recording::{
    AppMinute, AppParts, AppRow, MachineMinute, Refused, SampleRow, SealedMinute, read_apps,
    read_machine,
};
use crate::session::{LEAD_IN_SECONDS, LOADED_SECONDS};

/// The sealed minutes the store reads at launch at most. The hour and its lead-in span 61
/// minutes, and each stop or recovery of a collector seals a shorter one.
const MINUTES_MAX: usize = 128;

/// How far before the newest sealed second the loaded hour and its lead-in reach.
const LOADED_NS: u64 = (LOADED_SECONDS + LEAD_IN_SECONDS) * 1_000_000_000;

/// How far before the newest sealed second the store reads Apps: the live span's 18 minutes.
const APPS_NS: u64 = 1_080 * 1_000_000_000;

const _: () = assert!(super::SECONDS_MAX == 1_080);

/// A sealed minute of the loaded hour, by its run, its marker and the newest wall time of its
/// ticks.
struct Found {
    run: RunIdentity,
    marker: PathBuf,
    wall_ns_max: u64,
}

/// Hands `take` the run, the second and the Apps of each tick the loaded hour's sealed minutes
/// hold, oldest first. The hour ends at the newest second the recording sealed, and only the
/// minutes of the live span before it give their Apps. Returns why the store holds less of the
/// hour than the recording sealed, where it does.
pub(super) fn load(
    root: &Path,
    mut take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> Option<Failure> {
    let mut found = Vec::with_capacity(MINUTES_MAX);
    if let Err(failure) = find(root, &mut found) {
        return Some(failure);
    }
    let apps_from_ns = found
        .first()
        .map_or(0, |newest| newest.wall_ns_max.saturating_sub(APPS_NS));
    found.reverse();

    let mut left_out = None;
    for mut batch in load_batches(root, &found, apps_from_ns) {
        for loaded in &mut batch.seconds {
            let apps = match &mut loaded.apps {
                Ok(apps) => TickApps::Loaded(apps),
                Err(missing) => TickApps::Missing(missing),
            };
            take(&loaded.run, &loaded.second, apps);
        }
        if let Some(failure) = batch.left_out {
            left_out = Some(failure);
        }
    }

    left_out
}

/// The threads that decode the loaded hour's sealed minutes at once.
const LOADERS_MAX: usize = 6;

/// The ticks a sealed minute holds where none was missed.
const TICKS_PER_MINUTE: usize = 60;

/// The seconds one loader decoded from its batch of minutes, oldest first, and why it holds
/// fewer than they sealed, where it does.
struct Batch {
    seconds: Vec<Loaded>,
    left_out: Option<Failure>,
}

/// A second a loader decoded, with its run, and its Apps or why it holds none.
struct Loaded {
    run: RunIdentity,
    second: MachineSecond,
    apps: Result<AppsSecond, Missing>,
}

/// Decodes the minutes of `found`, oldest first, in batches of neighbouring minutes, each
/// batch on a loader of its own, with the Apps of each minute whose ticks reach `apps_from_ns`.
/// Returns the batches in the order of `found`.
fn load_batches(root: &Path, found: &[Found], apps_from_ns: u64) -> Vec<Batch> {
    let minutes_per_batch = found.len().div_ceil(LOADERS_MAX).max(1);
    thread::scope(|scope| {
        let mut loaders = Vec::with_capacity(LOADERS_MAX);
        for (index, minutes) in found.chunks(minutes_per_batch).enumerate() {
            let before = (index * minutes_per_batch)
                .checked_sub(1)
                .and_then(|before| found.get(before));
            let loader = thread::Builder::new()
                .name(String::from("flightdeck-loader"))
                .spawn_scoped(scope, move || {
                    load_batch(root, before, minutes, apps_from_ns)
                });
            loaders.push(loader);
        }

        let mut batches = Vec::with_capacity(loaders.len());
        for loader in loaders {
            let batch = match loader {
                Ok(handle) => match handle.join() {
                    Ok(batch) => batch,
                    Err(panic) => panic::resume_unwind(panic),
                },
                Err(error) => Batch {
                    seconds: Vec::new(),
                    left_out: Some(Failure::Loader(error.kind())),
                },
            };
            batches.push(batch);
        }
        batches
    })
}

/// Decodes `minutes`, oldest first, with the Apps of each minute whose ticks reach
/// `apps_from_ns`. The first minute's first second takes its rates from the last tick of the
/// minute `before` it, which the loader before this one decodes too.
fn load_batch(root: &Path, before: Option<&Found>, minutes: &[Found], apps_from_ns: u64) -> Batch {
    let mut batch = Batch {
        seconds: Vec::with_capacity(minutes.len() * TICKS_PER_MINUTE),
        left_out: None,
    };
    let mut current = MachineMinute::default();
    let mut previous = MachineMinute::default();
    let mut apps = AppMinute::default();
    let mut previous_run = None;
    if let Some(before) = before {
        // The loader that decodes the minute before reports it where it does not decode.
        if decode(root, &before.marker, &mut previous, None).is_ok() {
            previous_run = Some(before.run);
        }
    }

    for minute in minutes {
        let read_apps = (minute.wall_ns_max >= apps_from_ns).then_some(&mut apps);
        let Ok(missing) = decode(root, &minute.marker, &mut current, read_apps) else {
            batch.left_out = Some(Failure::Refused);
            previous_run = None;
            continue;
        };

        // Rates never cross a run, so a minute's first second follows only its own run's.
        let before = if previous_run == Some(minute.run) {
            readings(&previous).last()
        } else {
            None
        };
        let decoded = Decoded {
            run: &minute.run,
            machine: &current,
            apps: missing.map_or(Ok(&apps), Err),
        };
        load_minute(&decoded, before, &mut batch.seconds);

        mem::swap(&mut current, &mut previous);
        previous_run = Some(minute.run);
    }

    batch
}

/// What a loader decoded of one sealed minute of run `run`.
struct Decoded<'minute> {
    run: &'minute RunIdentity,
    machine: &'minute MachineMinute,
    /// The Apps the minute keeps, or why the loader holds none of them.
    apps: Result<&'minute AppMinute, Missing>,
}

/// Pushes the run, the second and the Apps of each tick of `minute` onto `seconds`, its rates
/// over the interval since the reading before it, `before` for the minute's first.
fn load_minute(minute: &Decoded<'_>, before: Option<Reading<'_>>, seconds: &mut Vec<Loaded>) {
    let mut previous = before;
    let mut kept = KeptWalk::default();
    for reading in readings(minute.machine) {
        if let Some(second) = MachineSecond::of(&reading, previous.as_ref()) {
            let held = seconds.last().and_then(|loaded| loaded.apps.as_ref().ok());
            let apps = load_minute_apps(minute, second.publication, &mut kept, held);
            seconds.push(Loaded {
                run: *minute.run,
                second,
                apps,
            });
        }
        previous = Some(reading);
    }
}

/// The Apps `minute` keeps of the tick of publication `publication`, or why it gives none.
/// `kept` is where the walk through the minute's Apps stands, and an App the second `previous`
/// holds keeps the name it holds there.
fn load_minute_apps(
    minute: &Decoded<'_>,
    publication: u64,
    kept: &mut KeptWalk,
    previous: Option<&AppsSecond>,
) -> Result<AppsSecond, Missing> {
    let apps = minute.apps.as_ref().map_err(Clone::clone)?;
    let Some((sample, rows)) = kept.find(apps, publication) else {
        return Err(Missing::Refused(String::from(
            "the sealed minute keeps no sample of this second",
        )));
    };

    let mut second = AppsSecond::with_room();
    let kept = Kept {
        run: minute.run,
        sample,
        rows,
        minute: apps,
    };
    second.keep(&kept, previous);
    Ok(second)
}

/// Where a walk through a minute's kept samples and App rows, in publication order, stands.
#[derive(Default)]
struct KeptWalk {
    sample_index: usize,
    row_index: usize,
}

impl KeptWalk {
    /// The sample `minute` keeps of the tick of publication `publication` and its App rows,
    /// where it keeps one. The walk takes publications in increasing order.
    fn find<'minute>(
        &mut self,
        minute: &'minute AppMinute,
        publication: u64,
    ) -> Option<(&'minute SampleRow, &'minute [AppRow])> {
        let samples = &minute.samples[self.sample_index..];
        self.sample_index += samples
            .iter()
            .take_while(|sample| sample.publication < publication)
            .count();
        let sample = minute.samples.get(self.sample_index)?;
        if sample.publication != publication {
            return None;
        }

        let rows = &minute.apps[self.row_index..];
        let start = self.row_index
            + rows
                .iter()
                .take_while(|row| row.publication < publication)
                .count();
        let count = minute.apps[start..]
            .iter()
            .take_while(|row| row.publication == publication)
            .count();
        self.row_index = start + count;
        Some((sample, &minute.apps[start..self.row_index]))
    }
}

/// Verifies the minute the marker at `marker` seals and decodes its machine readings into
/// `machine`, and its Apps into `apps` where it is given. Returns why `apps` holds none of the
/// minute's Apps, where it holds none.
fn decode(
    root: &Path,
    marker: &Path,
    machine: &mut MachineMinute,
    apps: Option<&mut AppMinute>,
) -> Result<Option<Missing>, Refused> {
    let sealed = SealedMinute::open(root, marker)?;
    read_machine(&sealed, machine)?;

    let Some(apps) = apps else {
        return Ok(Some(Missing::Unread));
    };
    apps.clear();
    match read_apps(&sealed, apps) {
        Ok(AppParts::Read) => Ok(None),
        Ok(AppParts::Missing | AppParts::Version(_)) => Ok(Some(Missing::Unread)),
        Err(refused) => Ok(Some(Missing::Refused(decode_refusal(&refused)))),
    }
}

/// Why the store holds none of a sealed minute's Apps, where reading them was `refused`.
fn decode_refusal(refused: &Refused) -> String {
    match refused {
        Refused::Commitment(error) => format!("the sealed minute did not verify: {error}"),
        Refused::Decode(error) => format!("the sealed minute's Apps did not decode: {error}"),
    }
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
                    wall_ns_max: wall_ns.max,
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
