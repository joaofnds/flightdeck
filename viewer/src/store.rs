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
mod native;
mod native_acceptance;
mod payload;
mod query;
mod raw_cache;
mod raw_response;
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
mod reconciliation_tests;

#[cfg(test)]
mod retention_tests;

#[cfg(test)]
mod stretch_tests;

use std::io;
use std::path::Path;
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
pub use native::AppMetadata;
pub use payload::{AppDetails, Payload, ProcessName, ProcessRow};
pub use query::{
    HistoryError, HistoryResult, QueryContext, QueryHandle, QueryNeedsLoad, QueryOutcome,
    QueryReadiness, QueryReply, QueryRequest, QuerySecond, QuerySelection, QuerySubmitError,
    QueryToken, QueryUnavailable, SourceGeneration,
};
pub use response::{ContextPart, ContextParts, ContextRanks, Response};
pub use sampled::{SampledRank, SampledRanking, SampledRanks, SampledSecond};
pub use second::{
    BatterySecond, CompleteMachine, CompleteMachineState, CpuCounter, CpuInterval, CpuPrevious,
    MachineAdmission, MachineCaptureError, MachineSecond, MemorySecond, Reading, Traffic, readings,
};
pub use stretch::AppsStretch;

use apps::TickApps;
use collector::ring::RunIdentity;
use native::NativeRead;
use native_acceptance::{make_ring, read_native};
use ring::Ring;
use stretch::Stretches;

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

/// The closed stretches of `EARLIER_STRIDE_SECONDS` the store holds Apps of: the stretches of
/// the older seconds, and the 108 of the live span's 1,080 seconds, so with the open stretch
/// they hold at least the hour and its lead-in, 3,660 seconds.
const STRETCHES_MAX: usize = EARLIER_MAX + SECONDS_MAX / 10;

/// The closed stretches past their span a View the main thread holds may keep. While it holds
/// one, the store takes in at most `SECONDS_HELD_MAX` seconds, which close at most eight
/// stretches where they follow one another.
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
    /// Older hover payloads, populated only by the opt-in complete payload store.
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
    queries: Option<query::Queries>,
    stop: AtomicBool,
    active: AtomicBool,
    catch_up: AtomicBool,
    catching_up: AtomicBool,
    paused: AtomicBool,
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
            queries: budget.as_ref().map(|_| query::Queries::new()),
            stop: AtomicBool::new(false),
            active: AtomicBool::new(true),
            catch_up: AtomicBool::new(false),
            catching_up: AtomicBool::new(true),
            paused: AtomicBool::new(false),
            cancellation: Arc::new(Cancellation::default()),
        });
        let mut held = match budget {
            Some(budget) => Held::with_payload(budget),
            None => Held::new(),
        };
        let root = root.to_owned();
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
                let mut ring = make_ring(&root, held.payload_budget.as_ref());
                let started = if held.payload_budget.is_some() {
                    startup_staged(&root, &mut held, &mut ring, &thread_shared, &operation)
                } else {
                    startup(&root, &mut held, &mut ring, &operation)
                };
                if let Err(error) = started {
                    if let Some(queries) = &thread_shared.queries {
                        queries.close();
                    }
                    eprintln!("flightdeck store startup: {error}");
                    return;
                }
                let mut load_buffers = recording::LoadBuffers::default();
                let operation = held.payload_budget.as_ref().map(|_| &operation);
                run(
                    &root,
                    &mut held,
                    &mut ring,
                    &thread_shared,
                    &mut load_buffers,
                    operation,
                );
                if let Some(queries) = &thread_shared.queries {
                    queries.close();
                }
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

    /// An independent source-free query mailbox, enabled only by the complete-payload path.
    #[must_use]
    pub fn query_handle(&self) -> Option<QueryHandle> {
        let queries = self.shared.queries.as_ref()?;
        let thread = self.thread.as_ref()?;

        Some(queries.handle(thread.thread().clone()))
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
    startup: StartupState,
    checked_catalog: Option<recording::CheckedCatalog>,
    source_generation: u64,
    source_status: Option<ring::Status>,
    /// Whether the store holds every second the recording sealed in the loaded hour.
    covers_hour: bool,
    ring_complete: bool,
    full_detail_evicted_through_ns: Option<u64>,
    payload_budget: Option<Arc<ByteBudget>>,
    raw: Option<raw_cache::RawCache>,
    sampled: Option<Span<SampledSecond>>,
    span: Span<MachineSecond>,
    /// Each App's totals at each second of `span`, in the same order.
    apps: Span<AppsSecond>,
    earlier: Span<MachineSecond>,
    /// Each App's readings added up over each stretch of the seconds of `earlier` and `span`.
    stretches: Stretches,
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
    /// The publication of the newest second of `run` whose Apps the stretches hold, which a
    /// loader hands over with an older second, so it is never older than `publication`.
    apps_publication: u64,
}

