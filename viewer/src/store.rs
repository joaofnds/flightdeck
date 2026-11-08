//! The readings the live window draws, held for the app's life. The store thread writes them
//! into buffers allocated when the store starts, and the main thread reads them through a View
//! that never changes while it holds it.

mod apps;
mod attribution;
mod buffers;
mod cohorts;
mod context_cpu;
mod context_response;
mod coverage;
mod key_order;
mod machine_stretch;
mod native;
mod native_acceptance;
mod payload;
mod query;
mod recording;
mod response;
mod ring;
mod sampled;
mod second;
mod selection;
mod stretch;

#[cfg(test)]
#[path = "store/startup_tests.rs"]
mod startup_tests;

#[cfg(test)]
mod activity_tests;

#[cfg(test)]
mod resume_tests;

#[cfg(test)]
mod expiry_tests;

#[cfg(test)]
mod reconciliation_tests;

#[cfg(test)]
mod retention_tests;

#[cfg(test)]
mod stretch_tests;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub use crate::byte_budget::{ByteBudget, ByteReservation};
pub use apps::{APPS_PER_SECOND_MAX, AppRow, AppRows, AppsSecond, DetachedAppsSecond, Missing};
pub use attribution::{
    Attribution, AttributionReading, COHORT_MAX, Completeness, OtherReading, Resident,
    ResidentOther, Totals,
};
pub use buffers::{Buffers, Mailbox, Span};
pub use cohorts::{Cohorts, Leaders, Readings, WallSeconds};
pub use machine_stretch::{Category, Gauge, MachinePlace, MachineStretch, Rate, pressure_level};
pub use model::summary::{Mark, Reduction};
pub use native::AppMetadata;
pub use payload::{AppDetails, Payload, ProcessName, ProcessRow};
pub use query::{
    HistoryError, HistoryResult, QueryContext, QueryHandle, QueryNeedsLoad, QueryOutcome,
    QueryReadiness, QueryReply, QueryRequest, QuerySecond, QuerySelection, QuerySubmitError,
    QueryToken, QueryUnavailable, SourceGeneration,
};
pub use response::{ContextPart, ContextParts, ContextRanks, Response};
pub use sampled::{SampledRank, SampledRanking, SampledRanks, SampledSecond};
pub(crate) use second::reading_at;
pub use second::{
    BatterySecond, CompleteMachine, CompleteMachineState, CpuCounter, CpuInterval, CpuPrevious,
    MachineAdmission, MachineCaptureError, MachineSecond, MemorySecond, Reading, Traffic, readings,
};
pub use stretch::AppsStretch;

use apps::TickApps;
use collector::ring::{Identity, RunIdentity};
use native::NativeRead;
use native_acceptance::{make_ring, read_native};
use persistence::retention::Retained;
use ring::Ring;
use stretch::Stretches;

use crate::apps::Stamp;
use crate::cancellation::{Cancellation, Operation};
use crate::live::resident_history::{Interval, consecutive};
use crate::live_app_response::Observation;
use crate::session::EARLIER_STRIDE_SECONDS;

#[cfg(test)]
#[path = "store_tests.rs"]
pub(crate) mod tests;

/// The seconds the store holds: the sixteen minutes a live chart draws a second apart, and
/// the two minutes the collector has not sealed yet.
const SECONDS_MAX: usize = 1_080;

/// The seconds the store takes on buffers it already has while a View the main thread holds keeps
/// the span's. Past them each second makes a new buffer, so a View held longer, as a hold does,
/// stops nothing, and the store holds at most this many buffers and the span that View keeps.
const SECONDS_HELD_MAX: usize = 64;

/// The older seconds of the loaded hour the store holds, one in every stretch of
/// `EARLIER_STRIDE_SECONDS` past the live span: the hour and its lead-in, 3,660 seconds, less the
/// live span's 1,080, is 258 stretches, and the stretch the live span cuts into is one more.
const EARLIER_MAX: usize = 259;

/// The older seconds the store takes on buffers it already has while a View the main thread holds
/// keeps theirs. The live span lets go of `SECONDS_HELD_MAX` seconds in that time, which start at
/// most eight stretches.
const EARLIER_HELD_MAX: usize = 8;

/// The closed stretches of `EARLIER_STRIDE_SECONDS` the store holds Apps of: the stretches of
/// the older seconds, and the 108 of the live span's 1,080 seconds, so with the open stretch
/// they hold at least the hour and its lead-in, 3,660 seconds.
const STRETCHES_MAX: usize = EARLIER_MAX + SECONDS_MAX / 10;

/// The closed stretches the store takes on buffers it already has while a View the main thread
/// holds keeps theirs. The store takes `SECONDS_HELD_MAX` seconds in that time, which close at most
/// eight stretches where they follow one another.
const STRETCHES_HELD_MAX: usize = EARLIER_HELD_MAX;

const _: () = {
    assert!(EARLIER_STRIDE_SECONDS == 10);
    assert!(SECONDS_MAX.is_multiple_of(10));
    assert!(STRETCHES_MAX * 10 >= 3_660);
};

