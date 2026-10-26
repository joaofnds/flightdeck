//! The readings the live window draws, held for the app's life. The store thread writes them
//! into buffers allocated when the store starts, and the main thread reads them through a View
//! that never changes while it holds it.

mod apps;
mod attribution;
mod buffers;
mod context;
mod payload;
mod recording;
mod ring;
mod sampled;
mod second;

#[cfg(test)]
#[path = "store/startup_tests.rs"]
mod startup_tests;

#[cfg(test)]
mod activity_tests;

#[cfg(test)]
mod resume_tests;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub use crate::byte_budget::{ByteBudget, ByteReservation};
pub use apps::{APPS_PER_SECOND_MAX, AppRow, AppsSecond, Missing};
pub use attribution::{
    Attribution, AttributionReading, COHORT_MAX, OtherReading, Resident, ResidentOther, Totals,
};
pub use buffers::{Buffers, Mailbox, Span};
pub use context::Cohorts;
pub use payload::{AppDetails, Payload, ProcessName, ProcessRow};
pub use sampled::{SampledRank, SampledRanking, SampledRanks, SampledSecond};
pub use second::{BatterySecond, MachineSecond, MemorySecond, Reading, Traffic, readings};

use apps::TickApps;
use collector::ring::RunIdentity;
use context::Context;
use ring::Ring;

use crate::apps::Stamp;
use crate::cancellation::{Cancellation, Operation};
use crate::live::resident_history::{Interval, consecutive};
use crate::live_app_response::Observation;
use crate::session::EARLIER_STRIDE_SECONDS;

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

/// The seconds the store holds: the sixteen minutes a live chart draws a second apart, and
/// the two minutes the collector has not sealed yet.
const SECONDS_MAX: usize = 1_080;

/// The seconds past the span a View the main thread holds may keep, after which the store
/// takes no more seconds until the main thread lets go of it.
const SECONDS_HELD_MAX: usize = 64;

/// The older seconds of the loaded hour the store holds, one in every stretch of
/// `EARLIER_STRIDE_SECONDS` past the live span: the hour and its lead-in, 3,660 seconds, less the
/// live span's 1,080, is 258 stretches, and the stretch the live span cuts into is one more.
const EARLIER_MAX: usize = 259;

/// The older seconds past their span a View the main thread holds may keep. While it holds one,
/// the live span lets go of at most `SECONDS_HELD_MAX` seconds, which start at most eight
/// stretches.
const EARLIER_HELD_MAX: usize = 8;

/// The Views the store writes: the one in the mailbox, the one the main thread draws, the one
/// it takes in place of that, and one to write.
const VIEWS_MAX: usize = 4;

/// The longest sleep between steps. Native startup decoding can take longer to finish after
/// cancellation, so this is not a shutdown deadline.
const WAIT: Duration = Duration::from_millis(100);

// The store looks for a collector started again at least once a second.
const _: () = assert!(WAIT.saturating_mul(ring::FOLLOW_STEPS).as_millis() <= 1_000);

/// The steps of the store thread, which it never reaches, since it steps until the app quits.
const STEPS_MAX: u64 = u64::MAX;

/// What the store held at one step, which never changes while the main thread holds it.
pub struct View {
    /// Higher in each View than in the one before, since the store writes each View's buffer
    /// again.
    pub version: u64,
    /// The seconds of the live span, every second the store holds of the newest 18 minutes,
    /// oldest first.
    pub seconds: Vec<Arc<MachineSecond>>,
    /// Each App's totals at each second of `seconds`, in the same order.
    pub apps: Vec<Arc<AppsSecond>>,
    /// The older seconds of the loaded hour, before `seconds`, oldest first.
    pub earlier: Vec<Arc<MachineSecond>>,
    /// Older hover payloads, populated only by the opt-in complete payload store.
    pub sampled: Vec<Arc<SampledSecond>>,
    /// The Apps each of Overview's App charts draws on a line of its own, chosen over the newest
    /// seconds of `seconds` those charts draw.
    pub cohorts: Cohorts,
    /// Why the store could not read the recording at its last step.
    pub failure: Option<Failure>,
    /// Why the store holds less of the loaded hour than the recording sealed when it started.
    pub left_out: Option<Failure>,
}

