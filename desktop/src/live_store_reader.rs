//! The live Worker presents already-ingested immutable Store facts.
#[path = "live_store_reader/ownership.rs"]
pub mod ownership;
#[path = "live_store_reader/payload.rs"]
mod payload;
#[path = "live_store_reader/presentation.rs"]
pub mod presentation;
#[cfg(test)]
#[path = "live_store_reader/tests.rs"]
pub(super) mod tests;

use std::sync::Arc;
use std::time::Instant;

use viewer::LiveSession;
use viewer::store::{
    ChartPoints, QueryContext, QueryHandle, QueryKey, QueryNeedsLoad, QueryOutcome, QueryReadiness,
    QueryRequest, QuerySelection, QueryToken, QueryUnavailable, RankedSeconds,
};

use super::{Command, Context, Histories, PointsKeeping, Reader, Shared, Snapshot, Update};

/// Steps of the worker, ten seconds of them at its pace, a page waits for the Store to answer it.
/// Past them the saved minutes answer, or the limit on contexts says so, since a Store that cannot
/// prove what it holds or has no recording to read would otherwise hold the page for ever.
const STORE_WAITS_MAX: u32 = 40;
const _: () = assert!(
    super::POLL_INTERVAL
        .saturating_mul(STORE_WAITS_MAX)
        .as_millis()
        == 10_000
);
/// Steps, five seconds of them, that a page in the steady state waits on its keyed question
/// before it asks a plain one, which a Store that serves answers at once, so a page whose Store
/// stops serving still reaches `STORE_WAITS_MAX` and the saved minutes.
const STORE_WAITS_KEYED_MAX: u32 = STORE_WAITS_MAX / 2;
const _: () = {
    assert!(STORE_WAITS_KEYED_MAX > 0);
    assert!(STORE_WAITS_KEYED_MAX * 2 == STORE_WAITS_MAX);
};

/// What one step of the reader found.
enum Answer {
    /// The Store has not answered, or its answer changes nothing the page shows.
    Waiting,
    Ready(Box<Snapshot>),
    /// The Store does not hold what the page asks for, so the saved recording answers.
    Saved,
}

pub(super) struct StoreReader {
    handle: QueryHandle,
    context: Option<Arc<QueryContext>>,
    pending: Option<Question>,
    /// The key of the last answer the page shows, or of a repeat of it the reader took since, which
    /// a keyed question names. A new page, a rest and an answer that is not a reading clear it. It
    /// is kept apart from the context the page shows, since a repeat answer is dropped and leaves
    /// that context, whose key is older.
    taken_key: Option<QueryKey>,
    generation: u64,
    published: u64,
    /// The page's older readings come with a later step's reading.
    tail_pending: bool,
    store_waits: u32,
    reused: Reused,
}

/// A question the reader put to the Store whose answer it has not taken.
#[derive(Clone, Copy)]
struct Question {
    generation: u64,
    token: QueryToken,
    /// Whether the question names the answer the page shows, which the Store keeps until it holds
    /// something new.
    keyed: bool,
}

impl Question {
    /// Whether the answer `token` to a question of the page of `generation` answers this one.
    fn answered_by(self, generation: u64, token: QueryToken) -> bool {
        if self.generation != generation {
            return false;
        }

        self.token == token
    }
}

/// What a reading of the Store leaves that the next reading shares, whichever page asks.
#[derive(Default)]
struct Reused {
    /// The top Apps of each second the reader last ranked, which the next reading that ranks
    /// them shares: the pages that show them rank the same Apps. A rest, a page that shows none
    /// and a selected page the saved minutes answer let go of them.
    ranked: RankedSeconds,
    /// The chart points of the seconds the reader last charted, which the next reading that
    /// draws a chart shares: every page that draws one draws the same points. A rest, a page
    /// that draws none and a selected page the saved minutes answer let go of them.
    chart_points: ChartPoints,
}