/// The Views the store writes: the one in the mailbox, the one the main thread draws, the one
/// it takes in place of that, and one to write.
const VIEWS_MAX: usize = 4;

/// The longest sleep between steps. Native startup decoding can take longer to finish after
/// cancellation, so this is not a shutdown deadline.
const WAIT: Duration = Duration::from_millis(100);

/// The rest between attempts to start on a recording that refused, each of which reads it again.
const STARTUP_RETRY_WAIT: Duration = Duration::from_secs(1);

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
    /// Each App's readings added up over each stretch of `EARLIER_STRIDE_SECONDS` of the loaded
    /// hour, oldest first, but the stretch of the newest second, which later seconds may add to.
    pub stretches: Vec<Arc<AppsStretch>>,
    /// The hover payloads of `earlier`, in the same order.
    pub sampled: Vec<Arc<SampledSecond>>,
    /// Whether the View holds the Apps of every second and stretch it holds, which the store
    /// loads at startup after the readings of the Mac and the Apps of the newest second.
    pub apps_loaded: bool,
    /// Why the store could not read the recording at its last step.
    pub failure: Option<Failure>,
    /// Why the store holds less of the loaded hour than the recording sealed when it started.
    pub left_out: Option<Failure>,
    /// The latest validated ring generation is caught up without an unexplained publication gap.
    /// This does not certify sealed-history coverage, retained context or complete App payloads.
    pub ring_complete: bool,
    /// Greatest wall time of a full-detail reading evicted since the last load reset.
    /// This does not certify checked history or complete payloads; older Views may still own it.
    pub full_detail_evicted_through_ns: Option<u64>,
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
        observations.sort_by(selection::compare);
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
    /// The shared native workspace reservation could not be admitted.
    Budget,
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
    queries: query::Queries,
    stop: AtomicBool,
    active: AtomicBool,
    catch_up: AtomicBool,
    catching_up: AtomicBool,
    paused: AtomicBool,
    cancellation: Arc<Cancellation>,
}

impl Store {
    /// Starts the store thread over the recording at `root`.
    #[cfg(test)]
    pub(crate) fn start(root: &Path) -> io::Result<Self> {
        Self::start_with_payload(root, 1024 * 1024 * 1024)
    }

    /// Starts a store of full process and App payloads with an explicit retained-byte budget.
    pub fn start_with_payload(root: &Path, bytes_max: usize) -> io::Result<Self> {
        Self::start_with_budget(root, Arc::new(ByteBudget::new(bytes_max)))
    }