impl View {
    /// Exact identities at this wall second, ordered by wall time, run and publication.
    #[must_use]
    pub fn alternatives(&self, second: u64) -> Vec<Observation> {
        let mut observations: Vec<_> = self
            .apps
            .iter()
            .filter_map(|apps| apps.observation)
            .filter(|observation| {
                observation.wall_ns.map(|wall| wall / 1_000_000_000) == Some(second)
            })
            .collect();
        observations.sort_by_key(|observation| {
            (
                observation.wall_ns,
                observation.stamp.run,
                observation.stamp.publication,
            )
        });
        observations
    }

    /// Finds the publication only when its recording, identity and every stored clock agree.
    #[must_use]
    pub fn observation(&self, observation: &Observation) -> Option<&AppsSecond> {
        self.apps
            .iter()
            .find(|apps| apps.observation.as_ref() == Some(observation))
            .map(AsRef::as_ref)
    }
}

/// Why the store could not read the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The recording's identity could not be read, as before the collector ever ran.
    Recording(io::ErrorKind),
    /// The recording's register of runs could not be read.
    Registry(io::ErrorKind),
    /// The newest run belongs to another recording.
    ForeignRecording,
    /// The newest run's ring belongs to another run than the register names.
    ChangedIdentity,
    /// The newest run's ring maps more memory than the store reads.
    MappingSize,
    /// The ring could not be opened, read or waited on.
    Ring(io::ErrorKind),
    /// The ring's shape asks for buffers the store does not size.
    TickShape,
    /// The recording's sealed minutes could not be listed.
    Commits(io::ErrorKind),
    /// A sealed minute did not show it committed or did not decode, so its seconds are missing.
    Refused,
    /// A thread to decode sealed minutes could not start, so the seconds of its minutes are
    /// missing.
    Loader(io::ErrorKind),
}

/// The store thread, and the main thread's way to its Views. Dropping the store stops its
/// thread.
pub struct Store {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    mailbox: Mailbox<View>,
    stop: AtomicBool,
    active: AtomicBool,
    catch_up: AtomicBool,
    catching_up: AtomicBool,
    paused: AtomicBool,
    window_seconds: AtomicU64,
    cancellation: Arc<Cancellation>,
}

impl Store {
    /// Starts the store thread over the recording at `root`, with every buffer it holds
    /// seconds and Views in allocated before the thread starts.
    pub fn start(root: &Path) -> io::Result<Self> {
        Self::start_inner(root, None)
    }

    /// Starts the expanded payload experiment with an explicit retained-byte budget.
    /// Production pages keep using `start` until the full footprint preflight passes.
    pub fn start_with_payload(root: &Path, bytes_max: usize) -> io::Result<Self> {
        Self::start_with_budget(root, Arc::new(ByteBudget::new(bytes_max)))
    }

    /// Shares reservations with the caller's loader inputs, decoded work and retained results.
    pub fn start_with_budget(root: &Path, budget: Arc<ByteBudget>) -> io::Result<Self> {
        Self::start_inner(root, Some(budget))
    }