impl StoreReader {
    pub(super) fn new(handle: QueryHandle) -> Self {
        Self {
            handle,
            context: None,
            pending: None,
            taken_key: None,
            generation: 0,
            published: 0,
            tail_pending: false,
            store_waits: 0,
            reused: Reused::default(),
        }
    }

    pub(super) fn rest(&mut self) {
        if let Err(error) = self.handle.cancel() {
            eprintln!("flightdeck Store query cancellation: {error:?}");
        }
        self.context = None;
        self.pending = None;
        self.reused = Reused::default();
        self.forget_page();
    }

    /// The handle whose answer ends the worker's pause: while a page shows nothing until its first
    /// reading comes, and while a page in the steady state asks a keyed question, which the Store
    /// answers once it holds something new. Any other question of a page that shows its readings
    /// is looked for at the worker's pace, however soon the Store answers.
    pub(super) fn answer_awaited(&self) -> Option<&QueryHandle> {
        if self.first_reading_awaited() {
            return Some(&self.handle);
        }
        if self.keyed_question().is_some() {
            return Some(&self.handle);
        }

        None
    }

    /// The token of the keyed question the reader has out, which the Store answers once it holds
    /// something new.
    pub(super) fn keyed_question(&self) -> Option<QueryToken> {
        let question = self.pending?;
        if question.keyed {
            return Some(question.token);
        }

        None
    }

    /// Whether the reading of the Store's next second is on its way to the page of `command`: a
    /// keyed question is out, which the Store answers once it takes that second, and the page
    /// shows a reading of it.
    pub(super) fn reading_on_its_way(&self, command: &Command) -> bool {
        if self.keyed_question().is_some() {
            return !super::route_shows_no_reading(&command.request.route);
        }

        false
    }

    /// Whether the page shows nothing until its first reading comes.
    fn first_reading_awaited(&self) -> bool {
        self.published != self.generation
    }

    /// Whether the Store published a page of `generation`.
    pub(super) fn published(&self, generation: u64) -> bool {
        self.published == generation
    }

    /// Whether the page's next step goes on with no wait: to the rest of a page the saved minutes
    /// answered, since a query of that page's ends the pause when answered, or to the older tail
    /// of a page the Store answered whole, which this thread reads with nothing to wait for.
    pub(super) fn reads_on(&self) -> bool {
        if self.tail_due().is_some() {
            return true;
        }

        self.published != self.generation && self.pending.is_none()
    }

    /// The context whose page reads the older tail it left at its next step, since the Store has
    /// answered the rest of it.
    fn tail_due(&self) -> Option<&Arc<QueryContext>> {
        self.context
            .as_ref()
            .filter(|context| self.tail_pending && context.readiness() == QueryReadiness::Complete)
    }

    /// What the reader knows of the page it read, which a new page or a rest starts over.
    fn forget_page(&mut self) {
        self.taken_key = None;
        self.published = 0;
        self.tail_pending = false;
        self.store_waits = 0;
    }

    fn snapshot(&mut self, command: &mut Command, shared: &Shared) -> Result<Answer, String> {
        if shared.cancellation.epoch() != command.epoch {
            #[cfg(test)]
            super::measurement::abandoned(shared, command.generation);
            self.handle
                .cancel()
                .map_err(|error| format!("Store query: {error:?}"))?;
            self.pending = None;
            return Ok(Answer::Waiting);
        }
        if self.generation != command.generation {
            self.handle
                .cancel()
                .map_err(|error| format!("Store query: {error:?}"))?;
            self.pending = None;
            self.generation = command.generation;
            self.forget_page();
        }
        if self.published == command.generation {
            if self.snapshot_published_stands(command, shared)? {
                return Ok(Answer::Waiting);
            }
        }
        if command.context == Context::Keep {
            return self.snapshot_kept(command, shared);
        }
        if let Some(context) = self.tail_due().cloned() {
            return payload::snapshot(&context, command, shared, None, &mut self.reused)
                .map(Answer::Ready);
        }
        if let Some(context) = &self.context {
            if fixed_context_answers(context, command)? {
                return payload::snapshot(context, command, shared, None, &mut self.reused)
                    .map(Answer::Ready);
            }
        }

        self.snapshot_query(command, shared)
    }