impl Newest {
    /// Whether a store whose newest second is `self` took `second` of run `run` in already, as a
    /// second of the same run no newer than it.
    fn holds(&self, run: &RunIdentity, second: &MachineSecond) -> bool {
        if self.run != *run {
            return false;
        }

        second.publication <= self.publication
    }

    /// Whether a store whose newest second is `self` holds the Apps of `second` of run `run` in
    /// its stretches already, as a second of the same run no newer than `apps_publication`.
    fn holds_apps(&self, run: &RunIdentity, second: &MachineSecond) -> bool {
        if self.run != *run {
            return false;
        }

        second.publication <= self.apps_publication
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StartupState {
    Loading,
    Primary,
    Complete,
}

impl Held {
    fn new() -> Self {
        Self::with_apps(AppsSecond::with_room)
    }

    fn with_payload(budget: Arc<ByteBudget>) -> Self {
        let mut held = Self::with_apps(AppsSecond::default);
        held.enable_payload(budget);
        held
    }

    fn with_apps(make_apps: impl FnMut() -> AppsSecond) -> Self {
        let seconds = Buffers::new(SECONDS_MAX + SECONDS_HELD_MAX, MachineSecond::default);
        let apps = Buffers::new(SECONDS_MAX + SECONDS_HELD_MAX, make_apps);
        let earlier = Buffers::new(EARLIER_MAX + EARLIER_HELD_MAX, MachineSecond::default);
        let views = Buffers::new(VIEWS_MAX, || View {
            version: 0,
            seconds: Vec::with_capacity(SECONDS_MAX),
            apps: Vec::with_capacity(SECONDS_MAX),
            earlier: Vec::with_capacity(EARLIER_MAX),
            stretches: Vec::with_capacity(STRETCHES_MAX),
            sampled: Vec::new(),
            apps_loaded: false,
            failure: None,
            left_out: None,
            ring_complete: false,
            full_detail_evicted_through_ns: None,
        });

        Self {
            startup: StartupState::Complete,
            checked_catalog: None,
            source_generation: 0,
            source_status: None,
            covers_hour: false,
            ring_complete: false,
            full_detail_evicted_through_ns: None,
            payload_budget: None,
            raw: None,
            sampled: None,
            span: Span::new(SECONDS_MAX, seconds),
            apps: Span::new(SECONDS_MAX, apps),
            earlier: Span::new(EARLIER_MAX, earlier),
            stretches: Stretches::new(STRETCHES_MAX, STRETCHES_HELD_MAX),
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
        self.checked_catalog = None;
        self.source_status = None;
        if let Some(raw) = &mut self.raw {
            raw.clear();
        }
        self.release_views();
        self.span.clear();
        self.span
            .release(|second| *second = MachineSecond::default());
        self.apps.clear();
        self.apps
            .release(|apps| apps.take(TickApps::Missing(&Missing::Unread), None, None));
        self.earlier.clear();
        self.earlier
            .release(|second| *second = MachineSecond::default());
        self.stretches.clear();
        if let Some(sampled) = &mut self.sampled {
            sampled.clear();
            sampled.release(|sample| *sample = SampledSecond::empty());
        }
        self.newest = None;
        self.covers_hour = false;
        self.ring_complete = false;
        self.full_detail_evicted_through_ns = None;
        self.unpublished = true;
    }

    /// Takes `second` of run `run` in after the newest, with what the store found of its Apps,
    /// `apps`, and keeps the second the live span lets go of where it is the first of its
    /// stretch. A second of the run no newer than the newest taken in is held already. Returns
    /// false and changes nothing where a span has no buffer to write, so the caller takes the
    /// second again at its next step.
    fn take(&mut self, run: &RunIdentity, second: &MachineSecond, apps: TickApps<'_>) -> bool {
        if self.holds(run, second) {
            return true;
        }

        self.release_views();

        let leaving = self.span.leaving().cloned();
        let kept = leaving
            .clone()
            .filter(|leaving| starts_stretch(self.earlier.newest(), leaving));
        if !self.take_room(kept.is_some(), stretch(second)) {
            return false;
        }
        let Ok(prepared_sample) = self.take_sample(kept.as_ref()) else {
            return false;
        };
        let segment = segment(self.newest.as_ref(), run, second);
        let apps_publication = apps_publication(self.newest.as_ref(), run, second, &apps);
        if let Some(kept) = kept {
            if let Some(sampled) = &mut self.sampled {
                let sample = prepared_sample.unwrap_or_else(|| unreachable!("admitted sample"));
                let pushed = sampled.push(|buffer| *buffer = sample);
                assert!(pushed);
            }
            let pushed = self.earlier.push(|buffer| *buffer = kept);
            assert!(pushed);
        }
        self.take_current(run, second, apps, segment);
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
        self.newest = Some(Newest {
            run: *run,
            publication: second.publication,
            segment,
            apps_publication,
        });
        self.unpublished = true;
        true
    }

    /// Whether the store took `second` of run `run` in already.
    fn holds(&self, run: &RunIdentity, second: &MachineSecond) -> bool {
        self.newest
            .as_ref()
            .is_some_and(|newest| newest.holds(run, second))
    }

    fn take_room(&self, kept: bool, stretch: u64) -> bool {
        if !self.span.has_room() {
            return false;
        }

        if !self.apps.has_room() {
            return false;
        }

        if !self.stretches.has_room(stretch) {
            return false;
        }

        if kept {
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
        true
    }

    fn take_sample(&self, kept: Option<&MachineSecond>) -> Result<Option<SampledSecond>, Missing> {
        let Some(kept) = kept else {
            return Ok(None);
        };

        let Some(budget) = &self.payload_budget else {
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
        SampledSecond::admitted(kept, apps, budget).map(Some)
    }

    fn take_current(
        &mut self,
        run: &RunIdentity,
        second: &MachineSecond,
        apps: TickApps<'_>,
        segment: u64,
    ) {
        let pushed = self.span.push(|buffer| {
            *buffer = second.clone();
            buffer.segment = segment;
        });
        assert!(pushed);
        let folded = match &apps {
            TickApps::Folded(folded) => Some(&folded.apps),
            _ => None,
        };
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
        let Some(newest) = self.apps.newest() else {
            unreachable!("the span just took the second in");
        };
        if let Some(observation) = newest.observation {
            assert_eq!(observation.stamp.publication, second.publication);
        }
        self.stretches.take(stretch(second), newest);
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
        if self.payload_budget.is_some() {
            self.apps.release(|apps| {
                if apps.apps.reserved_bytes() == 0 {
                    if apps.apps.capacity() > 0 {
                        apps.take(TickApps::Missing(&Missing::Unread), None, None);
                        return;
                    }
                }
                *apps = AppsSecond::default();
            });
        }
        self.span
            .release(|second| *second = MachineSecond::default());
        self.earlier
            .release(|second| *second = MachineSecond::default());
        self.stretches.release();
        if let Some(sampled) = &mut self.sampled {
            sampled.release(|second| *second = SampledSecond::empty());
        }
    }

    /// Hands the main thread a View of what the store holds, or leaves it to the next step
    /// where the main thread holds every View.
    fn publish(&mut self, mailbox: &Mailbox<View>) {
        self.release_views();
        let version = self.version + 1;
        let span = &self.span;
        let apps = &self.apps;
        let earlier = &self.earlier;
        // The payload store installs a second's Apps after it takes the second in, so its
        // stretches never add them up, and its Views hold none.
        let stretches = self.payload_budget.is_none().then_some(&self.stretches);
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
            if let Some(sampled) = sampled {
                view.sampled.extend(sampled.held().map(Arc::clone));
                assert_eq!(view.sampled.len(), view.earlier.len());
            }
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

/// The publication of the newest second of run `run` whose Apps the stretches hold once the
/// store takes `second` of that run in after `newest` with `apps`, what it found of the second's
/// Apps.
fn apps_publication(
    newest: Option<&Newest>,
    run: &RunIdentity,
    second: &MachineSecond,
    apps: &TickApps<'_>,
) -> u64 {
    let held = match newest {
        Some(newest) if newest.run == *run => newest.apps_publication.max(second.publication),
        _ => second.publication,
    };

    match apps {
        TickApps::Folded(folded) => {
            assert!(folded.publication >= second.publication);
            held.max(folded.publication)
        }
        _ => held,
    }
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
        if held.left_out.is_none() {
            held.covers_hour = true;
        }

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
    coverage::accept_catalog(held, outcome.checked_catalog)?;
    held.left_out = outcome.left_out;
    if held.left_out.is_none() {
        held.covers_hour = true;
    }
    ring.accept(outcome.terminal);

    Ok(())
}

/// The opt-in constructor uses the same producer while yielding between finite source steps.
fn startup_staged(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    operation: &Operation,
) -> persistence::Result<()> {
    use recording::staged::{Failure as StartupFailure, Input, Startup};
    let budget = Arc::clone(
        held.payload_budget
            .as_ref()
            .unwrap_or_else(|| unreachable!()),
    );
    let input = Input {
        root,
        budget: &budget,
        operation,
    };
    let mut startup = Startup::new();
    let mut blocked = None;
    held.startup = StartupState::Loading;
    for _ in 0..STEPS_MAX {
        operation.check()?;
        match startup_staged_turn(&input, held, ring, shared, &mut startup, &mut blocked) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(StartupFailure::Catalog(Failure::ChangedIdentity | Failure::ForeignRecording)) => {
                held.reset();
                held.startup = StartupState::Loading;
                *ring = make_ring(root, Some(&budget));
                startup = Startup::new();
                blocked = None;
            }
            Err(error) => return Err(startup_error(error)),
        }
    }
    unreachable!("startup finishes or the Store is cancelled");
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
        let queries = shared.queries.as_ref().unwrap_or_else(|| unreachable!());
        if queries.requested() {
            startup.revalidate(input)?;
            run_queries(held, ring, queries);
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
        Some(input.operation),
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
        sampled: held.sampled.as_ref().map_or(0, Span::free),
        newest: held.newest.map(|newest| (newest.run, newest.publication)),
    }
}

fn startup_error(error: recording::staged::Failure) -> persistence::Error {
    match error {
        recording::staged::Failure::Blocked => persistence::Error::Bound("startup admission"),
        recording::staged::Failure::Read(crate::recording::Refused::Decode(error)) => error,
        recording::staged::Failure::Read(error) => {
            persistence::Error::Io(io::Error::other(format!("{error:?}")))
        }
        recording::staged::Failure::Catalog(error) => {
            persistence::Error::Io(io::Error::other(format!("{error:?}")))
        }
    }
}

/// Steps until the store is dropped.
fn run(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    load_buffers: &mut recording::LoadBuffers,
    operation: Option<&Operation>,
) {
    let mut waited = Ok(());
    for _ in 0..STEPS_MAX {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        if let Some(queries) = &shared.queries {
            run_queries(held, ring, queries);
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
            match refresh(root, held, ring, shared, load_buffers, operation) {
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
        if run_retry(held, ring, shared) {
            waited = Ok(());
            continue;
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

fn run_queries(held: &mut Held, ring: &mut Ring, queries: &query::Queries) {
    let budget = held
        .payload_budget
        .as_ref()
        .unwrap_or_else(|| unreachable!("only the complete Store enables queries"));
    let checked = match coverage::checked(held, ring) {
        Ok(checked) => checked,
        Err(failure) => {
            held.failure = Some(failure);
            held.unpublished = true;
            None
        }
    };
    queries.serve(held, checked.as_ref(), budget);
}

fn run_retry(held: &mut Held, ring: &Ring, shared: &Shared) -> bool {
    if !ring.reload_required() {
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
    load_buffers: &mut recording::LoadBuffers,
    operation: Option<&Operation>,
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
                    *ring = make_ring(root, held.payload_budget.as_ref());
                    return Ok(true);
                }
            }
        }
    }

    refresh_load(root, held, ring, shared, load_buffers, operation)
}

/// Takes in what the recording sealed after the newest second the store holds, and follows a
/// new ring from the newest sealed minute where the recording handed any second over. An hour
/// that left a sealed minute out reads every minute again, so the refused one is retried, while
/// losing the place in the ring alone leaves the held minutes as they are. The payload store
/// reads the whole hour each time, so the Apps its sealed minutes saved replace the live ones.
fn refresh_load(
    root: &Path,
    held: &mut Held,
    ring: &mut Ring,
    shared: &Shared,
    load_buffers: &mut recording::LoadBuffers,
    operation: Option<&Operation>,
) -> persistence::Result<bool> {
    let budget = held.payload_budget.clone();
    let since = held.newest;
    let retries_hour = held.left_out.is_some();
    let mut accepted = true;
    let mut loaded = false;
    let mut checked_catalog = None;
    let mut next_ring = make_ring(root, budget.as_ref());
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
    let left_out = if let Some(budget) = &budget {
        let operation = operation.unwrap_or_else(|| unreachable!("payload reads are cancellable"));
        let outcome = recording::load_payload_hour(
            root,
            budget,
            operation,
            since.as_ref(),
            |run, second, apps| take(recording::Continuity::Replace, run, second, apps),
        )?;
        if accepted {
            next_ring.accept(outcome.terminal);
            checked_catalog = outcome.checked_catalog;
        }
        outcome.left_out
    } else {
        if retries_hour {
            recording::load_hour(root, since.as_ref(), load_buffers, take)
        } else {
            recording::load_since(root, since.as_ref(), load_buffers, take)
        }
    };
    if held.left_out != left_out {
        held.left_out = left_out;
        held.unpublished = true;
    }
    if budget.is_none() {
        if loaded {
            let operation =
                Arc::clone(&shared.cancellation).operation(shared.cancellation.epoch())?;
            next_ring.accept(recording::terminal(root, &operation)?);
        }
    }
    if accepted {
        if budget.is_some() {
            coverage::accept_catalog(held, checked_catalog)?;
        }
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
/// The return value reports native admission; the legacy path performs no native read.
fn step(
    held: &mut Held,
    ring: &mut Ring,
    waited: Result<(), Failure>,
    mailbox: &Mailbox<View>,
    operation: Option<&Operation>,
    publication: Publication,
) -> persistence::Result<NativeRead> {
    if let Some(operation) = operation {
        operation.check()?;
    }
    let mut failure = waited.err();

    let read = if held.payload_budget.is_some() {
        let operation = operation.ok_or(persistence::Error::Bound("complete Store operation"))?;
        read_native(held, ring, operation)
    } else {
        ring.read(|run, second, apps| held.take(run, second, apps))
            .map(|()| NativeRead::Idle)
    };
    let progress = match read {
        Ok(progress) => progress,
        Err(read) => {
            failure = Some(read);
            NativeRead::Idle
        }
    };

    if let Err(followed) = ring.follow() {
        failure = Some(followed);
    }

    if let Some(operation) = operation {
        operation.check()?;
    }

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
