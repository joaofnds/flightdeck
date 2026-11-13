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

use serde_json::Value;
use viewer::LiveSession;
use viewer::store::{
    QueryContext, QueryHandle, QueryNeedsLoad, QueryOutcome, QueryReadiness, QueryRequest,
    QuerySelection, QueryToken, QueryUnavailable,
};

use super::{Command, Context, Histories, Presentation, Reader, Shared, Snapshot, Update};

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
    pending: Option<(u64, QueryToken)>,
    generation: u64,
    published: u64,
    /// The page's older readings come with a later step's reading.
    tail_pending: bool,
    store_waits: u32,
}

impl StoreReader {
    pub(super) fn new(handle: QueryHandle) -> Self {
        Self {
            handle,
            context: None,
            pending: None,
            generation: 0,
            published: 0,
            tail_pending: false,
            store_waits: 0,
        }
    }

    pub(super) fn rest(&mut self) {
        if let Err(error) = self.handle.cancel() {
            eprintln!("flightdeck Store query cancellation: {error:?}");
        }
        self.context = None;
        self.pending = None;
        self.forget_page();
    }

    /// The handle whose answer ends the worker's pause, while a page shows nothing until its first
    /// reading comes. A page that shows its readings asks again at the worker's pace, however soon
    /// the Store answers.
    pub(super) fn answer_awaited(&self) -> Option<&QueryHandle> {
        if self.published == self.generation {
            return None;
        }

        Some(&self.handle)
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
        if let Some(context) = self.tail_due() {
            return payload::snapshot(context, command, shared, None).map(Answer::Ready);
        }
        if let Some(context) = &self.context {
            if detached_context_answers(context, command)? {
                return payload::snapshot(context, command, shared, None).map(Answer::Ready);
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

        payload::snapshot(context, command, shared, Some(&observation)).map(Answer::Ready)
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
            self.pending = Some((command.generation, token));
            return Ok(self.asked());
        }

        let Some(reply) = self.handle.take() else {
            return Ok(self.waited());
        };

        if self.pending != Some((command.generation, reply.token)) {
            return Ok(Answer::Waiting);
        }

        self.pending = None;
        match reply.outcome {
            QueryOutcome::Ready(context) => {
                self.store_waits = 0;
                if context.readiness() == QueryReadiness::Primary {
                    return Ok(Answer::Waiting);
                }

                self.context = Some(context);
                self.snapshot_kept_present(command, shared)
            }
            QueryOutcome::NeedsLoad(reason) => Ok(self.needs_load(reason)),
            QueryOutcome::Unavailable(reason) => self.unavailable(reason),
        }
    }

    fn snapshot_query(&mut self, command: &mut Command, shared: &Shared) -> Result<Answer, String> {
        if self.pending.is_none() {
            let selection = self.query_selection(command)?;
            if self.published == 0 {
                self.context = None;
            }
            self.snapshot_query_submit(command, selection)?;
            return Ok(self.asked());
        }
        let Some(reply) = self.handle.take() else {
            return Ok(self.waited());
        };
        if self.pending != Some((command.generation, reply.token)) {
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
                self.pending = Some((command.generation, token));
                return Ok(self.asked());
            }
        }
        let snapshot = payload::snapshot(&context, command, shared, None)?;
        self.context = Some(context);

        Ok(Answer::Ready(snapshot))
    }

    /// What a step that asks the Store does. While the page shows nothing yet, the answer ends the
    /// pause after it, so the step counts nothing, and that answer may be the one that says why
    /// the page cannot be read. A page that shows its readings waits out the step for the answer,
    /// so the step counts toward the bound and the wait lasts as long as the first page's.
    fn asked(&mut self) -> Answer {
        if self.answer_awaited().is_some() {
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
            QuerySelection::FollowLatest => None,
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
        self.pending = Some((command.generation, token));
        if let Some(handle) = self.answer_awaited() {
            handle.wake_store();
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

/// Whether the whole context of a second the Store's span has left answers a read of that second.
fn detached_context_answers(context: &QueryContext, command: &Command) -> Result<bool, String> {
    if command.request.second != Some(context.selected_second()) {
        return Ok(false);
    }
    if context.readiness() != QueryReadiness::Complete {
        return Ok(false);
    }
    if !context.selected().detached() {
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
        Ok(Answer::Saved) => match read_saved(reader, root, command, shared) {
            Ok(snapshot) => {
                // What the recording saves comes a minute apart, so a page it answered asks again
                // a second later rather than at the worker's pace, unless part of the page is due.
                if !super::reader_pending(reader, command.generation) {
                    reader.retry_after = Instant::now() + super::RETRY_INTERVAL;
                }
                snapshot.map(Update::Ready)
            }
            Err(message) => Some(super::reader_failed(reader, command, message)),
        },
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
    // step reads the rest.
    let tail_pending = reader
        .store
        .as_ref()
        .is_some_and(|store| store.tail_pending);
    let plan = super::reader_snapshot_plan(reader, &command.request.route);
    let tail_at_once = plan.tail_at_once && !tail_pending;
    let selection = std::mem::replace(&mut snapshot.selection, Presentation::Legacy(Value::Null));
    let mut pending = false;
    snapshot.selection = selection.edited(|selection| {
        pending = read_earlier_selection(reader, root, command, shared, tail_at_once, selection);
    });
    if let Some(store) = &mut reader.store {
        store.tail_pending = pending;
    }

    pending
}

pub(super) fn read_earlier_selection(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &Command,
    shared: &Shared,
    tail_at_once: bool,
    selection: &mut Value,
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
        Ok(session) => super::reader_earlier(session, &command.request, tail_at_once, selection),
        Err(message) => {
            if let Some(fields) = selection.as_object_mut() {
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