    /// Whether what this request last published still answers it: a held second's whole context
    /// on a page that reads that second, or a reading the window has not taken yet, unless the page
    /// reads the older tail it left next.
    fn snapshot_published_stands(
        &self,
        command: &Command,
        shared: &Shared,
    ) -> Result<bool, String> {
        if self.tail_due().is_some() {
            return Ok(false);
        }
        let complete = self
            .context
            .as_ref()
            .is_some_and(|context| context.readiness() == QueryReadiness::Complete);
        if command.request.second.is_some() {
            if !self.tail_pending {
                if complete {
                    if !super::route_shows_no_reading(&command.request.route) {
                        return Ok(true);
                    }
                }
            }
        }
        let mailbox = shared.mailbox.lock().map_err(|error| error.to_string())?;

        Ok(mailbox.output.is_some())
    }

    fn snapshot_kept(&mut self, command: &mut Command, shared: &Shared) -> Result<Answer, String> {
        let Some(context) = &self.context else {
            return self.snapshot_query(command, shared);
        };
        if context.readiness() == QueryReadiness::Primary {
            return self.snapshot_kept_refresh(command, shared);
        }

        self.snapshot_kept_present(command, shared)
    }

    fn snapshot_kept_present(
        &mut self,
        command: &mut Command,
        shared: &Shared,
    ) -> Result<Answer, String> {
        let context = self
            .context
            .as_ref()
            .ok_or("No Store context is available to inspect")?;
        let second = command
            .request
            .second
            .ok_or("A kept Store read needs its second")?;
        let identity = command
            .request
            .identity
            .as_ref()
            .ok_or("A kept Store read needs identity")?;
        if !context.retains_second(second) {
            return Ok(Answer::Saved);
        }
        let observation = context
            .observation_for_identity(second, identity)
            .map_err(|error| error.to_string())?;

        payload::snapshot(
            context,
            command,
            shared,
            Some(&observation),
            &mut self.reused,
        )
        .map(Answer::Ready)
    }

    fn snapshot_kept_refresh(
        &mut self,
        command: &mut Command,
        shared: &Shared,
    ) -> Result<Answer, String> {
        let context = self.context.as_ref().ok_or("Missing Store context")?;
        if self.pending.is_none() {
            let token = self
                .handle
                .submit(QueryRequest {
                    epoch: command.epoch,
                    source: Some(context.source),
                    selection: QuerySelection::Exact {
                        observation: context.selected().observation,
                        context_last_second: context.context_last_second(),
                    },
                })
                .map_err(|error| format!("Store kept query: {error:?}"))?;
            self.pending = Some(Question {
                generation: command.generation,
                token,
                keyed: false,
            });
            return Ok(self.asked());
        }

        let Some(reply) = self.handle.take() else {
            return Ok(self.waited());
        };

        if !self
            .pending
            .is_some_and(|question| question.answered_by(command.generation, reply.token))
        {
            return Ok(Answer::Waiting);
        }

        self.pending = None;
        match reply.outcome {
            QueryOutcome::Ready(context) => {
                self.store_waits = 0;
                if context.readiness() == QueryReadiness::Primary {
                    return Ok(Answer::Waiting);
                }

                self.taken_key = Some(context.key());
                self.context = Some(context);
                self.snapshot_kept_present(command, shared)
            }
            QueryOutcome::NeedsLoad(reason) => Ok(self.needs_load(reason)),
            QueryOutcome::Unavailable(reason) => self.unavailable(reason),
        }
    }

