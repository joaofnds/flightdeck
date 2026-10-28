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

use collector::recording::{Registry, RegistryBuffer, read_identity};
use collector::ring::{Identity, RunIdentity};
use persistence::layout::{self, Kind, WINDOWS_PER_DAY, WindowKey};

use super::apps::{Kept, TickApps};
use super::stretch::{Folded, Folding};
use super::{AppsSecond, AppsStretch, Failure, MachineSecond, Missing, Reading, readings};
use crate::recording::{
    AppMinute, AppParts, AppRow, MachineMinute, Refused, SampleRow, SealedMinute, read_apps,
    read_machine,
};
use crate::session::{LEAD_IN_SECONDS, LOADED_SECONDS};

/// Accepted sealed observations, their incomplete-read status and their independent predecessors.
pub(super) struct Outcome {
    pub left_out: Option<Failure>,
    pub terminal: Option<Terminal>,
    pub checked_catalog: Option<CheckedCatalog>,
    pub decoded_catalog: DecodedCatalog,
}

/// Source discovery checked all catalog windows at or beyond this actual pruning floor.
/// No floor means all discovered windows were decoded, including an empty catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CheckedCatalog {
    pub recording: Identity,
    pub newest_run: Option<RunIdentity>,
    pub first_ns: Option<u64>,
    pub windows: u32,
    pub first_known_second: Option<u64>,
    pub last_known_second: Option<u64>,
    pub unknown_clock_ticks: Option<u64>,
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
    fn from_minute(
        run: RunIdentity,
        mut machine: MachineMinute,
        process: Option<crate::recording::Before>,
    ) -> Option<Self> {
        let publication = machine.ticks.last()?.publication;
        machine.ticks.retain(|row| row.publication == publication);
        machine.cpus.retain(|row| row.publication == publication);
        machine.memory.retain(|row| row.publication == publication);
        machine
            .interfaces
            .retain(|row| row.publication == publication);
        machine
            .gpu_states
            .retain(|row| row.publication == publication);
        machine
            .core_states
            .retain(|row| row.publication == publication);
        machine.fans.retain(|row| row.publication == publication);
        assert_eq!(machine.ticks.len(), 1);

        Some(Self {
            run,
            machine,
            process,
            machine_reservation: None,
        })
    }
}

/// Decodes and consumes the opt-in payload prefix one minute at a time under caller cancellation.
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

pub(super) fn load_payload_using(
    root: &Path,
    budget: &Arc<super::ByteBudget>,
    operation: &crate::cancellation::Operation,
    capture: &crate::recording::InputCapture<'_>,
    take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Outcome> {
    load_payload_window_using(
        root,
        budget,
        operation,
        capture,
        &LoadWindow::all(None),
        take,
    )
}

#[cfg(test)]
pub(super) fn load_payload_since(
    root: &Path,
    budget: &Arc<super::ByteBudget>,
    operation: &crate::cancellation::Operation,
    since: Option<&super::Newest>,
    take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
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
    take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
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
        take,
    )
}

fn load_payload_window_using(
    root: &Path,
    budget: &Arc<super::ByteBudget>,
    operation: &crate::cancellation::Operation,
    capture: &crate::recording::InputCapture<'_>,
    window: &LoadWindow<'_>,
    mut take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Outcome> {
    operation.check()?;
    let mut found = Vec::with_capacity(MINUTES_MAX);
    let discovered = catalog::find(root, &mut found, window, operation);
    operation.check()?;
    let checked = match discovered {
        Ok(checked) => checked,
        Err(failure) => {
            return Ok(Outcome {
                left_out: Some(failure),
                terminal: None,
                checked_catalog: None,
                decoded_catalog: DecodedCatalog::default(),
            });
        }
    };
    found.reverse();
    let mut admitted = true;
    if window.unread {
        if window.since.is_some() {
            admitted = false;
        }
    }
    let mut outcome = payload::load(
        root,
        &found,
        budget,
        operation,
        capture,
        |run, second, apps| {
            if !load_payload_admitted(&apps) {
                admitted = false;
            }
            take(run, second, apps);
        },
    )?;
    operation.check()?;
    if outcome.left_out.is_none() {
        if admitted {
            match catalog::revalidate(root, &checked) {
                Ok(()) => {
                    let mut checked = checked;
                    if outcome.decoded_catalog.windows == checked.windows {
                        checked.first_known_second = outcome.decoded_catalog.first_known_second;
                        checked.last_known_second = outcome.decoded_catalog.last_known_second;
                        checked.unknown_clock_ticks =
                            Some(outcome.decoded_catalog.unknown_clock_ticks);
                    }
                    outcome.checked_catalog = Some(checked);
                }
                Err(failure) => outcome.left_out = Some(failure),
            }
        }
    }
    operation.check()?;

    Ok(outcome)
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
        TickApps::Missing(_)
        | TickApps::Derived(_)
        | TickApps::Refused(_)
        | TickApps::Folded(_) => true,
    }
}