    fn start_inner(root: &Path, budget: Option<Arc<ByteBudget>>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            mailbox: Mailbox::new(),
            stop: AtomicBool::new(false),
            active: AtomicBool::new(true),
            catch_up: AtomicBool::new(false),
            catching_up: AtomicBool::new(true),
            paused: AtomicBool::new(false),
            window_seconds: AtomicU64::new(crate::session::LOADED_SECONDS),
            cancellation: Arc::new(Cancellation::default()),
        });
        let mut held = Held::new();
        if let Some(budget) = budget {
            held.enable_payload(budget);
        }
        let root = root.to_owned();
        let operation = Arc::clone(&shared.cancellation)
            .operation(0)
            .map_err(io::Error::other)?;

        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(String::from("flightdeck-store"))
            .spawn(move || {
                // A ring's mapping stays on the thread that maps it.
                let mut ring = Ring::new(root.clone());
                if let Err(error) = startup(&root, &mut held, &mut ring, &operation) {
                    eprintln!("flightdeck store startup: {error}");
                    return;
                }
                let operation = held.payload_budget.as_ref().map(|_| &operation);
                run(&root, &mut held, &mut ring, &thread_shared, operation);
            })?;

        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Pauses between bounded reads while hidden, or wakes the thread to reconcile on show.
    pub fn set_active(&self, active: bool) {
        if self.shared.active.swap(active, Ordering::AcqRel) == active {
            return;
        }

        if !active {
            self.shared.catching_up.store(true, Ordering::Release);
            self.shared.catch_up.store(true, Ordering::Release);
        }
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }

    /// Selects the live period loaded on the next reconciliation or range change.
    #[must_use]
    pub fn set_window(&self, seconds: u64) -> bool {
        assert!(seconds > 0);
        assert!(seconds <= crate::session::LOADED_SECONDS);
        if self.shared.window_seconds.swap(seconds, Ordering::AcqRel) == seconds {
            return false;
        }

        self.shared.catching_up.store(true, Ordering::Release);
        self.shared.catch_up.store(true, Ordering::Release);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
        true
    }

    /// Whether the store has finished its in-flight read and parked without polling.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Acquire)
    }

    /// Whether the window currently requests live ingestion.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.shared.active.load(Ordering::Acquire)
    }

    /// Whether catch-up or a published View still awaits the window's next step.
    #[must_use]
    pub fn pending(&self) -> bool {
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return false;
        }

        if self.shared.catch_up.load(Ordering::Acquire) {
            return true;
        }

        if self.shared.catching_up.load(Ordering::Acquire) {
            return true;
        }

        self.shared.mailbox.pending()
    }

    /// The View the store handed over since the last take, or `None` when it handed over none
    /// or is handing one over now.
    #[must_use]
    pub fn take(&self) -> Option<Arc<View>> {
        if self.shared.catch_up.load(Ordering::Acquire) {
            return None;
        }
        if self.shared.catching_up.load(Ordering::Acquire) {
            return None;
        }

        self.shared.mailbox.take()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Err(error) = self.shared.cancellation.cancel() {
            eprintln!("flightdeck store cancellation: {error}");
        }
        let Some(thread) = self.thread.take() else {
            unreachable!("only dropping the store takes its thread");
        };

        thread.thread().unpark();
        if let Err(panic) = thread.join() {
            if !thread::panicking() {
                std::panic::resume_unwind(panic);
            }
        }
    }
}

/// What the store thread holds between steps.
struct Held {
    payload_budget: Option<Arc<ByteBudget>>,
    sampled: Option<Span<SampledSecond>>,
    span: Span<MachineSecond>,
    /// Each App's totals at each second of `span`, in the same order.
    apps: Span<AppsSecond>,
    earlier: Span<MachineSecond>,
    /// What each App used over the newest seconds of `apps` that Overview's App charts draw.
    context: Context,
    /// The run and publication of the newest second taken in.
    newest: Option<Newest>,
    views: Buffers<View>,
    version: u64,
    failure: Option<Failure>,
    left_out: Option<Failure>,
    /// Whether the store holds what no View it handed over shows.
    unpublished: bool,
}

#[derive(Clone, Copy)]
struct Newest {
    run: RunIdentity,
    publication: u64,
    segment: u64,
}

impl Held {
    fn new() -> Self {
        let seconds = Buffers::new(SECONDS_MAX + SECONDS_HELD_MAX, MachineSecond::default);
        let apps = Buffers::new(SECONDS_MAX + SECONDS_HELD_MAX, AppsSecond::with_room);
        let earlier = Buffers::new(EARLIER_MAX + EARLIER_HELD_MAX, MachineSecond::default);
        let views = Buffers::new(VIEWS_MAX, || View {
            version: 0,
            seconds: Vec::with_capacity(SECONDS_MAX),
            apps: Vec::with_capacity(SECONDS_MAX),
            earlier: Vec::with_capacity(EARLIER_MAX),
            sampled: Vec::new(),
            cohorts: Cohorts::with_room(),
            failure: None,
            left_out: None,
        });

        Self {
            payload_budget: None,
            sampled: None,
            span: Span::new(SECONDS_MAX, seconds),
            apps: Span::new(SECONDS_MAX, apps),
            earlier: Span::new(EARLIER_MAX, earlier),
            context: Context::new(),
            newest: None,
            views,
            version: 0,
            failure: None,
            left_out: None,
            unpublished: true,
        }
    }

    fn enable_payload(&mut self, budget: Arc<ByteBudget>) {
        assert!(self.span.is_empty());
        self.payload_budget = Some(budget);
        self.sampled = Some(Span::new(
            EARLIER_MAX,
            Buffers::new(EARLIER_MAX + EARLIER_HELD_MAX, SampledSecond::empty),
        ));
        self.views
            .release(|view| view.sampled = Vec::with_capacity(EARLIER_MAX));
    }