    fn snapshot_query(&mut self, command: &mut Command, shared: &Shared) -> Result<Answer, String> {
        if self.pending.is_none() {
            let selection = self.snapshot_query_selection(command)?;
            if self.published == 0 {
                self.context = None;
            }
            self.snapshot_query_submit(command, selection)?;
            return Ok(self.asked());
        }
        let Some(reply) = self.handle.take() else {
            return self.snapshot_query_waited(command);
        };
        if !self
            .pending
            .is_some_and(|question| question.answered_by(command.generation, reply.token))
        {
            return Ok(Answer::Waiting);
        }
        self.pending = None;
        let context = match reply.outcome {
            QueryOutcome::Ready(context) => context,
            QueryOutcome::NeedsLoad(reason) => return Ok(self.needs_load(reason)),
            QueryOutcome::Unavailable(reason) => return self.unavailable(reason),
        };
        self.store_waits = 0;
        if self.published == command.generation {
            if self.repeats_selection(&context) {
                self.taken_key = Some(context.key());
                self.snapshot_query_repeated(command)?;
                return Ok(Answer::Waiting);
            }
        }
        if let Some(identity) = &command.request.identity {
            let second = command
                .request
                .second
                .ok_or("Selected Store identity has no second")?;
            let observation = context
                .observation_for_identity(second, identity)
                .map_err(|error| error.to_string())?;
            if observation != context.selected().observation {
                let source = context.source;
                let context_last_second = context.context_last_second();
                drop(context);
                let token = self
                    .handle
                    .submit(QueryRequest {
                        epoch: command.epoch,
                        source: Some(source),
                        selection: QuerySelection::Exact {
                            observation,
                            context_last_second,
                        },
                    })
                    .map_err(|error| format!("Store exact query: {error:?}"))?;
                self.pending = Some(Question {
                    generation: command.generation,
                    token,
                    keyed: false,
                });
                return Ok(self.asked());
            }
        }
        let snapshot = payload::snapshot(&context, command, shared, None, &mut self.reused)?;
        self.taken_key = Some(context.key());
        self.context = Some(context);

        Ok(Answer::Ready(snapshot))
    }

    /// What a step whose question the Store has not answered does: it waits, up to the bound, and
    /// half the bound into the wait replaces a keyed question with a plain one, which a Store that
    /// serves answers at its next serve, whatever it holds.
    fn snapshot_query_waited(&mut self, command: &Command) -> Result<Answer, String> {
        let waited = self.waited();
        if self.keyed_question().is_some() {
            if self.store_waits >= STORE_WAITS_KEYED_MAX {
                let selection = self.snapshot_query_selection(command)?;
                self.snapshot_query_submit(command, selection)?;
            }
        }

        Ok(waited)
    }

    /// What a step whose answer repeats what the page shows does: it shows nothing new, and a page
    /// in the steady state puts its next keyed question out at once, naming that answer, so the
    /// Store keeps it until it holds something new. Any other page asks again at its next step.
    fn snapshot_query_repeated(&mut self, command: &Command) -> Result<(), String> {
        let selection = self.snapshot_query_selection(command)?;
        if let QuerySelection::FollowLatestAfter(_) = selection {
            self.snapshot_query_submit(command, selection)?;
        }

        Ok(())
    }

    /// The next question the page of `command` asks the Store, keyed in the steady state.
    fn snapshot_query_selection(&self, command: &Command) -> Result<QuerySelection, String> {
        Ok(self.query_keyed(command, self.query_selection(command)?))
    }

    /// What a step that asks the Store does. While the page shows nothing yet, the answer ends the
    /// pause after it, so the step counts nothing, and that answer may be the one that says why
    /// the page cannot be read. A page that shows its readings counts the step toward the bound,
    /// whether its question waits for the worker's pace or for the Store to hold something new.
    fn asked(&mut self) -> Answer {
        if self.first_reading_awaited() {
            return Answer::Waiting;
        }

        self.waited()
    }