/// The sealed minutes the store reads at launch at most. The hour and its lead-in span 61
/// minutes, and each stop or recovery of a collector seals a shorter one.
pub(super) const MINUTES_MAX: usize = 128;

/// How far before the newest sealed second the store reads Apps: the live span's 18 minutes.
const APPS_NS: u64 = 1_080 * 1_000_000_000;

const _: () = assert!(super::SECONDS_MAX == 1_080);

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

/// Reconstructs only the newest saved minute's predecessor for a freshly opened live ring.
pub(super) fn terminal(
    root: &Path,
    operation: &crate::cancellation::Operation,
) -> persistence::Result<Option<Terminal>> {
    const BYTES_MAX: usize = 256 * 1024 * 1024;
    operation.check()?;
    let mut found = Vec::with_capacity(MINUTES_MAX);
    find_window(
        root,
        &mut found,
        &LoadWindow {
            since: None,
            seconds: 1,
            unread: false,
        },
    )
    .map_err(|failure| persistence::Error::Io(io::Error::other(format!("{failure:?}"))))?;
    operation.check()?;
    let Some(found) = found.first() else {
        return Ok(None);
    };
    let budget = Arc::new(super::ByteBudget::new(BYTES_MAX));
    let terminal = payload::terminal(
        root,
        found,
        &budget,
        operation,
        &persistence::engine::AdmittedTable::from_file_budgeted,
    );
    operation.check()?;
    match terminal {
        Ok(terminal) => Ok(terminal),
        Err(failure) => {
            eprintln!("flightdeck store rate predecessor: {failure:?}");
            Ok(None)
        }
    }
}

/// A sealed minute of the loaded hour, by its run, its marker and the newest wall time of its
/// ticks.
struct Found {
    run: RunIdentity,
    marker: PathBuf,
    wall_ns_max: Option<u64>,
    publication_end: u64,
    previous_source: Option<BoundaryKey>,
}

struct BoundaryKey {
    run: RunIdentity,
    marker: PathBuf,
    publication_end: u64,
}

impl Found {
    /// Whether the store holds the minute's Apps at each of its seconds, where it holds those of
    /// minutes whose ticks reach `apps_from_ns`. It holds those of an older minute added up over
    /// each stretch.
    fn apps_by_second(&self, apps_from_ns: u64) -> bool {
        match self.wall_ns_max {
            Some(wall_ns) => wall_ns >= apps_from_ns,
            None => true,
        }
    }
}

/// Hands `take` the run, the second and the Apps of each tick the loaded hour's sealed minutes
/// hold, oldest first. The hour ends at the newest second the recording sealed, and only the
/// minutes of the live span before it give their Apps at each second. Those of older minutes
/// come added up over each stretch. Returns why the store holds less of the hour than the
/// recording sealed, where it does.
pub(super) fn load(
    root: &Path,
    mut take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> Option<Failure> {
    load_window_inner(
        root,
        &LoadWindow::all(None),
        None,
        |_, run, second, apps| {
            take(run, second, apps);
        },
    )
}

/// Whether the ticks a load hands over replace the seconds the store holds or continue them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Continuity {
    Replace,
    Extend,
}