    fn reset(&mut self) {
        self.release_views();
        self.span.clear();
        self.apps.clear();
        self.apps
            .release(|apps| apps.take(TickApps::Missing(&Missing::Unread), None, None));
        self.earlier.clear();
        if let Some(sampled) = &mut self.sampled {
            sampled.clear();
            sampled.release(|sample| *sample = SampledSecond::empty());
        }
        self.context.clear();
        self.newest = None;
        self.unpublished = true;
    }

    /// Takes `second` of run `run` in after the newest, with what the store found of its Apps,
    /// `apps`, and keeps the second the live span lets go of where it is the first of its
    /// stretch. A second of the run no newer than the newest taken in is held already. Returns
    /// false and changes nothing where a span has no buffer to write, so the caller takes the
    /// second again at its next step.
    fn take(&mut self, run: &RunIdentity, second: &MachineSecond, apps: TickApps<'_>) -> bool {
        if let Some(newest) = &self.newest {
            if newest.run == *run {
                if second.publication <= newest.publication {
                    return true;
                }
            }
        }

        self.release_views();

        let leaving = self.span.leaving().copied();
        let kept = leaving.filter(|leaving| starts_stretch(self.earlier.newest(), leaving));
        if !self.span.has_room() {
            return false;
        }
        if !self.apps.has_room() {
            return false;
        }
        if kept.is_some() {
            if !self.earlier.has_room() {
                return false;
            }
            if self
                .sampled
                .as_ref()
                .is_some_and(|sampled| !sampled.has_room())
            {
                return false;
            }
        }

        let segment = segment(self.newest.as_ref(), run, second);
        self.context.leave(&self.apps);
        if let Some(kept) = kept {
            if let Some(sampled) = &mut self.sampled {
                let apps = self.apps.leaving().unwrap_or_else(|| unreachable!());
                let budget = self
                    .payload_budget
                    .as_ref()
                    .unwrap_or_else(|| unreachable!());
                let pushed = sampled.push(|buffer| buffer.take(&kept, apps, budget));
                assert!(pushed);
            }
            let pushed = self.earlier.push(|buffer| *buffer = kept);
            assert!(pushed);
        }
        self.take_current(run, second, apps, segment);
        self.newest = Some(Newest {
            run: *run,
            publication: second.publication,
            segment,
        });
        self.unpublished = true;
        true
    }

    fn take_current(
        &mut self,
        run: &RunIdentity,
        second: &MachineSecond,
        apps: TickApps<'_>,
        segment: u64,
    ) {
        let pushed = self.span.push(|buffer| {
            *buffer = *second;
            buffer.segment = segment;
        });
        assert!(pushed);
        let previous = self.apps.newest_held().map(Arc::clone);
        let tick = take_observation(run, second);
        let budget = self.payload_budget.as_ref();
        let pushed = self.apps.push(|buffer| {
            buffer.take(apps, previous.as_deref(), budget);
            if buffer.observation.is_none() {
                buffer.observation = Some(tick);
            }
            buffer.consecutive = take_consecutive(previous.as_deref(), buffer);
        });
        assert!(pushed);
        if let Some(observation) = self.apps.newest().and_then(|apps| apps.observation) {
            assert_eq!(observation.stamp.publication, second.publication);
        }
        self.context.follow(&self.span, &self.apps);
    }

    fn release_views(&mut self) {
        self.views.release(|view| {
            view.seconds.clear();
            view.apps.clear();
            view.earlier.clear();
            view.sampled.clear();
            view.cohorts.clear();
        });
    }

    /// Hands the main thread a View of what the store holds, or leaves it to the next step
    /// where the main thread holds every View.
    fn publish(&mut self, mailbox: &Mailbox<View>) {
        self.release_views();
        let version = self.version + 1;
        let span = &self.span;
        let apps = &self.apps;
        let earlier = &self.earlier;
        let sampled = &self.sampled;
        let context = &mut self.context;
        let failure = self.failure;
        let left_out = self.left_out;
        let written = self.views.write(|view| {
            assert!(view.seconds.is_empty());
            assert!(view.apps.is_empty());
            assert!(view.earlier.is_empty());
            assert!(view.sampled.is_empty());
            let capacity = (
                view.seconds.capacity(),
                view.apps.capacity(),
                view.earlier.capacity(),
                view.sampled.capacity(),
            );

            view.version = version;
            view.seconds.extend(span.held().map(Arc::clone));
            view.apps.extend(apps.held().map(Arc::clone));
            view.earlier.extend(earlier.held().map(Arc::clone));
            if let Some(sampled) = sampled {
                view.sampled.extend(sampled.held().map(Arc::clone));
                assert_eq!(view.sampled.len(), view.earlier.len());
            }
            context.choose(&mut view.cohorts);
            view.failure = failure;
            view.left_out = left_out;

            assert_eq!(view.apps.len(), view.seconds.len());
            assert!(view.cohorts.seconds <= view.seconds.len());
            let held = (
                view.seconds.capacity(),
                view.apps.capacity(),
                view.earlier.capacity(),
                view.sampled.capacity(),
            );
            assert_eq!(held, capacity);
        });
        let Some(view) = written else {
            return;
        };

        mailbox.put(view);
        self.version = version;
        self.unpublished = false;
    }
}