    /// Shares reservations with the caller's loader inputs, decoded work and retained results.
    pub fn start_with_budget(root: &Path, budget: Arc<ByteBudget>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            mailbox: Mailbox::new(),
            queries: query::Queries::new(),
            stop: AtomicBool::new(false),
            active: AtomicBool::new(true),
            catch_up: AtomicBool::new(false),
            catching_up: AtomicBool::new(true),
            paused: AtomicBool::new(false),
            cancellation: Arc::new(Cancellation::default()),
        });
        let root = root.to_owned();
        let mut held = Held::new(root.clone(), budget);
        let operation = Arc::clone(&shared.cancellation)
            .operation(0)
            .map_err(io::Error::other)?;

        let thread_shared = Arc::clone(&shared);
        let observation =
            persistence::work::ingestion::inherit(persistence::work::ingestion::Producer::Store);
        let thread = thread::Builder::new()
            .name(String::from("flightdeck-store"))
            .spawn(move || {
                let observation = observation.enter();
                // A ring's mapping stays on the thread that maps it.
                let mut ring = make_ring(&root, &held.payload_budget);
                let started =
                    startup_staged(&root, &mut held, &mut ring, &thread_shared, &operation);
                if let Err(error) = started {
                    thread_shared.queries.close();
                    eprintln!("flightdeck store startup: {error}");
                    return;
                }
                let mut load_buffers = recording::LoadBuffers::default();
                run(
                    &root,
                    &mut held,
                    &mut ring,
                    &thread_shared,
                    &mut load_buffers,
                    &operation,
                );
                thread_shared.queries.close();
                drop(observation);
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

    /// An independent source-free query mailbox.
    #[must_use]
    pub fn query_handle(&self) -> QueryHandle {
        let thread = self
            .thread
            .as_ref()
            .unwrap_or_else(|| unreachable!("only dropping the store takes its thread"));

        self.shared.queries.handle(thread.thread().clone())
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
    /// The recording whose retention period says what the store may hold, none in a test of the
    /// store's own bounds.
    root: PathBuf,
    startup: StartupState,
    checked_catalog: Option<recording::CheckedCatalog>,
    source_generation: u64,
    source_status: Option<ring::Status>,
    /// Whether the store holds every second the recording sealed in the loaded hour.
    covers_hour: bool,
    ring_complete: bool,
    full_detail_evicted_through_ns: Option<u64>,
    payload_budget: Arc<ByteBudget>,
    sampled: Span<SampledSecond>,
    span: Span<MachineSecond>,
    /// Each App's totals at each second of `span`, in the same order.
    apps: Span<AppsSecond>,
    earlier: Span<MachineSecond>,
    /// Each App's readings added up over each stretch of the seconds of `earlier` and `span`.
    stretches: Stretches,
    /// The minutes whose Apps a launch left out of `stretches`, which no View holds until the
    /// store adds those Apps up.
    owed: Option<recording::Owed>,
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

impl Newest {
    /// The newest second of a store whose newest second was `newest` once it takes `second` of
    /// run `run` in.
    fn after(newest: Option<&Self>, run: &RunIdentity, second: &MachineSecond) -> Self {
        Self {
            run: *run,
            publication: second.publication,
            segment: segment(newest, run, second),
        }
    }

    /// Whether a store whose newest second is `self` took `second` of run `run` in already, as a
    /// second of the same run no newer than it.
    fn holds(&self, run: &RunIdentity, second: &MachineSecond) -> bool {
        if self.run != *run {
            return false;
        }

        second.publication <= self.publication
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StartupState {
    Loading,
    Primary,
    Complete,
}

impl Held {
    /// A store with no recording behind it, where no retention period applies.
    #[cfg(test)]
    fn without_recording() -> Self {
        Self::without_recording_with_payload(Arc::new(ByteBudget::new(1024 * 1024 * 1024)))
    }

    /// As `without_recording`, for a store that holds samples under `budget`.
    #[cfg(test)]
    fn without_recording_with_payload(budget: Arc<ByteBudget>) -> Self {
        Self::new(PathBuf::new(), budget)
    }

    fn new(root: PathBuf, budget: Arc<ByteBudget>) -> Self {
        let seconds = Buffers::new(SECONDS_MAX + SECONDS_HELD_MAX, MachineSecond::default);
        let apps = Buffers::new(SECONDS_MAX + SECONDS_HELD_MAX, AppsSecond::default);
        let earlier = Buffers::new(EARLIER_MAX + EARLIER_HELD_MAX, MachineSecond::default);
        let views = Buffers::new(VIEWS_MAX, || View {
            version: 0,
            seconds: Vec::with_capacity(SECONDS_MAX),
            apps: Vec::with_capacity(SECONDS_MAX),
            earlier: Vec::with_capacity(EARLIER_MAX),
            stretches: Vec::with_capacity(STRETCHES_MAX),
            sampled: Vec::with_capacity(EARLIER_MAX),
            apps_loaded: false,
            failure: None,
            left_out: None,
            ring_complete: false,
            full_detail_evicted_through_ns: None,
        });

        Self {
            root,
            startup: StartupState::Complete,
            checked_catalog: None,
            source_generation: 0,
            source_status: None,
            covers_hour: false,
            ring_complete: false,
            full_detail_evicted_through_ns: None,
            payload_budget: budget,
            sampled: Span::new(
                EARLIER_MAX,
                Buffers::new(EARLIER_MAX + EARLIER_HELD_MAX, SampledSecond::empty),
            ),
            span: Span::new(SECONDS_MAX, seconds),
            apps: Span::new(SECONDS_MAX, apps),
            earlier: Span::new(EARLIER_MAX, earlier),
            stretches: Stretches::new(STRETCHES_MAX, STRETCHES_HELD_MAX),
            owed: None,
            newest: None,
            views,
            version: 0,
            failure: None,
            left_out: None,
            unpublished: true,
        }
    }

    fn reset(&mut self) {
        self.checked_catalog = None;
        self.source_status = None;
        self.release_views();
        self.span.clear();
        self.span
            .release(|second| *second = MachineSecond::default());
        self.apps.clear();
        self.apps
            .release(|apps| apps.take(TickApps::Missing(&Missing::Unread), None));
        self.earlier.clear();
        self.earlier
            .release(|second| *second = MachineSecond::default());
        self.stretches.clear();
        self.owed = None;
        self.sampled.clear();
        self.sampled
            .release(|sample| *sample = SampledSecond::empty());
        self.newest = None;
        self.covers_hour = false;
        self.ring_complete = false;
        self.full_detail_evicted_through_ns = None;
        self.unpublished = true;
    }

    /// Takes `second` of run `run` in after the newest, with what the store found of its Apps,
    /// `apps`, and keeps the second the live span lets go of where it is the first of its
    /// stretch. A second of the run no newer than the newest taken in is held already. Returns
    /// false and changes nothing where the budget refuses the second's sample, so the caller
    /// takes the second again at its next step.
    fn take(&mut self, run: &RunIdentity, second: &MachineSecond, apps: TickApps<'_>) -> bool {
        if self.holds(run, second) {
            return true;
        }

        self.release_views();

        let leaving = self.span.leaving().cloned();
        let kept = leaving
            .clone()
            .filter(|leaving| starts_stretch(self.earlier.newest(), leaving));
        let Ok(prepared_sample) = self.take_sample(kept.as_ref()) else {
            return false;
        };
        let newest = Newest::after(self.newest.as_ref(), run, second);
        if let Some(kept) = kept {
            let sample = prepared_sample.unwrap_or_else(|| unreachable!("admitted sample"));
            self.sampled.push(|buffer| *buffer = sample);
            self.earlier.push(|buffer| *buffer = kept);
        }
        self.take_current(run, second, apps, newest.segment);
        persistence::work::ingestion::record_publication(
            *run,
            second.publication,
            persistence::work::ingestion::Kind::Accepted,
        );
        if let Some(leaving) = leaving {
            self.full_detail_evicted_through_ns = Some(
                self.full_detail_evicted_through_ns
                    .map_or(leaving.wall_ns, |previous| previous.max(leaving.wall_ns)),
            );
        }
        self.newest = Some(newest);
        self.unpublished = true;
        true
    }

    /// Whether the store took `second` of run `run` in already.
    fn holds(&self, run: &RunIdentity, second: &MachineSecond) -> bool {
        self.newest
            .as_ref()
            .is_some_and(|newest| newest.holds(run, second))
    }

    fn take_sample(&self, kept: Option<&MachineSecond>) -> Result<Option<SampledSecond>, Missing> {
        let Some(kept) = kept else {
            return Ok(None);
        };

        let apps = self
            .apps
            .leaving()
            .unwrap_or_else(|| unreachable!("departing Apps"));
        if apps.missing == Some(Missing::Unread) {
            return Ok(Some(SampledSecond::unread(
                kept,
                apps.observation
                    .unwrap_or_else(|| unreachable!("departing identity")),
            )));
        }
        SampledSecond::admitted(kept, apps, &self.payload_budget).map(Some)
    }

    fn take_current(
        &mut self,
        run: &RunIdentity,
        second: &MachineSecond,
        apps: TickApps<'_>,
        segment: u64,
    ) {
        self.span.push(|buffer| {
            *buffer = second.clone();
            buffer.boot = run.boot.0;
            buffer.run = run.run.0;
            buffer.segment = segment;
        });
        let folded = match &apps {
            TickApps::Folded(folded) => Some(&folded.apps),
            _ => None,
        };
        let previous = self.apps.newest_held().map(Arc::clone);
        let tick = take_observation(run, second);
        self.apps.push(|buffer| {
            buffer.take(apps, previous.as_deref());
            if buffer.observation.is_none() {
                buffer.observation = Some(tick);
            }
            buffer.consecutive = take_consecutive(previous.as_deref(), buffer);
        });
        let Some(newest) = self.apps.newest() else {
            unreachable!("the span just took the second in");
        };
        if let Some(observation) = newest.observation {
            assert_eq!(observation.stamp.publication, second.publication);
        }
        self.stretches.take(stretch(second), newest, run, second);
        if let Some(folded) = folded {
            self.stretches.add_folded(folded);
        }
    }

    fn release_views(&mut self) {
        self.views.release(|view| {
            view.seconds.clear();
            view.apps.clear();
            view.earlier.clear();
            view.stretches.clear();
            view.sampled.clear();
        });
        self.apps.release(|apps| {
            if apps.apps.reserved_bytes() == 0 {
                if apps.apps.capacity() > 0 {
                    apps.take(TickApps::Missing(&Missing::Unread), None);
                    return;
                }
            }
            *apps = AppsSecond::default();
        });
        self.span
            .release(|second| *second = MachineSecond::default());
        self.earlier
            .release(|second| *second = MachineSecond::default());
        self.stretches.release();
        self.sampled
            .release(|second| *second = SampledSecond::empty());
    }

    /// Lets go of what the retention period no longer covers, oldest first, so the next View
    /// holds none of it. A View already handed over keeps what it holds until it is replaced.
    /// A reader asked for an older second goes to the recording, which hides the same readings.
    fn expire(&mut self) {
        let boots = self
            .earlier
            .held()
            .chain(self.span.held())
            .map(|second| Identity(second.boot));
        let retained = Retained::read(&self.root, boots);
        let mut removed = self.stretches.expire(&retained);
        for _ in 0..self.earlier.expired_count(&retained, expire_place) {
            self.earlier.remove_oldest();
            self.sampled.remove_oldest();
            removed = true;
        }
        for _ in 0..self.span.expired_count(&retained, expire_place) {
            self.span.remove_oldest();
            self.apps.remove_oldest();
            removed = true;
        }

        self.unpublished |= removed;
    }

    /// Hands the main thread a View of what the store holds, or leaves it to the next step
    /// where the main thread holds every View.
    fn publish(&mut self, mailbox: &Mailbox<View>) {
        self.expire();
        self.release_views();
        let version = self.version + 1;
        let span = &self.span;
        let apps = &self.apps;
        let earlier = &self.earlier;
        // A store still loading holds stretches of no Apps. One that owes its stretches the Apps
        // of its minutes writes each stretch again as it adds them, which a View holding the
        // stretch would leave no buffer for, so it hands over none until then.
        let stretches = (self.startup == StartupState::Complete && self.owed.is_none())
            .then_some(&self.stretches);
        let sampled = &self.sampled;
        let complete = self.startup == StartupState::Complete;
        let failure = self.failure;
        let left_out = self.left_out;
        let ring_complete = self.ring_complete;
        let full_detail_evicted_through_ns = self.full_detail_evicted_through_ns;
        let written = self.views.write(|view| {
            assert!(view.seconds.is_empty());
            assert!(view.apps.is_empty());
            assert!(view.earlier.is_empty());
            assert!(view.stretches.is_empty());
            assert!(view.sampled.is_empty());
            let capacity = (
                view.seconds.capacity(),
                view.apps.capacity(),
                view.earlier.capacity(),
                view.stretches.capacity(),
                view.sampled.capacity(),
            );

            view.version = version;
            view.seconds.extend(span.held().map(Arc::clone));
            view.apps.extend(apps.held().map(Arc::clone));
            view.earlier.extend(earlier.held().map(Arc::clone));
            if let Some(stretches) = stretches {
                view.stretches.extend(stretches.held().map(Arc::clone));
            }
            view.sampled.extend(sampled.held().map(Arc::clone));
            assert_eq!(view.sampled.len(), view.earlier.len());
            view.apps_loaded = complete;
            view.failure = failure;
            view.left_out = left_out;
            view.ring_complete = ring_complete;
            view.full_detail_evicted_through_ns = full_detail_evicted_through_ns;

            assert_eq!(view.apps.len(), view.seconds.len());
            let held = (
                view.seconds.capacity(),
                view.apps.capacity(),
                view.earlier.capacity(),
                view.stretches.capacity(),
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
        if second.follows() {
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
    stretch_of_wall_ns(second.wall_ns)
}

/// The stretch of `EARLIER_STRIDE_SECONDS` that holds the wall time `wall_ns`. Both divisions
/// floor.
fn stretch_of_wall_ns(wall_ns: u64) -> u64 {
    stretch_of_wall_second(wall_ns / 1_000_000_000)
}

/// The stretch of `EARLIER_STRIDE_SECONDS` that holds the wall second `wall_second`, which
/// `AppsStretch::stretch` names. The division floors.
#[must_use]
pub fn stretch_of_wall_second(wall_second: u64) -> u64 {
    wall_second / EARLIER_STRIDE_SECONDS
}

/// Where `second` lies on the retention clock, which one with no clock has no place on.
fn expire_place(second: &MachineSecond) -> Option<(Identity, u64)> {
    Some((Identity(second.boot), second.continuous_ns?))
}

/// Adds up the Apps a launch owes the store's stretches once the main thread has taken the
/// store's first View, and hands over a View that holds them.
fn run_owed(
    root: &Path,
    held: &mut Held,
    mailbox: &Mailbox<View>,
    load_buffers: &mut recording::LoadBuffers,
) {
    // A launch hands over its first View before it adds up the Apps it owes its stretches, and
    // waits for the main thread to take it, since the pass's loaders would take the cores the
    // window's first frame needs.
    if held.version == 0 {
        return;
    }
    if mailbox.pending() {
        return;
    }
    let Some(owed) = held.owed.take() else {
        return;
    };

    run_owed_add(root, held, &owed, load_buffers);
    held.publish(mailbox);
}

/// Adds the Apps of the minutes `owed` holds that are older than the live span to the stretches
/// that hold those minutes' seconds, as a load of the whole hour adds them up.
fn run_owed_add(
    root: &Path,
    held: &mut Held,
    owed: &recording::Owed,
    load_buffers: &mut recording::LoadBuffers,
) {
    let mut walk = OwedWalk::default();
    let stretches = &mut held.stretches;
    let left_out = recording::load_owed(root, owed, load_buffers, |run, second, apps| {
        let Some(start) = walk.step(run, second) else {
            return;
        };
        if let TickApps::Folded(folded) = apps {
            stretches.add_owed(&start.run, start.publication, &folded.apps);
        }
    });

    if left_out.is_some() {
        held.left_out = left_out;
        held.covers_hour = false;
    }
    held.unpublished = true;
}

/// Where a walk through the seconds a launch handed over stands, as the store took them in.
#[derive(Default)]
struct OwedWalk {
    /// The run and publication of the newest second the store took in.
    newest: Option<(RunIdentity, u64)>,
    /// The first second of the stretch the store took that second into.
    start: Option<StretchStart>,
}

/// The first second the store took into a stretch, by its run and its publication.
#[derive(Clone, Copy)]
struct StretchStart {
    stretch: u64,
    run: RunIdentity,
    publication: u64,
}

impl OwedWalk {
    /// The first second of the stretch the store took the second of `tick` of run `run` into,
    /// or `None` where it held that second already and took it in no second time, as a second of
    /// the same run no newer than the newest it took in.
    fn step(&mut self, run: &RunIdentity, tick: &recording::OwedTick) -> Option<StretchStart> {
        if self.newest.is_some_and(|(newest_run, publication)| {
            newest_run == *run && tick.publication <= publication
        }) {
            return None;
        }

        self.newest = Some((*run, tick.publication));
        let stretch = stretch_of_wall_ns(tick.wall_ns);
        if let Some(start) = self.start {
            if start.stretch == stretch {
                return Some(start);
            }
        }
        let start = StretchStart {
            stretch,
            run: *run,
            publication: tick.publication,
        };
        self.start = Some(start);
        Some(start)
    }
}

/// Loads the sealed hour in one pass and follows the ring from its newest minute, as a test sets
/// up the store a window starts through `startup_staged`.
#[cfg(test)]
fn startup(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    operation: &Operation,
) -> persistence::Result<()> {
    let budget = Arc::clone(&held.payload_budget);
    let take = |run: &RunIdentity, second: &MachineSecond, apps: TickApps<'_>| {
        assert!(held.take(run, second, apps));
    };
    let outcome = recording::load_payload(root, &budget, operation, take);

    startup_accept(outcome, held, ring, operation)
}

#[cfg(test)]
fn startup_accept(
    outcome: persistence::Result<recording::Outcome>,
    held: &mut Held,
    ring: &mut Ring,
    operation: &Operation,
) -> persistence::Result<()> {
    let outcome = outcome?;
    operation.check()?;
    coverage::accept_catalog(held, outcome.checked_catalog)?;
    held.left_out = outcome.left_out;
    if held.left_out.is_none() {
        held.covers_hour = true;
    }
    ring.accept(outcome.terminal);

    Ok(())
}

/// A Store with a budget uses the same producer while yielding between finite source steps.
fn startup_staged(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    operation: &Operation,
) -> persistence::Result<()> {
    use recording::staged::{Failure as StartupFailure, Input, Startup};
    let budget = Arc::clone(&held.payload_budget);
    let input = Input {
        root,
        budget: &budget,
        operation,
    };
    let mut startup = Startup::new();
    let mut blocked = None;
    let mut reported = None;
    held.startup = StartupState::Loading;
    for _ in 0..STEPS_MAX {
        operation.check()?;
        let restart =
            match startup_staged_turn(&input, held, ring, shared, &mut startup, &mut blocked) {
                Ok(true) => return Ok(()),
                Ok(false) => false,
                Err(StartupFailure::Catalog(
                    Failure::ChangedIdentity | Failure::ForeignRecording,
                )) => true,
                // The collector makes the recording after the window opens.
                Err(StartupFailure::Catalog(Failure::Recording(io::ErrorKind::NotFound))) => {
                    thread::park_timeout(WAIT);
                    true
                }
                // A saved minute that reads badly costs its own seconds, and the load that serves
                // every later catch-up leaves it out and goes on.
                Err(StartupFailure::Read(refused)) => {
                    operation.check()?;
                    eprintln!("flightdeck store startup: {refused:?}");
                    held.reset();
                    held.startup = StartupState::Complete;
                    shared.catch_up.store(true, Ordering::Release);
                    return Ok(());
                }
                // What refuses now may clear while the window stays open.
                Err(StartupFailure::Blocked) => {
                    startup_staged_refused(held, ring, shared, &mut reported, Failure::Budget);
                    true
                }
                Err(StartupFailure::Catalog(failure)) => {
                    startup_staged_refused(held, ring, shared, &mut reported, failure);
                    true
                }
            };
        if restart {
            held.reset();
            held.startup = StartupState::Loading;
            *ring = make_ring(root, &budget);
            startup = Startup::new();
            blocked = None;
        }
    }
    unreachable!("startup finishes or the Store is cancelled");
}

/// Answers the page's query with why the Store cannot start, and rests before the next attempt,
/// which reads the recording again.
fn startup_staged_refused(
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    reported: &mut Option<Failure>,
    failure: Failure,
) {
    if *reported != Some(failure) {
        eprintln!("flightdeck store startup: {failure:?}");
    }
    *reported = Some(failure);
    held.failure = Some(failure);
    run_queries(held, ring, &shared.queries);
    thread::park_timeout(STARTUP_RETRY_WAIT);
}

fn startup_staged_turn(
    input: &recording::staged::Input<'_>,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    startup: &mut recording::staged::Startup,
    blocked: &mut Option<StartupCapacity>,
) -> Result<bool, recording::staged::Failure> {
    use recording::staged::{Advance, Failure as StartupFailure};
    if !shared.active.load(Ordering::Acquire) {
        shared.paused.store(true, Ordering::Release);
        thread::park();
        shared.paused.store(false, Ordering::Release);
        startup.revalidate(input)?;
        return Ok(false);
    }

    if held.startup == StartupState::Primary {
        if shared.queries.requested() {
            startup.revalidate(input)?;
            run_queries(held, ring, &shared.queries);
        }
        startup_staged_native(input, held, ring, shared)?;
        if coverage::source_changed(held, ring) {
            return Err(StartupFailure::Catalog(Failure::ChangedIdentity));
        }
    }
    if let Some(previous) = blocked {
        held.release_views();
        let capacity = startup_capacity(held, input.budget);
        if !capacity.improved(previous) {
            startup_staged_follow(held, ring);
            thread::park_timeout(WAIT);
            return Ok(false);
        }
    }
    match startup.advance(input, held, ring)? {
        Advance::Progress => *blocked = None,
        Advance::Blocked => {
            *blocked = Some(startup_capacity(held, input.budget));
            thread::park_timeout(WAIT);
        }
        Advance::Primary => {
            if startup_staged_native(input, held, ring, shared)? == NativeRead::Blocked {
                *blocked = Some(startup_capacity(held, input.budget));
                thread::park_timeout(WAIT);
                return Ok(false);
            }

            *blocked = None;
            if ring.caught_up().map_err(StartupFailure::Catalog)? {
                if held.failure.is_none() {
                    startup.primary(held)?;
                }
            }
            if held.failure.is_some() {
                thread::park_timeout(WAIT);
            }
        }
        Advance::Complete => return Ok(true),
    }
    Ok(false)
}

fn startup_staged_follow(held: &mut Held, ring: &mut Ring) {
    if held.startup == StartupState::Loading {
        if ring.identity().is_some() {
            let failure = ring.follow().err();
            if failure != held.failure {
                held.failure = failure;
                held.unpublished = true;
            }
        }
    }
}

fn startup_staged_native(
    input: &recording::staged::Input<'_>,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
) -> Result<NativeRead, recording::staged::Failure> {
    let progress = step(
        held,
        ring,
        Ok(()),
        &shared.mailbox,
        input.operation,
        Publication::Deferred,
    )?;
    if ring.reload_required() {
        return Err(recording::staged::Failure::Catalog(
            Failure::ChangedIdentity,
        ));
    }
    Ok(progress)
}

#[derive(PartialEq, Eq)]
struct StartupCapacity {
    bytes: usize,
    apps: usize,
    sampled: usize,
    newest: Option<(RunIdentity, u64)>,
}

impl StartupCapacity {
    fn improved(&self, previous: &Self) -> bool {
        if self.bytes < previous.bytes {
            return true;
        }

        if self.apps > previous.apps {
            return true;
        }

        if self.sampled > previous.sampled {
            return true;
        }

        self.newest != previous.newest
    }
}

fn startup_capacity(held: &Held, budget: &ByteBudget) -> StartupCapacity {
    StartupCapacity {
        bytes: budget.used(),
        apps: held.apps.free(),
        sampled: held.sampled.free(),
        newest: held.newest.map(|newest| (newest.run, newest.publication)),
    }
}

/// Steps until the store is dropped.
fn run(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    load_buffers: &mut recording::LoadBuffers,
    operation: &Operation,
) {
    let mut waited = Ok(());
    for _ in 0..STEPS_MAX {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        if !shared.active.load(Ordering::Acquire) {
            run_answers(held, ring, shared);
            shared.paused.store(true, Ordering::Release);
            thread::park();
            shared.paused.store(false, Ordering::Release);
            continue;
        }
        held.expire();
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
                    run_answers(held, ring, shared);
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
        // After the step, since a query that came with a new reading in the ring would otherwise
        // be told the Store has not caught up, and ask again a poll later.
        run_answers(held, ring, shared);
        if run_retry(held, ring, shared) {
            waited = Ok(());
            continue;
        }
        run_ready(held, ring, shared);
        run_owed(root, held, &shared.mailbox, load_buffers);
        waited = if held.failure.is_some() {
            // A ring that failed may fail again at once, so the thread sleeps instead.
            thread::sleep(WAIT);
            Ok(())
        } else {
            run_wait(ring, shared)
        };
    }
    unreachable!("the store thread steps until the store is dropped");
}

/// Waits on the ring, leaving where a consumer can reach it what ends that wait.
fn run_wait(ring: &Ring, shared: &Shared) -> Result<(), Failure> {
    shared.queries.set_ring_waker(ring.waker());

    ring.wait(WAIT)
}

fn run_answers(held: &mut Held, ring: &mut Ring, shared: &Shared) {
    run_queries(held, ring, &shared.queries);
}

fn run_queries(held: &mut Held, ring: &mut Ring, queries: &query::Queries) {
    let checked = match coverage::checked(held, ring) {
        Ok(checked) => checked,
        Err(failure) => {
            held.failure = Some(failure);
            held.unpublished = true;
            None
        }
    };
    queries.serve(held, checked.as_ref(), &held.payload_budget);
}

fn run_retry(held: &mut Held, ring: &Ring, shared: &Shared) -> bool {
    // A load that lost its run holds no catalog, so the Store could answer no query until it read
    // the hour again. That read starts over, since what it would continue from may be what went.
    if held.left_out == Some(Failure::ChangedIdentity) {
        held.reset();
    } else if !ring.reload_required() {
        if !coverage::source_changed(held, ring) {
            return false;
        }
    }
    held.checked_catalog = None;
    held.covers_hour = false;
    held.ring_complete = false;
    shared.catch_up.store(true, Ordering::Release);
    true
}

fn run_ready(held: &mut Held, ring: &mut Ring, shared: &Shared) {
    if !shared.catching_up.load(Ordering::Acquire) {
        return;
    }

    let caught_up = match ring.caught_up() {
        Ok(caught_up) => caught_up,
        Err(failure) => {
            held.failure = Some(failure);
            held.ring_complete = false;
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
    if !shared.catching_up.load(Ordering::Acquire) {
        ring.finish_resume();
    }
}

/// Reuses a covered checkpoint or takes in what the recording sealed since it before publishing
/// its endpoint.
fn refresh(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    operation: &Operation,
) -> persistence::Result<bool> {
    drop(shared.mailbox.take());
    held.unpublished = true;
    if held.covers_hour {
        if let Some(checkpoint) = &held.newest {
            match ring.covers(checkpoint) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(failure) => {
                    held.failure = Some(failure);
                    *ring = make_ring(root, &held.payload_budget);
                    return Ok(true);
                }
            }
        }
    }

    refresh_load(root, held, ring, shared, operation)
}

/// Takes in what the recording sealed after the newest second the store holds, and follows a new
/// ring from the newest sealed minute. An hour that left a sealed minute out reads every minute
/// again, so the refused one is retried, as does a store whose ring overran while it followed it,
/// which holds seconds past readings it never took, and a store whose newest second the loaded
/// hour no longer holds.
fn refresh_load(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    operation: &Operation,
) -> persistence::Result<bool> {
    let budget = Arc::clone(&held.payload_budget);
    let since = held.newest;
    let reads_hour = held.left_out.is_some() || ring.reload_required();
    let mut accepted = true;
    let mut loaded = false;
    let mut checked_catalog = None;
    let mut next_ring = make_ring(root, &budget);
    let mut take = |continuity, run: &RunIdentity, second: &MachineSecond, apps: TickApps<'_>| {
        if !shared.active.load(Ordering::Acquire) {
            accepted = false;
        }
        if accepted {
            if !loaded {
                if continuity == recording::Continuity::Replace {
                    held.reset();
                }
                loaded = true;
            }
            accepted = held.take(run, second, apps);
        }
    };
    let outcome = if reads_hour {
        recording::load_payload_hour(
            root,
            &budget,
            operation,
            since.as_ref(),
            |run, second, apps| {
                take(recording::Continuity::Replace, run, second, apps);
            },
        )?
    } else {
        recording::load_payload_since(root, &budget, operation, since.as_ref(), &mut take)?
    };
    if accepted {
        next_ring.accept(outcome.terminal);
        // A continued load decoded none of the minutes the store took in before, which the store
        // holds, as it holds the seconds it follows from a ring past its catalog.
        checked_catalog = outcome.checked_catalog.or(outcome.continued_catalog);
    }
    let left_out = outcome.left_out;
    if held.left_out != left_out {
        held.left_out = left_out;
        held.unpublished = true;
    }
    if accepted {
        coverage::accept_catalog(held, checked_catalog)?;
        held.covers_hour = left_out.is_none();
        refresh_load_finish(held, ring, next_ring, loaded);
    }
    Ok(accepted)
}

fn refresh_load_finish(held: &mut Held, ring: &mut Ring, next: Ring, loaded: bool) {
    if loaded {
        *ring = next;
    } else {
        if held.left_out.is_some() {
            *ring = next;
        }
        if let Err(failure) = ring.resume() {
            held.failure = Some(failure);
            held.unpublished = true;
        }
    }
}

#[derive(Clone, Copy)]
enum Publication {
    Complete,
    Progress,
    Deferred,
}

/// Takes in what the ring published since the last step, follows the newest run, and hands
/// over a View where anything changed. `waited` is how the wait before the step went.
/// The return value reports native admission.
fn step(
    held: &mut Held,
    ring: &mut Ring,
    waited: Result<(), Failure>,
    mailbox: &Mailbox<View>,
    operation: &Operation,
    publication: Publication,
) -> persistence::Result<NativeRead> {
    operation.check()?;
    let mut failure = waited.err();

    let progress = match read_native(held, ring, operation) {
        Ok(progress) => progress,
        Err(read) => {
            failure = Some(read);
            NativeRead::Idle
        }
    };

    if let Err(followed) = ring.follow() {
        failure = Some(followed);
    }

    operation.check()?;

    if ring.reload_required() {
        return Ok(progress);
    }

    if failure != held.failure {
        held.failure = failure;
        held.unpublished = true;
    }
    let complete = step_complete(held, ring);
    if held.unpublished {
        if matches!(publication, Publication::Deferred) {
            return Ok(progress);
        }
        if matches!(publication, Publication::Complete) {
            if !complete {
                if held.failure.is_none() {
                    return Ok(progress);
                }
            }
        }
        held.publish(mailbox);
    }
    Ok(progress)
}

fn step_complete(held: &mut Held, ring: &mut Ring) -> bool {
    let status = ring.status();
    if held.source_status != status {
        held.source_status = status;
        held.unpublished = true;
    }
    let complete = match ring.caught_up() {
        Ok(complete) => complete,
        Err(failure) => {
            held.failure = Some(failure);
            held.unpublished = true;
            true
        }
    };
    let mut ring_complete = false;
    if complete {
        if held.failure.is_none() {
            ring_complete = held
                .newest
                .as_ref()
                .is_some_and(|newest| ring.complete(&newest.run.recording));
        }
    }
    if held.ring_complete != ring_complete {
        held.ring_complete = ring_complete;
        held.unpublished = true;
    }

    complete
}