/// Hands `take` the ticks of every minute the loaded hour holds, oldest first, to replace what
/// the store holds, once `since`'s run is found to be still one of the recording's runs.
pub(super) fn load_hour(
    root: &Path,
    since: Option<&super::Newest>,
    buffers: &mut LoadBuffers,
    take: impl FnMut(Continuity, &RunIdentity, &MachineSecond, TickApps<'_>),
) -> Option<Failure> {
    load_window_inner(
        root,
        &LoadWindow {
            since,
            seconds: LOADED_SECONDS,
            unread: false,
        },
        Some(buffers),
        take,
    )
}

/// Hands `take` the ticks of the minutes sealed after `since`, oldest first, after those of
/// the newest minute that holds `since`'s run no further than `since`, which seeds the rates of
/// the minute after it. Where the loaded hour holds no such minute, `take` gets the whole hour.
pub(super) fn load_since(
    root: &Path,
    since: Option<&super::Newest>,
    buffers: &mut LoadBuffers,
    take: impl FnMut(Continuity, &RunIdentity, &MachineSecond, TickApps<'_>),
) -> Option<Failure> {
    load_window_inner(root, &LoadWindow::all(since), Some(buffers), take)
}

fn load_window_inner(
    root: &Path,
    window: &LoadWindow<'_>,
    buffers: Option<&mut LoadBuffers>,
    mut take: impl FnMut(Continuity, &RunIdentity, &MachineSecond, TickApps<'_>),
) -> Option<Failure> {
    let mut found = Vec::with_capacity(MINUTES_MAX);
    let continuity = match find_window(root, &mut found, window) {
        Ok(continuity) => continuity,
        Err(failure) => return Some(failure),
    };

    let apps_from_ns = found
        .first()
        .and_then(|newest| newest.wall_ns_max)
        .map_or(0, |newest| newest.saturating_sub(APPS_NS));
    found.reverse();
    // A load that continues the store's seconds hands over seconds the store holds already,
    // which it takes in no second time.
    let held = match continuity {
        Continuity::Extend => window.since,
        Continuity::Replace => None,
    };

    let mut left_out = None;
    for mut batch in load_batches(root, &found, apps_from_ns, held, buffers) {
        for loaded in &mut batch.seconds {
            take(continuity, &loaded.run, &loaded.second, loaded.apps.tick());
        }
        if let Some(failure) = batch.left_out {
            left_out = Some(failure);
        }
    }

    left_out
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
struct Batch {
    seconds: Vec<Loaded>,
    left_out: Option<Failure>,
}

/// A second a loader decoded, with its run, and what it holds of its Apps.
struct Loaded {
    run: RunIdentity,
    second: MachineSecond,
    apps: LoadedApps,
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
/// batch on a loader of its own, with the Apps at each second of each minute whose ticks reach
/// `apps_from_ns`, and those of older minutes added up over each stretch but the seconds of
/// `held`, the newest second the store holds already. Returns the batches in the order of
/// `found`.
fn load_batches(
    root: &Path,
    found: &[Found],
    apps_from_ns: u64,
    held: Option<&super::Newest>,
    buffers: Option<&mut LoadBuffers>,
) -> Vec<Batch> {
    // Every minute decodes its Apps, whether by second or by stretch, at about the same cost.
    let costs = vec![1; found.len()];
    thread::scope(|scope| {
        let mut loaders = Vec::with_capacity(LOADERS_MAX);
        let mut buffers = buffers.map(|buffers| buffers.apps.iter_mut());
        for batch in load_batches_split(&costs) {
            let apps = buffers.as_mut().map(|buffers| {
                buffers
                    .next()
                    .unwrap_or_else(|| unreachable!("one buffer per bounded batch"))
            });
            let before = batch
                .start
                .checked_sub(1)
                .and_then(|before| found.get(before));
            let minutes = &found[batch];
            let observation = persistence::work::ingestion::inherit_current();
            let loader = thread::Builder::new()
                .name(String::from("flightdeck-loader"))
                .spawn_scoped(scope, move || {
                    let observation = observation.enter();
                    let result = load_batch_using(root, before, minutes, apps_from_ns, held, apps);
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

/// Startup releases its decode storage with the batch; catch-up borrows the store's buffers.
fn load_batch_using(
    root: &Path,
    before: Option<&Found>,
    minutes: &[Found],
    apps_from_ns: u64,
    held: Option<&super::Newest>,
    apps: Option<&mut AppMinute>,
) -> Batch {
    let mut fresh = AppMinute::default();
    load_batch(
        root,
        before,
        minutes,
        &mut Fold::new(apps_from_ns, held),
        apps.unwrap_or(&mut fresh),
    )
}

/// Decodes `minutes`, oldest first, with their Apps, which `fold` adds up over each stretch
/// where the minute is older than those whose Apps the store holds at each second. The first
/// minute's first second takes its rates from the last tick of the minute `before` it, which the
/// loader before this one decodes too.
fn load_batch(
    root: &Path,
    before: Option<&Found>,
    minutes: &[Found],
    fold: &mut Fold<'_>,
    apps: &mut AppMinute,
) -> Batch {
    let mut batch = Batch {
        seconds: Vec::with_capacity(minutes.len() * TICKS_PER_MINUTE),
        left_out: None,
    };
    let mut current = MachineMinute::default();
    let mut previous = MachineMinute::default();
    let mut previous_run = None;
    if let Some(before) = before {
        let _purpose = persistence::work::ingestion::purpose(
            persistence::work::ingestion::Purpose::Predecessor,
        );
        // The loader that decodes the minute before reports it where it does not decode.
        if decode(root, &before.marker, &mut previous, None).is_ok() {
            previous_run = Some(before.run);
        }
    }

    for minute in minutes {
        let Ok(missing) = decode(root, &minute.marker, &mut current, Some(&mut *apps)) else {
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
            apps: missing.map_or(Ok(&*apps), Err),
            folded: !minute.apps_by_second(fold.apps_from_ns),
        };
        load_minute(&decoded, before, &mut batch.seconds, fold);

        mem::swap(&mut current, &mut previous);
        previous_run = Some(minute.run);
    }
    fold.finish(&mut batch.seconds);

    apps.clear();
    batch
}

/// What a loader adds up of the Apps of the minutes older than those whose Apps the store holds
/// at each second, the minutes whose ticks do not reach `apps_from_ns`.
struct Fold<'since> {
    apps_from_ns: u64,
    /// The newest second the store holds already. The loader adds up none of the seconds whose
    /// Apps the store holds.
    held: Option<&'since super::Newest>,
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
    first: usize,
    run: RunIdentity,
    /// The publication of the newest of them.
    publication: u64,
}

impl<'since> Fold<'since> {
    fn new(apps_from_ns: u64, held: Option<&'since super::Newest>) -> Self {
        Self {
            apps_from_ns,
            held,
            stretches: Folding::new(),
            current: AppsSecond::with_room(),
            previous: AppsSecond::with_room(),
            part: None,
        }
    }

    /// Hands the stretch it is adding up to the oldest second of `seconds` it added up into it,
    /// where it added any.
    fn finish(&mut self, seconds: &mut [Loaded]) {
        if let Some(folded) = self.stretches.finish() {
            self.hand_over(folded, seconds);
        }
    }

    /// Hands `folded`, the stretch it added up, to the oldest second of `seconds` it added up
    /// into it, which holds none of its Apps until then. A load that stops after that second
    /// leaves the store holding the Apps of every second of the stretch it added up.
    fn hand_over(&mut self, folded: AppsStretch, seconds: &mut [Loaded]) {
        let Some(part) = self.part.take() else {
            unreachable!("a stretch adds up at least one second");
        };
        let apps = &mut seconds[part.first].apps;
        assert!(matches!(apps, LoadedApps::Missing(Missing::Unread)));

        *apps = LoadedApps::Folded(Folded {
            apps: folded,
            publication: part.publication,
        });
    }
}

/// What a loader decoded of one sealed minute of run `run`.
struct Decoded<'minute> {
    run: &'minute RunIdentity,
    machine: &'minute MachineMinute,
    /// The Apps the minute keeps, or why the loader holds none of them.
    apps: Result<&'minute AppMinute, Missing>,
    /// Whether the loader adds up the minute's Apps over each stretch rather than holding them
    /// at each second.
    folded: bool,
}

/// Pushes the run, the second and the Apps of each tick of `minute` onto `seconds`, its rates
/// over the interval since the reading before it, `before` for the minute's first. `fold` adds
/// up the Apps of a minute the store holds by stretch.
fn load_minute(
    minute: &Decoded<'_>,
    before: Option<Reading<'_>>,
    seconds: &mut Vec<Loaded>,
    fold: &mut Fold<'_>,
) {
    let mut previous = before;
    let mut kept = KeptWalk::default();
    for reading in readings(minute.machine) {
        persistence::work::ingestion::record_publication(
            *minute.run,
            reading.tick.publication,
            persistence::work::ingestion::Kind::MachineReduction,
        );
        if let Some(second) = MachineSecond::of(&reading, previous.as_ref()) {
            let apps = if minute.folded {
                load_minute_fold(minute, &second, &mut kept, fold, seconds)
            } else {
                let held = seconds.last().and_then(|loaded| loaded.apps.second());
                load_minute_apps(minute, second.publication, &mut kept, held).into()
            };
            seconds.push(Loaded {
                run: *minute.run,
                second,
                apps,
            });
        }
        previous = Some(reading);
    }
}

/// Adds the Apps `minute` keeps of `second` to the stretch `fold` adds up, where it keeps them
/// and the store does not hold them already, and hands the stretch it added up before to the
/// oldest of `seconds` it added up into it, where `second` belongs to another stretch or run.
/// `kept` is where the walk through the minute's Apps stands. Returns what the loader holds of
/// the second's Apps at the second, which `fold` makes the stretch it hands over at the oldest
/// second it adds up.
fn load_minute_fold(
    minute: &Decoded<'_>,
    second: &MachineSecond,
    kept: &mut KeptWalk,
    fold: &mut Fold<'_>,
    seconds: &mut [Loaded],
) -> LoadedApps {
    if fold
        .held
        .is_some_and(|held| held.holds_apps(minute.run, second))
    {
        return LoadedApps::Missing(Missing::Unread);
    }
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
    if let Some(folded) = fold.stretches.add(super::stretch(second), &fold.current) {
        fold.hand_over(folded, seconds);
    }
    mem::swap(&mut fold.current, &mut fold.previous);
    match &mut fold.part {
        Some(part) => part.publication = second.publication,
        None => {
            fold.part = Some(Part {
                first: seconds.len(),
                run: *minute.run,
                publication: second.publication,
            });
        }
    }
    LoadedApps::Missing(Missing::Unread)
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

    let mut second = AppsSecond::with_room_for(rows.len());
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
        Refused::Input(error) => format!("the sealed minute's input was refused: {error:?}"),
    }
}

/// Finds the sealed minutes of the loaded hour into `found`, newest first, from the newest run
/// the recording registers back.
#[cfg(test)]
fn find(root: &Path, found: &mut Vec<Found>, since: Option<&super::Newest>) -> Result<(), Failure> {
    find_window(root, found, &LoadWindow::all(since)).map(|_| ())
}

fn find_window(
    root: &Path,
    found: &mut Vec<Found>,
    window: &LoadWindow<'_>,
) -> Result<Continuity, Failure> {
    assert!(window.seconds > 0);
    assert!(window.seconds <= LOADED_SECONDS);
    let recording = read_identity(root).map_err(|error| Failure::Recording(error.kind()))?;
    let mut buffer: RegistryBuffer = [0; _];
    let registry = Registry::read_into(&root.join("runs"), &mut buffer)
        .map_err(|error| Failure::Registry(error.kind()))?;
    let mut runs = Vec::new();
    for run in registry.runs() {
        let run = run.map_err(|error| Failure::Registry(error.kind()))?;
        runs.push(run.identity);
    }

    if let Some(since) = window.since {
        if since.run.recording != recording {
            return Err(Failure::ForeignRecording);
        }
        if !runs.contains(&since.run) {
            return Err(Failure::ChangedIdentity);
        }
    }
    // Every run is searched, since a load that finds no held minute to continue from replaces
    // the hour, and the hour reaches back into the runs before `since`'s.
    let mut oldest_ns = None;
    for run in runs.iter().rev() {
        let finding = FindRun {
            root,
            recording,
            run,
            span_ns: (window.seconds + LEAD_IN_SECONDS) * 1_000_000_000,
        };
        let more = finding
            .find(&mut oldest_ns, found)
            .map_err(|error| Failure::Commits(error.kind()))?;
        if !more {
            break;
        }
    }
    if window.unread {
        if let Some(since) = window.since {
            if let Some(predecessor) = find_predecessor(found, since) {
                found.truncate(predecessor + 1);
                return Ok(Continuity::Extend);
            }
        }
    }

    Ok(Continuity::Replace)
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

/// The search for the sealed minutes of one run of the recording.
struct FindRun<'a> {
    root: &'a Path,
    recording: Identity,
    run: &'a RunIdentity,
    span_ns: u64,
}

impl FindRun<'_> {
    /// Finds the run's sealed minutes newer than `oldest_ns` into `found`, newest first, where
    /// the first minute found sets `oldest_ns`. Returns false at the old boundary and fails
    /// if another required minute exceeds `found`'s capacity.
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
                let oldest = *oldest_ns.get_or_insert_with(|| {
                    manifest
                        .wall_ns
                        .map_or(0, |wall| wall.max.saturating_sub(self.span_ns))
                });
                if let Some(wall_ns) = manifest.wall_ns {
                    if wall_ns.max < oldest {
                        return Ok(false);
                    }
                }
                if found.len() == MINUTES_MAX {
                    return Err(io::Error::from(io::ErrorKind::InvalidData));
                }
                found.push(Found {
                    run: *self.run,
                    marker,
                    wall_ns_max: manifest.wall_ns.map(|wall| wall.max),
                    publication_end: manifest.publication_end,
                    previous_source: None,
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