/// The tick of `second` of run `run`, as the App rule names a tick it samples.
fn take_observation(run: &RunIdentity, second: &MachineSecond) -> Observation {
    Observation {
        recording: run.recording.0,
        stamp: Stamp {
            boot: run.boot.0,
            run: run.run.0,
            publication: second.publication,
            continuous_ns: second.continuous_ns,
            awake_ns: second.awake_ns,
        },
        run_tick: second.run_tick,
        wall_ns: Some(second.wall_ns),
    }
}

/// Whether the tick of `second` is the one after the tick of `previous`, the second the store
/// took in before it, by the rule the worker's App histories break a chart's line at.
fn take_consecutive(previous: Option<&AppsSecond>, second: &AppsSecond) -> bool {
    let Some(before) = previous.and_then(|previous| previous.observation.as_ref()) else {
        return false;
    };
    let Some(after) = second.observation.as_ref() else {
        unreachable!("the store names the tick of every second it takes in");
    };

    consecutive(&Interval { before, after })
}

/// The segment of `second` of run `run`, taken in after `newest`.
fn segment(newest: Option<&Newest>, run: &RunIdentity, second: &MachineSecond) -> u64 {
    let Some(newest) = newest else {
        return 0;
    };

    if newest.run == *run {
        if second.follows {
            return newest.segment;
        }
    }
    newest.segment + 1
}

/// Whether `second` starts a stretch of `EARLIER_STRIDE_SECONDS` after the stretch of `newest`.
fn starts_stretch(newest: Option<&MachineSecond>, second: &MachineSecond) -> bool {
    let Some(newest) = newest else {
        return true;
    };

    stretch(newest) != stretch(second)
}

/// The stretch of `EARLIER_STRIDE_SECONDS` that holds `second`. Both divisions floor.
fn stretch(second: &MachineSecond) -> u64 {
    second.wall_ns / 1_000_000_000 / EARLIER_STRIDE_SECONDS
}

/// Loads the sealed prefix before following the ring, keeping legacy startup separate.
fn startup(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    operation: &Operation,
) -> persistence::Result<()> {
    let budget = held.payload_budget.clone();
    let take = |run: &RunIdentity, second: &MachineSecond, apps: TickApps<'_>| {
        // No View is out during startup, so every buffer is free.
        assert!(held.take(run, second, apps));
    };

    if let Some(budget) = &budget {
        let outcome = recording::load_payload(root, budget, operation, take);

        startup_accept(outcome, held, ring, operation)
    } else {
        held.left_out = recording::load(root, take);

        Ok(())
    }
}

fn startup_accept(
    outcome: persistence::Result<recording::Outcome>,
    held: &mut Held,
    ring: &mut Ring,
    operation: &Operation,
) -> persistence::Result<()> {
    let outcome = outcome?;
    operation.check()?;
    held.left_out = outcome.left_out;
    ring.accept(outcome.terminal);

    Ok(())
}

/// Steps until the store is dropped.
fn run(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    operation: Option<&Operation>,
) {
    let mut waited = Ok(());
    for _ in 0..STEPS_MAX {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        if !shared.active.load(Ordering::Acquire) {
            shared.paused.store(true, Ordering::Release);
            thread::park();
            shared.paused.store(false, Ordering::Release);
            continue;
        }
        if shared.catch_up.load(Ordering::Acquire) {
            shared.catching_up.store(true, Ordering::Release);
        }
        if shared.catch_up.swap(false, Ordering::AcqRel) {
            match refresh(root, held, ring, shared, operation) {
                Ok(true) => {
                    waited = Ok(());
                }
                Ok(false) => {
                    shared.catch_up.store(true, Ordering::Release);
                    thread::park_timeout(WAIT);
                    continue;
                }
                Err(error) => {
                    eprintln!("flightdeck store catch-up: {error}");
                    return;
                }
            }
        }
        let publication = if shared.catching_up.load(Ordering::Acquire) {
            Publication::Complete
        } else {
            Publication::Progress
        };
        if let Err(error) = step(held, ring, waited, &shared.mailbox, operation, publication) {
            eprintln!("flightdeck store publication: {error}");
            return;
        }
        run_ready(held, ring, shared);
        waited = if held.failure.is_some() {
            // A ring that failed may fail again at once, so the thread sleeps instead.
            thread::sleep(WAIT);
            Ok(())
        } else {
            ring.wait(WAIT)
        };
    }
    unreachable!("the store thread steps until the store is dropped");
}