    /// What a step that has no answer from the Store does: it waits, up to the bound.
    fn waited(&mut self) -> Answer {
        if self.store_waits < STORE_WAITS_MAX {
            self.store_waits += 1;
            return Answer::Waiting;
        }

        Answer::Saved
    }

    /// What a step does with a query the Store could not yet or cannot answer from what it holds.
    fn needs_load(&mut self, reason: QueryNeedsLoad) -> Answer {
        // An answer that is not a reading leaves the page with no key, so its next question is
        // plain, which the Store answers at its next serve.
        self.taken_key = None;
        match reason {
            QueryNeedsLoad::AppsPending
            | QueryNeedsLoad::UncheckedCoverage
            | QueryNeedsLoad::IncompleteRing => self.waited(),
            QueryNeedsLoad::SourceChanged => {
                self.context = None;
                self.waited()
            }
            QueryNeedsLoad::OutsideCoverage
            | QueryNeedsLoad::FullDetailEvicted
            | QueryNeedsLoad::MissingObservation => Answer::Saved,
        }
    }

    /// The window lets go of the context it shows once it takes the reading the reader published,
    /// so a limit that lasts means something else keeps a context.
    fn unavailable(&mut self, reason: QueryUnavailable) -> Result<Answer, String> {
        // As in `needs_load`, the next question asks with no key.
        self.taken_key = None;
        match reason {
            QueryUnavailable::NoReadings | QueryUnavailable::NoObservation => Ok(Answer::Saved),
            QueryUnavailable::ContextLimit if self.store_waits < STORE_WAITS_MAX => {
                self.store_waits += 1;
                Ok(Answer::Waiting)
            }
            reason => Err(format!("Store readings unavailable: {reason:?}")),
        }
    }

    fn snapshot_query_submit(
        &mut self,
        command: &Command,
        selection: QuerySelection,
    ) -> Result<(), String> {
        let source = match selection {
            QuerySelection::FollowLatest | QuerySelection::FollowLatestAfter(_) => None,
            QuerySelection::Second { .. } | QuerySelection::Exact { .. } => {
                self.context.as_ref().map(|context| context.source)
            }
        };
        let token = self
            .handle
            .submit(QueryRequest {
                epoch: command.epoch,
                source,
                selection,
            })
            .map_err(|error| format!("Store query: {error:?}"))?;
        self.pending = Some(Question {
            generation: command.generation,
            token,
            keyed: matches!(selection, QuerySelection::FollowLatestAfter(_)),
        });
        // Only a page that shows nothing yet wakes the Store's wait on its ring, since that wake
        // reaches every reader of the ring. A keyed question waits for the Store's own turn.
        if self.first_reading_awaited() {
            self.handle.wake_store();
        }

        Ok(())
    }

    fn repeats_selection(&self, context: &QueryContext) -> bool {
        let Some(previous) = &self.context else {
            return false;
        };
        if previous.source != context.source {
            return false;
        }
        if previous.readiness() != context.readiness() {
            return false;
        }
        if previous.context_last_second() != context.context_last_second() {
            return false;
        }

        previous.selected().observation == context.selected().observation
    }

    /// The question a page asks with `selection`: in the steady state, where the page shows a
    /// complete reading of the Store and follows the newest second, a follow question names the
    /// answer the reader took last, so the Store keeps it until it holds something new. Before
    /// that, as at launch, and half the bound into a wait, it asks with no key, so the Store
    /// answers at its next serve.
    fn query_keyed(&self, command: &Command, selection: QuerySelection) -> QuerySelection {
        if selection != QuerySelection::FollowLatest {
            return selection;
        }
        if self.first_reading_awaited() {
            return selection;
        }
        let complete = self
            .context
            .as_ref()
            .is_some_and(|context| context.readiness() == QueryReadiness::Complete);
        if !complete {
            return selection;
        }
        if self.store_waits >= STORE_WAITS_KEYED_MAX {
            return selection;
        }
        assert_eq!(self.published, command.generation);

        self.taken_key
            .map_or(selection, QuerySelection::FollowLatestAfter)
    }

