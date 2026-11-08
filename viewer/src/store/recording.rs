//! The store's recording adapter. At launch it reads the sealed minutes of the loaded hour, so a
//! window that opens draws the hour before the ticks the ring still holds, and the Apps of the
//! minutes of the live span.

use std::io;
use std::mem;
use std::ops::Range;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

#[path = "recording_payload.rs"]
mod payload;

#[path = "recording_catalog.rs"]
mod catalog;

#[path = "recording_startup.rs"]
pub(super) mod staged;

#[cfg(test)]
#[path = "recording_buffer_tests.rs"]
mod buffer_tests;

#[cfg(test)]
#[path = "recording_discovery_tests.rs"]
mod discovery_tests;

use collector::ring::{Identity, RunIdentity};
use persistence::canonical::catalog::Catalog;

use super::apps::{Kept, TickApps};
use super::stretch::{Folded, Folding};
use super::{AppsSecond, AppsStretch, Failure, MachineSecond, Missing, Reading, readings};
use crate::recording::{
    AppMinute, AppParts, AppRow, MachineMinute, Refused, SampleRow, SealedMinute, read_apps,
    read_machine,
};
use crate::session::LOADED_SECONDS;
use crate::session::registered::catalog_retry;

/// Accepted sealed observations, their incomplete-read status and their independent predecessors.
pub(super) struct Outcome {
    pub left_out: Option<Failure>,
    pub terminal: Option<Terminal>,
    pub checked_catalog: Option<CheckedCatalog>,
    /// The catalog of a load that continued the store's seconds, which decoded none of the older
    /// minutes and so checks the catalog only for a store that holds them already.
    pub continued_catalog: Option<CheckedCatalog>,
    pub decoded_catalog: DecodedCatalog,
}

/// Whether the seconds a load hands over continue those the store holds or replace them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Continuity {
    Replace,
    Extend,
}

/// Source discovery checked all catalog windows at or beyond this actual pruning floor.
/// No floor means all discovered windows were decoded, including an empty catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CheckedCatalog {
    pub recording: Identity,
    pub newest_run: Option<RunIdentity>,
    /// Whether `newest_run` has sealed a minute, whose last reading its ring follows on from.
    pub newest_run_sealed: bool,
    pub first_ns: Option<u64>,
    pub windows: u32,
    pub first_known_second: Option<u64>,
    pub last_known_second: Option<u64>,
    pub unknown_clock_ticks: Option<u64>,
    /// Whether decoding every minute `windows` counts reads all the recording holds, which a
    /// registered recording's catalog needs no decoding to answer.
    pub decoded_is_whole: bool,
}

#[derive(Debug, Default)]
pub(super) struct DecodedCatalog {
    pub windows: u32,
    pub first_known_second: Option<u64>,
    pub last_known_second: Option<u64>,
    pub unknown_clock_ticks: u64,
}

impl DecodedCatalog {
    fn observe(&mut self, wall_ns: Option<u64>) {
        if let Some(wall_ns) = wall_ns {
            let second = wall_ns / 1_000_000_000;
            self.first_known_second = Some(
                self.first_known_second
                    .map_or(second, |first| first.min(second)),
            );
            self.last_known_second = Some(
                self.last_known_second
                    .map_or(second, |last| last.max(second)),
            );
        } else {
            self.unknown_clock_ticks += 1;
        }
    }

    fn extend(&mut self, other: &Self) {
        self.windows += other.windows;
        self.unknown_clock_ticks += other.unknown_clock_ticks;
        if let Some(first) = other.first_known_second {
            self.first_known_second =
                Some(self.first_known_second.map_or(first, |own| own.min(first)));
        }
        if let Some(last) = other.last_known_second {
            self.last_known_second = Some(self.last_known_second.map_or(last, |own| own.max(last)));
        }
    }
}

/// The last raw machine tick and process frame, accepted only by the same run's ring.
pub(super) struct Terminal {
    pub run: RunIdentity,
    pub machine: MachineMinute,
    pub process: Option<crate::recording::Before>,
    pub machine_reservation: Option<super::ByteReservation>,
}