fn run_ready(held: &mut Held, ring: &Ring, shared: &Shared) {
    if !shared.catching_up.load(Ordering::Acquire) {
        return;
    }

    let caught_up = match ring.caught_up() {
        Ok(caught_up) => caught_up,
        Err(failure) => {
            held.failure = Some(failure);
            held.unpublished = true;
            held.publish(&shared.mailbox);
            true
        }
    };
    if !held.unpublished {
        if caught_up {
            shared.catching_up.store(false, Ordering::Release);
        } else {
            if held.failure.is_some() {
                shared.catching_up.store(false, Ordering::Release);
            }
        }
    }
}

/// Replaces the cached period before publishing the current ring's endpoint.
fn refresh(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    operation: Option<&Operation>,
) -> persistence::Result<bool> {
    drop(shared.mailbox.take());
    held.unpublished = true;
    let budget = held.payload_budget.clone();
    let since = held.newest;
    let seconds = shared.window_seconds.load(Ordering::Acquire);
    let mut accepted = true;
    let mut reset = false;
    let mut next_ring = Ring::new(root.to_owned());
    let mut take = |run: &RunIdentity, second: &MachineSecond, apps: TickApps<'_>| {
        if !shared.active.load(Ordering::Acquire) {
            accepted = false;
        }
        if accepted {
            if !reset {
                held.reset();
                reset = true;
            }
            accepted = held.take(run, second, apps);
        }
    };
    let left_out = if let Some(budget) = &budget {
        let operation = operation.unwrap_or_else(|| unreachable!("payload reads are cancellable"));
        let outcome = recording::load_payload_window(
            root,
            budget,
            operation,
            since.as_ref(),
            seconds,
            &mut take,
        )?;
        if accepted {
            next_ring.accept(outcome.terminal);
        }
        outcome.left_out
    } else {
        recording::load_window(root, since.as_ref(), seconds, take)
    };
    if held.left_out != left_out {
        held.left_out = left_out;
        held.unpublished = true;
    }
    if budget.is_none() {
        if reset {
            let operation =
                Arc::clone(&shared.cancellation).operation(shared.cancellation.epoch())?;
            next_ring.accept(recording::terminal(root, &operation)?);
        }
    }
    if accepted {
        if reset {
            *ring = next_ring;
        } else {
            if left_out.is_some() {
                *ring = next_ring;
            }
            if let Err(failure) = ring.resume() {
                held.failure = Some(failure);
                held.unpublished = true;
            }
        }
    }
    Ok(accepted)
}

#[derive(Clone, Copy)]
enum Publication {
    Complete,
    Progress,
}

/// Takes in what the ring published since the last step, follows the newest run, and hands
/// over a View where anything changed. `waited` is how the wait before the step went.
fn step(
    held: &mut Held,
    ring: &mut Ring,
    waited: Result<(), Failure>,
    mailbox: &Mailbox<View>,
    operation: Option<&Operation>,
    publication: Publication,
) -> persistence::Result<()> {
    let mut failure = waited.err();

    let read = ring.read(|run, second, apps| held.take(run, second, apps));
    if let Err(read) = read {
        failure = Some(read);
    }

    if let Err(followed) = ring.follow() {
        failure = Some(followed);
    }

    if let Some(operation) = operation {
        operation.check()?;
    }

    if failure != held.failure {
        held.failure = failure;
        held.unpublished = true;
    }
    if held.unpublished {
        let complete = match ring.caught_up() {
            Ok(complete) => complete,
            Err(failure) => {
                held.failure = Some(failure);
                true
            }
        };
        if matches!(publication, Publication::Complete) {
            if !complete {
                if held.failure.is_none() {
                    return Ok(());
                }
            }
        }
        held.publish(mailbox);
    }
    Ok(())
}
