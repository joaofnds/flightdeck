//! The live Worker presents already-ingested immutable Store facts.
#[path = "live_store_reader/ownership.rs"]
pub mod ownership;
#[path = "live_store_reader/payload.rs"]
mod payload;
#[path = "live_store_reader/presentation.rs"]
pub mod presentation;
#[cfg(test)]
#[path = "live_store_reader/tests.rs"]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use viewer::LiveSession;
use viewer::store::{
    QueryContext, QueryHandle, QueryNeedsLoad, QueryOutcome, QueryReadiness, QueryRequest,
    QuerySelection, QueryToken, QueryUnavailable,
};

use super::{Command, Context, Earlier, Histories, Presentation, Reader, Shared, Snapshot, Update};

/// Steps of the worker a query may meet the Store's limit on contexts before the page says so.
const CONTEXT_WAITS_MAX: u32 = 20;

/// What one step of the reader found.
enum Reading {
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
    context_waits: u32,
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
            context_waits: 0,
        }
    }

    pub(super) fn rest(&mut self) {
        if let Err(error) = self.handle.cancel() {
            eprintln!("flightdeck Store query cancellation: {error:?}");
        }
        self.context = None;
        self.pending = None;
        self.published = 0;
        self.tail_pending = false;
        self.context_waits = 0;
    }

    fn snapshot(&mut self, command: &mut Command, shared: &Shared) -> Result<Reading, String> {
        if shared.cancellation.epoch() != command.epoch {
            #[cfg(test)]
            super::measurement::abandoned(shared, command.generation);
            self.handle
                .cancel()
                .map_err(|error| format!("Store query: {error:?}"))?;
            self.pending = None;
            return Ok(Reading::Waiting);
        }
        if self.generation != command.generation {
            self.handle
                .cancel()
                .map_err(|error| format!("Store query: {error:?}"))?;
            self.pending = None;
            self.generation = command.generation;
            self.published = 0;
            self.tail_pending = false;
            self.context_waits = 0;
        }
        if self.published == command.generation {
            if self.snapshot_published_stands(command, shared)? {
                return Ok(Reading::Waiting);
            }
        }
        if command.context == Context::Keep {
            return self.snapshot_kept(command, shared);
        }
        if self.tail_pending {
            if let Some(context) = &self.context {
                if context.readiness() == QueryReadiness::Complete {
                    return payload::snapshot(context, command, shared, None).map(Reading::Ready);
                }
            }
        }
        if let Some(context) = &self.context {
            if command.request.second == Some(context.selected_second()) {
                if context.readiness() == QueryReadiness::Complete {
                    if !context.selected().detached() {
                        return self.snapshot_query(command, shared);
                    }
                    let identity_matches = match &command.request.identity {
                        Some(identity) => {
                            context
                                .observation_for_identity(context.selected_second(), identity)
                                .map_err(|error| error.to_string())?
                                == context.selected().observation
                        }
                        None => true,
                    };
                    if identity_matches {
                        return payload::snapshot(context, command, shared, None)
                            .map(Reading::Ready);
                    }
                }
            }
        }

        self.snapshot_query(command, shared)
    }

    /// Whether what this request last published still answers it: a held second's whole context
    /// on a page that reads that second, or a reading the window has not taken yet.
    fn snapshot_published_stands(
        &self,
        command: &Command,
        shared: &Shared,
    ) -> Result<bool, String> {
        let complete = self
            .context
            .as_ref()
            .is_some_and(|context| context.readiness() == QueryReadiness::Complete);
        if command.request.second.is_some() {
            if !self.tail_pending {
                if complete {
                    if !route_reads_the_newest(&command.request.route) {
                        return Ok(true);
                    }
                }
            }
        }
        let mailbox = shared.mailbox.lock().map_err(|error| error.to_string())?;

        Ok(mailbox.output.is_some())
    }

    fn snapshot_kept(&mut self, command: &mut Command, shared: &Shared) -> Result<Reading, String> {
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
    ) -> Result<Reading, String> {
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
            return Ok(Reading::Saved);
        }
        let observation = context
            .observation_for_identity(second, identity)
            .map_err(|error| error.to_string())?;

        payload::snapshot(context, command, shared, Some(&observation)).map(Reading::Ready)
    }

    fn snapshot_kept_refresh(
        &mut self,
        command: &mut Command,
        shared: &Shared,
    ) -> Result<Reading, String> {
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
            return Ok(Reading::Waiting);
        }

        let Some(reply) = self.handle.take() else {
            return Ok(Reading::Waiting);
        };

        if self.pending != Some((command.generation, reply.token)) {
            return Ok(Reading::Waiting);
        }

        self.pending = None;
        match reply.outcome {
            QueryOutcome::Ready(context) => {
                self.context_waits = 0;
                if context.readiness() == QueryReadiness::Primary {
                    return Ok(Reading::Waiting);
                }

                self.context = Some(context);
                self.snapshot_kept_present(command, shared)
            }
            QueryOutcome::NeedsLoad(reason) => Ok(self.needs_load(reason)),
            QueryOutcome::Unavailable(reason) => self.unavailable(reason),
        }
    }

    fn snapshot_query(
        &mut self,
        command: &mut Command,
        shared: &Shared,
    ) -> Result<Reading, String> {
        if self.pending.is_none() {
            let selection = self.query_selection(command)?;
            if self.published == 0 {
                self.context = None;
            }
            self.snapshot_query_submit(command, selection)?;
            return Ok(Reading::Waiting);
        }
        let Some(reply) = self.handle.take() else {
            return Ok(Reading::Waiting);
        };
        if self.pending != Some((command.generation, reply.token)) {
            return Ok(Reading::Waiting);
        }
        self.pending = None;
        let context = match reply.outcome {
            QueryOutcome::Ready(context) => context,
            QueryOutcome::NeedsLoad(reason) => return Ok(self.needs_load(reason)),
            QueryOutcome::Unavailable(reason) => return self.unavailable(reason),
        };
        self.context_waits = 0;
        if self.published == command.generation {
            if self.repeats_selection(&context) {
                return Ok(Reading::Waiting);
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
                return Ok(Reading::Waiting);
            }
        }
        let snapshot = payload::snapshot(&context, command, shared, None)?;
        self.context = Some(context);

        Ok(Reading::Ready(snapshot))
    }

    /// What a step does with a query the Store could not yet or cannot answer from what it holds.
    fn needs_load(&mut self, reason: QueryNeedsLoad) -> Reading {
        match reason {
            QueryNeedsLoad::AppsPending
            | QueryNeedsLoad::UncheckedCoverage
            | QueryNeedsLoad::IncompleteRing => Reading::Waiting,
            QueryNeedsLoad::SourceChanged => {
                self.context = None;
                Reading::Waiting
            }
            QueryNeedsLoad::OutsideCoverage
            | QueryNeedsLoad::FullDetailEvicted
            | QueryNeedsLoad::MissingObservation => Reading::Saved,
        }
    }

    /// The window lets go of the context it shows once it takes the reading the reader published,
    /// so a limit that lasts means something else keeps a context.
    fn unavailable(&mut self, reason: QueryUnavailable) -> Result<Reading, String> {
        match reason {
            QueryUnavailable::NoReadings | QueryUnavailable::NoObservation => Ok(Reading::Saved),
            QueryUnavailable::ContextLimit if self.context_waits < CONTEXT_WAITS_MAX => {
                self.context_waits += 1;
                Ok(Reading::Waiting)
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
        if route_reads_the_newest(&command.request.route) {
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

/// Whether the page shows the newest reading even while the user holds a second, as Computer and
/// Self-monitoring do, which hold no second of their own.
fn route_reads_the_newest(route: &str) -> bool {
    matches!(route, "computer" | "self")
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
        Ok(Reading::Ready(mut snapshot)) => {
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
            snapshot.read_ms = started.elapsed().as_secs_f64() * 1000.0;
            super::reader_snapshot_record(reader, command, &snapshot, histories);
            Some(Update::Ready(snapshot))
        }
        Ok(Reading::Waiting) => None,
        Ok(Reading::Saved) => match read_saved(reader, root, command, shared) {
            Ok(snapshot) => snapshot.map(Update::Ready),
            Err(message) => Some(read_failed(reader, command, message)),
        },
        Err(message) => Some(read_failed(reader, command, message)),
    }
}

fn read_failed(reader: &mut Reader, command: &Command, message: String) -> Update {
    reader.retry_after = Instant::now() + super::RETRY_INTERVAL;
    reader.last_generation = 0;
    Update::Failed {
        generation: command.generation,
        message,
    }
}

/// Answers from the saved recording, through a session that reads no ring, since the Store alone
/// takes in the live Mac's readings.
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
        let session =
            LiveSession::open_history_with(root, Arc::clone(&shared.cancellation), command.epoch)
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
    if command.request.earlier == Earlier::Skip {
        return false;
    }
    if command.request.route == "apps" {
        return false;
    }
    // A page's first step reads what the saved minutes keep, and its next step reads the rest.
    let tail_pending = reader
        .store
        .as_ref()
        .is_some_and(|store| store.tail_pending);
    let plan = super::reader_snapshot_plan(reader, &command.request.route);
    let kept_tail_only = plan.kept_tail_only && !tail_pending;
    let selection = std::mem::replace(&mut snapshot.selection, Presentation::Legacy(Value::Null));
    let mut pending = false;
    snapshot.selection = selection.edited(|selection| {
        pending = read_earlier_selection(reader, root, command, shared, kept_tail_only, selection);
    });
    if let Some(store) = &mut reader.store {
        store.tail_pending = pending;
    }

    pending
}

fn read_earlier_selection(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &Command,
    shared: &Shared,
    kept_tail_only: bool,
    selection: &mut Value,
) -> bool {
    let scope =
        persistence::work::ingestion::purpose(persistence::work::ingestion::Purpose::History);
    let session = read_saved_open(reader, root, command, shared).and_then(|()| {
        super::reader_snapshot_session(&mut reader.session, root, command, shared, false)
    });
    let pending = match session {
        Ok(session) => super::reader_earlier(session, &command.request, kept_tail_only, selection),
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