    fn query_selection(&self, command: &Command) -> Result<QuerySelection, String> {
        if super::route_shows_no_reading(&command.request.route) {
            return Ok(QuerySelection::FollowLatest);
        }
        let Some(second) = command.request.second else {
            return Ok(QuerySelection::FollowLatest);
        };
        if let Some(context) = &self.context {
            if second == context.selected_second() {
                let observation = match &command.request.identity {
                    Some(identity) => context
                        .observation_for_identity(second, identity)
                        .map_err(|error| error.to_string())?,
                    None => context.selected().observation,
                };
                return Ok(QuerySelection::Exact {
                    observation,
                    context_last_second: context.context_last_second(),
                });
            }
        }

        Ok(QuerySelection::Second {
            second,
            alternative: 0,
        })
    }
}

/// Whether the whole context of a fixed second answers a read of that second, which it does
/// whether or not the Store's span still holds the second.
fn fixed_context_answers(context: &QueryContext, command: &Command) -> Result<bool, String> {
    if command.request.second != Some(context.selected_second()) {
        return Ok(false);
    }
    if context.readiness() != QueryReadiness::Complete {
        return Ok(false);
    }
    if !context.selected().fixed() {
        return Ok(false);
    }
    let Some(identity) = &command.request.identity else {
        return Ok(true);
    };
    let observation = context
        .observation_for_identity(context.selected_second(), identity)
        .map_err(|error| error.to_string())?;

    Ok(observation == context.selected().observation)
}

pub(super) fn read(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &mut Command,
    shared: &Shared,
) -> Option<Update> {
    if Instant::now() < reader.retry_after {
        return None;
    }
    let started = Instant::now();
    #[cfg(test)]
    super::measurement::checkpoint(
        shared,
        command.generation,
        super::measurement::Stage::Reading,
    );
    let store = reader
        .store
        .as_mut()
        .unwrap_or_else(|| unreachable!("Store reader branch"));
    match store.snapshot(command, shared) {
        Ok(Answer::Ready(mut snapshot)) => {
            store.published = command.generation;
            snapshot.catalog["path"] = serde_json::json!(root);
            let histories = if snapshot.histories_pending {
                Histories::Defer
            } else {
                Histories::Read
            };
            if read_earlier(reader, root, command, shared, &mut snapshot) {
                snapshot.histories_pending = true;
            }
            // A hover leaves the page it is over to whoever answered that page.
            if command.context == Context::Select {
                super::reader_page_caches_unread(reader, started);
            }
            snapshot.read_ms = started.elapsed().as_secs_f64() * 1000.0;
            super::reader_snapshot_record(reader, command, &snapshot, histories);
            Some(Update::Ready(snapshot))
        }
        Ok(Answer::Waiting) => None,
        Ok(Answer::Saved) => {
            // The saved minutes rank a selected page's Apps and draw its charts, so nothing shows
            // the Store's ranks or points. A hover's reply goes to the hover and leaves the
            // page's ranks and charts, and so what the reader reuses, the Store's.
            if command.context == Context::Select {
                store.reused.ranked = RankedSeconds::default();
                store.reused.chart_points = ChartPoints::default();
            }
            match read_saved(reader, root, command, shared) {
                Ok(snapshot) => {
                    // What the recording saves comes a minute apart, so a page it answered asks
                    // again a second later rather than at the worker's pace, unless part of the
                    // page is due.
                    if !super::reader_pending(reader, command.generation) {
                        reader.retry_after = Instant::now() + super::RETRY_INTERVAL;
                    }
                    snapshot.map(Update::Ready)
                }
                Err(message) => Some(super::reader_failed(reader, command, message)),
            }
        }
        Err(message) => Some(super::reader_failed(reader, command, message)),
    }
}

