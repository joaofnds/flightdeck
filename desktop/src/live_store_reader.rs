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

use viewer::store::{
    QueryContext, QueryHandle, QueryOutcome, QueryRequest, QuerySelection, QueryToken,
};

use super::{Command, Context, Histories, Reader, Shared, Snapshot, Update};

pub(super) struct StoreReader {
    handle: QueryHandle,
    context: Option<Arc<QueryContext>>,
    pending: Option<(u64, QueryToken)>,
    generation: u64,
    published: u64,
}

impl StoreReader {
    pub(super) fn new(handle: QueryHandle) -> Self {
        Self {
            handle,
            context: None,
            pending: None,
            generation: 0,
            published: 0,
        }
    }

    pub(super) fn rest(&mut self) {
        if let Err(error) = self.handle.cancel() {
            eprintln!("flightdeck Store query cancellation: {error:?}");
        }
        self.context = None;
        self.pending = None;
        self.published = 0;
    }

    fn snapshot(
        &mut self,
        command: &mut Command,
        shared: &Shared,
    ) -> Result<Option<Box<Snapshot>>, String> {
        if shared.cancellation.epoch() != command.epoch {
            self.handle
                .cancel()
                .map_err(|error| format!("Store query: {error:?}"))?;
            self.pending = None;
            return Ok(None);
        }
        if self.generation != command.generation {
            self.handle
                .cancel()
                .map_err(|error| format!("Store query: {error:?}"))?;
            self.pending = None;
            self.generation = command.generation;
            self.published = 0;
        }
        if self.published == command.generation {
            if command.request.second.is_some() {
                if command.request.route != "computer" {
                    return Ok(None);
                }
            }
            let mailbox = shared.mailbox.lock().map_err(|error| error.to_string())?;
            if mailbox.output.is_some() {
                return Ok(None);
            }
        }
        if command.context == Context::Keep {
            return self.snapshot_kept(command, shared);
        }
        if let Some(context) = &self.context {
            if command.request.second == Some(context.selected_second()) {
                if context.selected().detached() {
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
                        return payload::snapshot(context, command, shared, None).map(Some);
                    }
                }
            }
        }

        self.snapshot_query(command, shared)
    }

    fn snapshot_kept(
        &mut self,
        command: &mut Command,
        shared: &Shared,
    ) -> Result<Option<Box<Snapshot>>, String> {
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
        let observation = context
            .observation_for_identity(second, identity)
            .map_err(|error| error.to_string())?;

        if command.request.route == "overview" {
            if observation != context.selected().observation {
                return self.snapshot_kept_raw(command, shared, observation);
            }
        }

        payload::snapshot(context, command, shared, Some(&observation)).map(Some)
    }

    fn snapshot_kept_raw(
        &mut self,
        command: &mut Command,
        shared: &Shared,
        observation: viewer::live_app_response::Observation,
    ) -> Result<Option<Box<Snapshot>>, String> {
        let context = self
            .context
            .as_ref()
            .unwrap_or_else(|| unreachable!("kept Store context"));
        if self.pending.is_none() {
            let context_last_second = command.request.second.ok_or("Missing inspected second")?;
            let token = self
                .handle
                .submit(QueryRequest {
                    epoch: command.epoch,
                    source: Some(context.source),
                    selection: QuerySelection::Exact {
                        observation,
                        context_last_second,
                    },
                })
                .map_err(|error| format!("Store inspection query: {error:?}"))?;
            self.pending = Some((command.generation, token));
            return Ok(None);
        }
        let Some(reply) = self.handle.take() else {
            return Ok(None);
        };
        if self.pending != Some((command.generation, reply.token)) {
            return Ok(None);
        }
        self.pending = None;
        let raw = match reply.outcome {
            QueryOutcome::Ready(raw) => raw,
            QueryOutcome::NeedsLoad(reason) => {
                return Err(format!("Store inspection coverage: {reason:?}"));
            }
            QueryOutcome::Unavailable(reason) => {
                return Err(format!("Store inspection unavailable: {reason:?}"));
            }
        };

        payload::snapshot_with_raw(context, &raw, command, shared, Some(&observation)).map(Some)
    }

    fn snapshot_query(
        &mut self,
        command: &mut Command,
        shared: &Shared,
    ) -> Result<Option<Box<Snapshot>>, String> {
        if self.pending.is_none() {
            let selection = self.query_selection(command)?;
            if self.published == 0 {
                self.context = None;
            }
            self.snapshot_query_submit(command, selection)?;
            return Ok(None);
        }
        let Some(reply) = self.handle.take() else {
            return Ok(None);
        };
        if self.pending != Some((command.generation, reply.token)) {
            return Ok(None);
        }
        self.pending = None;
        let context = match reply.outcome {
            QueryOutcome::Ready(context) => context,
            QueryOutcome::NeedsLoad(reason) => {
                return Err(format!("Store coverage unavailable: {reason:?}"));
            }
            QueryOutcome::Unavailable(reason) => {
                return Err(format!("Store readings unavailable: {reason:?}"));
            }
        };
        if self.published == command.generation {
            if self.repeats_selection(&context) {
                return Ok(None);
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
                return Ok(None);
            }
        }
        let snapshot = payload::snapshot(&context, command, shared, None)?;
        self.context = Some(context);

        Ok(Some(snapshot))
    }

    fn snapshot_query_submit(
        &mut self,
        command: &Command,
        selection: QuerySelection,
    ) -> Result<(), String> {
        let token = self
            .handle
            .submit(QueryRequest {
                epoch: command.epoch,
                source: None,
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

        previous.selected().observation == context.selected().observation
    }

    fn query_selection(&self, command: &Command) -> Result<QuerySelection, String> {
        if command.request.route == "computer" {
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

pub(super) fn read(reader: &mut Reader, command: &mut Command, shared: &Shared) -> Option<Update> {
    if Instant::now() < reader.retry_after {
        return None;
    }
    let started = Instant::now();
    let store = reader
        .store
        .as_mut()
        .unwrap_or_else(|| unreachable!("Store reader branch"));
    match store.snapshot(command, shared) {
        Ok(Some(mut snapshot)) => {
            snapshot.read_ms = started.elapsed().as_secs_f64() * 1000.0;
            store.published = command.generation;
            super::reader_snapshot_record(reader, command, &snapshot, Histories::Read);
            Some(Update::Ready(snapshot))
        }
        Ok(None) => None,
        Err(message) => {
            reader.retry_after = Instant::now() + super::RETRY_INTERVAL;
            Some(Update::Failed {
                generation: command.generation,
                message,
            })
        }
    }
}

impl Drop for StoreReader {
    fn drop(&mut self) {
        match self.handle.cancel() {
            Ok(()) | Err(viewer::store::QuerySubmitError::Closed) => {}
            Err(error) => eprintln!("flightdeck Store query shutdown: {error:?}"),
        }
    }
}