impl Terminal {
    /// The terminal of `run` from `machine`, the last tick a decoder kept, where it kept one.
    fn from_minute(
        run: RunIdentity,
        machine: MachineMinute,
        process: Option<crate::recording::Before>,
    ) -> Option<Self> {
        if machine.ticks.is_empty() {
            return None;
        }
        assert_eq!(machine.ticks.len(), 1);

        Some(Self {
            run,
            machine,
            process,
            machine_reservation: None,
        })
    }
}

/// Decodes and consumes the payload prefix one minute at a time under caller cancellation.
#[cfg(test)]
pub(super) fn load_payload(
    root: &Path,
    budget: &Arc<super::ByteBudget>,
    operation: &crate::cancellation::Operation,
    take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Outcome> {
    load_payload_using(
        root,
        budget,
        operation,
        &persistence::engine::AdmittedTable::from_file_budgeted,
        take,
    )
}

#[cfg(test)]
pub(super) fn load_payload_using(
    root: &Path,
    budget: &Arc<super::ByteBudget>,
    operation: &crate::cancellation::Operation,
    capture: &crate::recording::InputCapture<'_>,
    mut take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Outcome> {
    load_payload_window_using(
        root,
        budget,
        operation,
        capture,
        &LoadWindow::all(None),
        |_, run, second, apps| take(run, second, apps),
    )
}

/// Decodes the minutes sealed after `since`, after the newest minute that holds `since`'s run no
/// further than `since`, which seeds the rates of the minute after it. Where the loaded hour
/// holds no such minute, it decodes the whole hour, and `take` hears that its seconds replace
/// the store's.
pub(super) fn load_payload_since(
    root: &Path,
    budget: &Arc<super::ByteBudget>,
    operation: &crate::cancellation::Operation,
    since: Option<&super::Newest>,
    take: impl FnMut(Continuity, &RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Outcome> {
    load_payload_window_using(
        root,
        budget,
        operation,
        &persistence::engine::AdmittedTable::from_file_budgeted,
        &LoadWindow::all(since),
        take,
    )
}

/// Decodes the loaded hour's sealed minutes again, checking that `since` is of this recording.
pub(super) fn load_payload_hour(
    root: &Path,
    budget: &Arc<super::ByteBudget>,
    operation: &crate::cancellation::Operation,
    since: Option<&super::Newest>,
    mut take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Outcome> {
    load_payload_window_using(
        root,
        budget,
        operation,
        &persistence::engine::AdmittedTable::from_file_budgeted,
        &LoadWindow {
            since,
            seconds: LOADED_SECONDS,
            unread: false,
        },
        |_, run, second, apps| take(run, second, apps),
    )
}

fn load_payload_window_using(
    root: &Path,
    budget: &Arc<super::ByteBudget>,
    operation: &crate::cancellation::Operation,
    capture: &crate::recording::InputCapture<'_>,
    window: &LoadWindow<'_>,
    take: impl FnMut(Continuity, &RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Outcome> {
    operation.check()?;
    let input = payload::LoadInput {
        root,
        budget,
        operation,
        capture,
    };
    let Some(since) = window.since.filter(|_| window.unread) else {
        let (found, discovered) = load_payload_listed(root, window);
        return load_payload_found(&input, window, found, discovered, take);
    };

    thread::scope(|scope| {
        // A listing that cannot start beside the walk runs before it, as without one.
        let Ok(finder) = load_payload_finder(scope, root, window) else {
            let (found, discovered) = load_payload_listed(root, window);
            return load_payload_found(&input, window, found, discovered, take);
        };
        // A walk that fails leaves the load to the listing, which says why the recording cannot be
        // read.
        let walked = match catalog::walk(root, since, window) {
            Ok(Some(walked)) if walked.len() > 1 => Some(walked),
            Ok(_) | Err(_) => None,
        };
        let Some(walked) = walked else {
            let (found, discovered) = load_payload_join(finder);
            return load_payload_found(&input, window, found, discovered, take);
        };

        load_payload_walked(&input, window, finder, walked, take)
    })
}

/// What listing the loaded hour found, newest first, and of its catalog or why it could not.
type Listed = (Vec<Found>, Result<CheckedCatalog, Failure>);

/// Loads `walked`, the minutes a walk back from the newest found newest first, while `finder`
/// lists the loaded hour, which must find the same minutes before the load hands any second to
/// `take`. Where the listing found other minutes, the work of the load it refused counts as
/// discarded, and the minutes the listing found load instead.
fn load_payload_walked(
    input: &payload::LoadInput<'_>,
    window: &LoadWindow<'_>,
    finder: thread::ScopedJoinHandle<'_, Listed>,
    mut walked: Vec<Found>,
    mut take: impl FnMut(Continuity, &RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Outcome> {
    walked.reverse();
    let (tentative_scope, tentative) = persistence::work::ingestion::tentative();
    let mut listed = None;
    let mut admitted = true;

    let loaded = payload::load(
        input,
        walked.first(),
        &walked[1..],
        || {
            drop(tentative_scope);
            let (found, discovered) = load_payload_join(finder);
            let agrees = load_payload_agrees(window, &found, &discovered, &walked);
            if agrees {
                tentative.keep();
            } else {
                tentative.discard();
            }
            listed = Some((found, discovered));
            agrees
        },
        |run, second, apps| {
            if !load_payload_admitted(&apps) {
                admitted = false;
            }
            take(Continuity::Extend, run, second, apps);
        },
    )?;

    let Some((found, discovered)) = listed else {
        unreachable!("a load that returns heard whether the listing agrees");
    };
    let Some(outcome) = loaded else {
        return load_payload_found(input, window, found, discovered, take);
    };
    let Ok(checked) = discovered else {
        unreachable!("a listing that agrees with the walk read the catalog");
    };

    load_payload_certified(input, checked, Continuity::Extend, admitted, outcome)
}

/// What listing the loaded hour, as a load in `window` lists it, found.
fn load_payload_listed(root: &Path, window: &LoadWindow<'_>) -> Listed {
    let mut found = Vec::with_capacity(MINUTES_MAX);
    let discovered = catalog::find(root, &mut found, window);

    (found, discovered)
}

/// What the listing of `finder` found, or its panic, which goes on in this thread.
fn load_payload_join(finder: thread::ScopedJoinHandle<'_, Listed>) -> Listed {
    match finder.join() {
        Ok(listed) => listed,
        Err(panic) => panic::resume_unwind(panic),
    }
}

/// Lists the loaded hour on a thread of `scope`, as a load in `window` lists it.
fn load_payload_finder<'scope>(
    scope: &'scope thread::Scope<'scope, '_>,
    root: &'scope Path,
    window: &'scope LoadWindow<'_>,
) -> io::Result<thread::ScopedJoinHandle<'scope, Listed>> {
    let observation = persistence::work::ingestion::inherit_current();
    thread::Builder::new()
        .name(String::from("flightdeck-finder"))
        .spawn_scoped(scope, move || {
            let observation = observation.enter();
            let listed = load_payload_listed(root, window);
            drop(observation);
            listed
        })
}

/// Whether the listing, which found `found` newest first and `discovered` of the catalog, found
/// what a walk found as `walked`, oldest first.
fn load_payload_agrees(
    window: &LoadWindow<'_>,
    found: &[Found],
    discovered: &Result<CheckedCatalog, Failure>,
    walked: &[Found],
) -> bool {
    discovered.is_ok()
        && load_continuity(window, found) == Continuity::Extend
        && found.iter().rev().eq(walked)
}

/// Loads `found`, the minutes the listing of the loaded hour found newest first, which
/// `discovered` says it found or why it could not.
fn load_payload_found(
    input: &payload::LoadInput<'_>,
    window: &LoadWindow<'_>,
    mut found: Vec<Found>,
    discovered: Result<CheckedCatalog, Failure>,
    mut take: impl FnMut(Continuity, &RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Outcome> {
    input.operation.check()?;
    let checked = match discovered {
        Ok(checked) => checked,
        Err(failure) => {
            return Ok(Outcome {
                left_out: Some(failure),
                terminal: None,
                checked_catalog: None,
                continued_catalog: None,
                decoded_catalog: DecodedCatalog::default(),
            });
        }
    };
    let continuity = load_continuity(window, &found);
    found.reverse();
    let (predecessor, minutes) = load_payload_minutes(continuity, &found);
    let mut admitted = true;
    let loaded = payload::load(
        input,
        predecessor,
        minutes,
        || true,
        |run, second, apps| {
            if !load_payload_admitted(&apps) {
                admitted = false;
            }
            take(continuity, run, second, apps);
        },
    )?;
    let Some(outcome) = loaded else {
        unreachable!("a load that needs no agreement hands its seconds over");
    };

    load_payload_certified(input, checked, continuity, admitted, outcome)
}

/// `outcome` with the catalog `checked` the load found, where its minutes read well enough and
/// the store `admitted` every second.
fn load_payload_certified(
    input: &payload::LoadInput<'_>,
    checked: CheckedCatalog,
    continuity: Continuity,
    admitted: bool,
    mut outcome: Outcome,
) -> persistence::Result<Outcome> {
    input.operation.check()?;
    // The catalog still says what the recording holds beside a minute that read badly, and a query
    // for one of that minute's seconds goes to the saved minutes, which say why it cannot be read.
    if matches!(outcome.left_out, None | Some(Failure::Refused)) {
        if admitted {
            load_payload_certify(input.root, checked, continuity, &mut outcome);
        }
    }
    input.operation.check()?;

    Ok(outcome)
}

/// The minute a load seeds its rates from, where it continues the store's seconds, and the
/// minutes it hands over, of `found`, the minutes it found, oldest first.
fn load_payload_minutes(continuity: Continuity, found: &[Found]) -> (Option<&Found>, &[Found]) {
    match continuity {
        Continuity::Extend => {
            let Some((predecessor, newer)) = found.split_first() else {
                unreachable!("a continued load found the minute it continues from");
            };
            (Some(predecessor), newer)
        }
        Continuity::Replace => (None, found),
    }
}

/// Hands `outcome` the catalog `checked`, which the load found, where the recording is still the
/// one it was found in, and the decoded clocks where the load decoded every minute it counts.
fn load_payload_certify(
    root: &Path,
    mut checked: CheckedCatalog,
    continuity: Continuity,
    outcome: &mut Outcome,
) {
    if let Err(failure) = catalog::revalidate(root, &checked) {
        outcome.left_out = Some(failure);
        return;
    }

    if checked.decoded_is_whole && outcome.decoded_catalog.windows == checked.windows {
        checked.first_known_second = outcome.decoded_catalog.first_known_second;
        checked.last_known_second = outcome.decoded_catalog.last_known_second;
        checked.unknown_clock_ticks = Some(outcome.decoded_catalog.unknown_clock_ticks);
    }
    match continuity {
        Continuity::Replace => outcome.checked_catalog = Some(checked),
        Continuity::Extend => outcome.continued_catalog = Some(checked),
    }
}

/// Whether a load continues from `window.since`, which it does where the minutes it found, newest
/// first, reach back to the newest one holding `since`'s run no further than `since`.
fn load_continuity(window: &LoadWindow<'_>, found: &[Found]) -> Continuity {
    if window.unread {
        if let Some(since) = window.since {
            if find_predecessor(found, since).is_some() {
                return Continuity::Extend;
            }
        }
    }

    Continuity::Replace
}

fn load_payload_admitted(apps: &TickApps<'_>) -> bool {
    match apps {
        TickApps::Missing(Missing::Budget) => false,
        TickApps::Loaded(apps) => {
            if matches!(&apps.missing, Some(Missing::Budget)) {
                return false;
            }

            !matches!(&apps.processes, Err(Missing::Budget))
        }
        TickApps::Missing(_) | TickApps::Refused(_) | TickApps::Folded(_) => true,
    }
}

/// The sealed minutes the store reads at launch at most. The hour and its lead-in span 61
/// minutes, and each stop or recovery of a collector seals a shorter one.
pub(super) const MINUTES_MAX: usize = 128;

struct LoadWindow<'a> {
    since: Option<&'a super::Newest>,
    seconds: u64,
    unread: bool,
}

impl<'a> LoadWindow<'a> {
    fn all(since: Option<&'a super::Newest>) -> Self {
        Self {
            since,
            seconds: LOADED_SECONDS,
            unread: true,
        }
    }
}

/// A sealed minute of the loaded hour, by its run, its marker and the newest wall time of its
/// ticks.
#[derive(PartialEq)]
struct Found {
    run: RunIdentity,
    marker: PathBuf,
    wall_ns_max: Option<u64>,
    publication_end: u64,
    previous_source: Option<BoundaryKey>,
}

#[derive(PartialEq)]
struct BoundaryKey {
    run: RunIdentity,
    marker: PathBuf,
    publication_end: u64,
}

/// The loaded hour's minutes, oldest first, whose Apps the store's stretches lack.
pub(super) struct Owed {
    found: Vec<Found>,
}

impl Owed {
    /// The Apps of every minute of `found`, oldest first, which a load that took the minutes' ticks
    /// in without them owes the store's stretches.
    fn all(found: Vec<Found>) -> Self {
        Self { found }
    }
}

/// Hands `take` the run, the place and the Apps of each tick of the minutes `owed` holds, oldest
/// first, with what a loader adds up of their Apps over each stretch. It reduces none of their
/// machine readings, which the store took in already. Returns why it handed over fewer than the
/// minutes sealed, where it did.
pub(super) fn load_owed(
    root: &Path,
    owed: &Owed,
    buffers: &mut LoadBuffers,
    mut take: impl FnMut(&RunIdentity, &OwedTick, TickApps<'_>),
) -> Option<Failure> {
    let mut left_out = None;
    for mut batch in load_batches(root, &owed.found, buffers) {
        for loaded in &mut batch.seconds {
            take(&loaded.run, &loaded.second, loaded.apps.tick());
        }
        if let Some(failure) = batch.left_out {
            left_out = Some(failure);
        }
    }

    left_out
}

/// The Apps second a load of a sealed minute makes of `sample`, the tick after the one whose Apps
/// second is `previous`, under `budget`.
#[cfg(test)]
pub(super) fn sample_second(
    sample: &crate::live_app_response::Sample,
    previous: Option<&AppsSecond>,
    budget: &Arc<super::ByteBudget>,
) -> AppsSecond {
    let mut second = AppsSecond::default();
    let mut names = None;
    payload::load_minute_rows(&Ok(sample), previous, budget, &mut second);
    payload::load_minute_payload(
        &Ok(sample),
        budget,
        sample.processes().len(),
        &mut names,
        &mut second,
    );
    second.observation = Some(sample.selected);
    second
}

/// The threads that decode the loaded hour's sealed minutes at once.
const LOADERS_MAX: usize = 6;

/// One retained App decoder buffer per concurrent catch-up batch, owned by the store thread.
#[derive(Default)]
pub(super) struct LoadBuffers {
    apps: [AppMinute; LOADERS_MAX],
}

/// The ticks a sealed minute holds where none was missed.
const TICKS_PER_MINUTE: usize = 60;

/// The seconds one loader decoded from its batch of minutes, oldest first, and why it holds
/// fewer than they sealed, where it does.
struct Batch<S = MachineSecond> {
    seconds: Vec<Loaded<S>>,
    left_out: Option<Failure>,
}

/// A second a loader decoded, with its run, and what it holds of its Apps.
struct Loaded<S = MachineSecond> {
    run: RunIdentity,
    second: S,
    apps: LoadedApps,
}

/// A tick of a minute whose Apps the store owes its stretches, by its publication and its wall
/// clock, which place it, since the store took its second in already.
pub(super) struct OwedTick {
    pub(super) publication: u64,
    pub(super) wall_ns: u64,
}

impl OwedTick {
    /// What the fold hands over of `reading`, or `None` where the tick has no wall clock, since a
    /// second without one has no place on a chart.
    fn of(reading: &Reading<'_>) -> Option<Self> {
        Some(Self {
            publication: reading.tick.publication,
            wall_ns: reading.tick.wall_ns?,
        })
    }
}

/// What a loader holds of a tick's Apps.
enum LoadedApps {
    /// The tick's Apps.
    Second(AppsSecond),
    /// Why the loader holds none of them at the tick. It holds those of a tick it adds up into
    /// its stretch at the oldest second of the stretch it adds up, and says here they are unread.
    Missing(Missing),
    /// What the loader added up of the Apps of the tick's stretch, from the tick and the seconds
    /// of the stretch and run it added up after it.
    Folded(Folded),
}

impl LoadedApps {
    /// What the store takes in of the tick's Apps.
    fn tick(&mut self) -> TickApps<'_> {
        match self {
            Self::Second(apps) => TickApps::Loaded(apps),
            Self::Missing(missing) => TickApps::Missing(missing),
            Self::Folded(folded) => TickApps::Folded(folded),
        }
    }

    /// The tick's Apps, where the loader holds them at the tick.
    #[cfg(test)]
    fn second(&self) -> Option<&AppsSecond> {
        match self {
            Self::Second(apps) => Some(apps),
            Self::Missing(_) | Self::Folded(_) => None,
        }
    }
}

impl From<Result<AppsSecond, Missing>> for LoadedApps {
    fn from(apps: Result<AppsSecond, Missing>) -> Self {
        match apps {
            Ok(apps) => Self::Second(apps),
            Err(missing) => Self::Missing(missing),
        }
    }
}

/// Decodes the minutes of `found`, oldest first, in batches of neighbouring minutes, each
/// batch on a loader of its own, and adds up their Apps over each stretch. Returns the batches
/// in the order of `found`.
fn load_batches(root: &Path, found: &[Found], buffers: &mut LoadBuffers) -> Vec<Batch<OwedTick>> {
    let costs = vec![1; found.len()];
    thread::scope(|scope| {
        let mut loaders = Vec::with_capacity(LOADERS_MAX);
        let mut buffers = buffers.apps.iter_mut();
        for batch in load_batches_split(&costs) {
            let buffer = buffers
                .next()
                .unwrap_or_else(|| unreachable!("one buffer per bounded batch"));
            let minutes = &found[batch];
            let observation = persistence::work::ingestion::inherit_current();
            let loader = thread::Builder::new()
                .name(String::from("flightdeck-loader"))
                .spawn_scoped(scope, move || {
                    let observation = observation.enter();
                    let result = load_batch(root, minutes, &mut Fold::new(), buffer);
                    drop(observation);
                    result
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

/// Splits minutes that cost `costs` to decode, oldest first, into at most `LOADERS_MAX` batches
/// of neighbouring minutes, each closed once it reaches an even share of the cost, so no batch
/// costs a minute's cost or more past its share.
pub(super) fn load_batches_split(costs: &[usize]) -> Vec<Range<usize>> {
    let share = costs.iter().sum::<usize>().div_ceil(LOADERS_MAX);
    let mut batches = Vec::with_capacity(LOADERS_MAX);
    let mut start = 0;
    let mut cost = 0;
    for (index, minute_cost) in costs.iter().enumerate() {
        cost += minute_cost;
        if cost >= share {
            batches.push(start..index + 1);
            start = index + 1;
            cost = 0;
        }
    }
    if start < costs.len() {
        batches.push(start..costs.len());
    }

    assert!(batches.len() <= LOADERS_MAX);
    batches
}

/// Decodes `minutes`, oldest first, with their Apps, which `fold` adds up over each stretch.
fn load_batch(
    root: &Path,
    minutes: &[Found],
    fold: &mut Fold,
    apps: &mut AppMinute,
) -> Batch<OwedTick> {
    let mut batch = Batch {
        seconds: Vec::with_capacity(minutes.len() * TICKS_PER_MINUTE),
        left_out: None,
    };
    let mut machine = MachineMinute::default();
    for minute in minutes {
        let Ok(missing) = decode(root, &minute.marker, &mut machine, apps) else {
            batch.left_out = Some(Failure::Refused);
            continue;
        };

        let decoded = Decoded {
            run: &minute.run,
            machine: &machine,
            apps: missing.map_or(Ok(&*apps), Err),
        };
        load_minute(&decoded, &mut batch.seconds, fold);
    }
    fold.finish(&mut batch.seconds);

    apps.clear();
    batch
}

/// What a loader adds up of the Apps of the minutes it decodes.
struct Fold {
    stretches: Folding,
    /// The Apps of the second being added up and those of the second added up before it, whose
    /// names it shares.
    current: AppsSecond,
    previous: AppsSecond,
    /// The seconds of the stretch it is adding up, where it added up any.
    part: Option<Part>,
}

/// The seconds of one stretch of one run that a loader adds up.
struct Part {
    /// Where the oldest of them stands among the batch's seconds.
    first_index: usize,
    run: RunIdentity,
}

impl Fold {
    fn new() -> Self {
        Self {
            stretches: Folding::new(),
            current: AppsSecond::with_room(),
            previous: AppsSecond::with_room(),
            part: None,
        }
    }

    /// Hands the stretch it is adding up to the oldest second of `seconds` it added up into it,
    /// where it added any.
    fn finish(&mut self, seconds: &mut [Loaded<OwedTick>]) {
        if let Some(folded) = self.stretches.finish() {
            self.hand_over(folded, seconds);
        }
    }

    /// Hands `folded`, the stretch it added up, to the oldest second of `seconds` it added up
    /// into it, which holds none of its Apps until then. A load that stops after that second
    /// leaves the store holding the Apps of every second of the stretch it added up.
    fn hand_over(&mut self, folded: AppsStretch, seconds: &mut [Loaded<OwedTick>]) {
        let Some(part) = self.part.take() else {
            unreachable!("a stretch adds up at least one second");
        };
        let apps = &mut seconds[part.first_index].apps;
        assert!(matches!(apps, LoadedApps::Missing(Missing::Unread)));

        *apps = LoadedApps::Folded(Folded { apps: folded });
    }
}

/// What a loader decoded of one sealed minute of run `run`.
struct Decoded<'minute> {
    run: &'minute RunIdentity,
    machine: &'minute MachineMinute,
    /// The Apps the minute keeps, or why the loader holds none of them.
    apps: Result<&'minute AppMinute, Missing>,
}

/// Pushes the run, the place and the Apps of each tick of `minute` onto `seconds`, as `fold`
/// adds them up.
fn load_minute(minute: &Decoded<'_>, seconds: &mut Vec<Loaded<OwedTick>>, fold: &mut Fold) {
    let mut kept = KeptWalk::default();
    for reading in readings(minute.machine) {
        if let Some(second) = OwedTick::of(&reading) {
            let apps = load_minute_fold(minute, &second, &mut kept, fold, seconds);
            seconds.push(Loaded {
                run: *minute.run,
                second,
                apps,
            });
        }
    }
}

/// Adds the Apps `minute` keeps of `second` to the stretch `fold` adds up, where it keeps them,
/// and hands the stretch it added up before to the
/// oldest of `seconds` it added up into it, where `second` belongs to another stretch or run.
/// `kept` is where the walk through the minute's Apps stands. Returns what the loader holds of
/// the second's Apps at the second, which `fold` makes the stretch it hands over at the oldest
/// second it adds up.
fn load_minute_fold(
    minute: &Decoded<'_>,
    second: &OwedTick,
    kept: &mut KeptWalk,
    fold: &mut Fold,
    seconds: &mut [Loaded<OwedTick>],
) -> LoadedApps {
    let apps = match &minute.apps {
        Ok(apps) => apps,
        Err(missing) => return LoadedApps::Missing(missing.clone()),
    };
    let Some((sample, rows)) = kept.find(apps, second.publication) else {
        return LoadedApps::Missing(Missing::Refused(String::from(
            "the sealed minute keeps no sample of this second",
        )));
    };
    let kept = Kept {
        run: minute.run,
        sample,
        rows,
        minute: apps,
    };
    fold.current.keep(&kept, Some(&fold.previous));
    if let Some(missing) = &fold.current.missing {
        return LoadedApps::Missing(missing.clone());
    }

    // A stretch the loader hands over holds one run's seconds, whose publications say how far
    // the store holds their Apps.
    if fold
        .part
        .as_ref()
        .is_some_and(|part| part.run != *minute.run)
    {
        fold.finish(seconds);
    }
    if let Some(folded) = fold
        .stretches
        .add(super::stretch_of_wall_ns(second.wall_ns), &fold.current)
    {
        fold.hand_over(folded, seconds);
    }
    mem::swap(&mut fold.current, &mut fold.previous);
    if fold.part.is_none() {
        fold.part = Some(Part {
            first_index: seconds.len(),
            run: *minute.run,
        });
    }
    LoadedApps::Missing(Missing::Unread)
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
/// `machine` and its Apps into `apps`. Returns why `apps` holds none of the
/// minute's Apps, where it holds none.
fn decode(
    root: &Path,
    marker: &Path,
    machine: &mut MachineMinute,
    apps: &mut AppMinute,
) -> Result<Option<Missing>, Refused> {
    let sealed = SealedMinute::open(root, marker)?;
    read_machine(&sealed, machine)?;

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
        Refused::Input(error) => format!("the sealed minute's input was refused: {error:?}"),
    }
}

/// The catalog of the recording at `root`. A root no writer finished setting up is a recording not
/// there yet, as one with no identity is, however far its setup got.
pub(super) fn find_window_catalog(root: &Path) -> Result<Catalog, Failure> {
    let budget = Arc::new(persistence::resources::ByteBudget::new(CATALOG_BYTES_MAX));
    let opened = catalog_retry(|| Catalog::open(root, &budget).map_err(persistence::Error::from));
    opened.map_err(|error| match error {
        persistence::Error::Io(error) if error.kind() == io::ErrorKind::NotFound => {
            Failure::Recording(io::ErrorKind::NotFound)
        }
        persistence::Error::Io(error) => Failure::Commits(error.kind()),
        _ => Failure::Commits(io::ErrorKind::InvalidData),
    })
}

/// Finds the sealed minutes of the loaded hour into `found`, newest first, as a load since `since`
/// finds them.
#[cfg(test)]
fn find(root: &Path, found: &mut Vec<Found>, since: Option<&super::Newest>) -> Result<(), Failure> {
    catalog::find(root, found, &LoadWindow::all(since)).map(|_| ())
}

/// The index in `found`, newest first, of the newest minute the store holds whole, which seeds
/// the rates of the new minutes before it, where `found` holds one.
fn find_predecessor(found: &[Found], since: &super::Newest) -> Option<usize> {
    assert!(found.len() <= MINUTES_MAX);
    found.iter().position(|minute| {
        if minute.run != since.run {
            return false;
        }
        minute.publication_end.saturating_sub(1) <= since.publication
    })
}

/// The working memory of a load's catalog lookups, each of which reserves its cursor's.
pub(super) const CATALOG_BYTES_MAX: usize = 1024 * 1024;