/// Answers from the saved recording, since the Store alone takes in the live Mac's readings.
fn read_saved(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &mut Command,
    shared: &Shared,
) -> Result<Option<Box<Snapshot>>, String> {
    let scope =
        persistence::work::ingestion::purpose(persistence::work::ingestion::Purpose::History);
    read_saved_open(reader, root, command, shared)?;
    let snapshot = super::reader_snapshot(reader, root, command, shared);
    drop(scope);

    snapshot
}

fn read_saved_open(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &Command,
    shared: &Shared,
) -> Result<(), String> {
    if reader.session.is_none() {
        let session = LiveSession::open_with(root, Arc::clone(&shared.cancellation), command.epoch)
            .map_err(|error| error.to_string())?;
        reader.session = Some(session);
    }

    Ok(())
}

/// Adds the saved readings older than the Store's context to the page's selection, and returns
/// whether the page still waits for them. They read from saved minutes, which the Store leaves
/// to this thread so a slow read holds up only this page.
fn read_earlier(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &Command,
    shared: &Shared,
    snapshot: &mut Snapshot,
) -> bool {
    if super::reader_earlier_skips(&command.request) {
        return false;
    }
    // A page's first step takes the older tail where its session reads it at once, and its next
    // step reads the rest. A reading that left the tail to the View, which draws every chart of
    // the page over the seconds the store holds, reads none of it and leaves none to a later
    // step.
    let pending = if snapshot.points_keeping == PointsKeeping::TailLeftToView {
        false
    } else {
        read_earlier_tail(reader, root, command, shared, snapshot)
    };
    if let Some(store) = &mut reader.store {
        store.tail_pending = pending;
    }

    pending
}

/// Reads what `read_earlier` takes of the older tail into `snapshot`, and returns whether the
/// page still waits for the rest of it.
fn read_earlier_tail(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &Command,
    shared: &Shared,
    snapshot: &mut Snapshot,
) -> bool {
    let tail_pending = reader
        .store
        .as_ref()
        .is_some_and(|store| store.tail_pending);
    let plan = super::reader_snapshot_plan(reader, &command.request.route);
    let tail_at_once = plan.tail_at_once && !tail_pending;
    let mut pending = false;
    snapshot.selection.edit(|selection| {
        pending = read_earlier_selection(reader, root, command, shared, tail_at_once, selection);
    });

    pending
}

pub(super) fn read_earlier_selection(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &Command,
    shared: &Shared,
    tail_at_once: bool,
    selection: &mut viewer::Selection,
) -> bool {
    let scope =
        persistence::work::ingestion::purpose(persistence::work::ingestion::Purpose::History);
    let last = crate::live_projection::context_last_second(selection);
    // Listing the saved minutes reopens the recording, which took 42 to 90 ms on an hour of them,
    // so a newly opened page shows with the minutes its session listed and the worker lists them
    // once the step is over.
    if tail_at_once {
        reader.deferred_listing_last_second = last;
    }
    let session = read_saved_open(reader, root, command, shared).and_then(|()| {
        let session = super::reader_snapshot_session(
            &mut reader.session,
            root,
            command,
            shared,
            super::Arrivals::Leave,
        )?;
        if !tail_at_once && let Some(last) = last {
            session
                .list_saved_for(last)
                .map_err(|error| error.to_string())?;
        }

        Ok(session)
    });
    let pending = match session {
        Ok(session) => super::reader_earlier(
            session,
            &mut reader.earlier_tail_unread_since,
            &command.request,
            tail_at_once,
            selection,
        ),
        Err(message) => {
            if let Some(mut fields) = selection.fields_mut() {
                fields.insert("earlier_error".into(), message.into());
            }

            false
        }
    };
    drop(scope);

    pending
}

impl Drop for StoreReader {
    fn drop(&mut self) {
        match self.handle.cancel() {
            Ok(()) | Err(viewer::store::QuerySubmitError::Closed) => {}
            Err(error) => eprintln!("flightdeck Store query shutdown: {error:?}"),
        }
    }
}
