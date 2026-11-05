//! The worker over a registered recording, whose canonical frontier is its progress.

use std::sync::Arc;

use collector::ring::{Found, PublicationTiming, RingReader};

use super::{
    Derivation, Limits, Progress, RegisteredRun, Result, RunState, SourceState, Staged, Store,
    Window, WindowIdentity, Writers,
};
use crate::canonical::ReadWork;
use crate::resources::{ByteBudget, WORKING_BYTES_MAX};
use crate::store::{ProofStep, PublicationStep, RegisteredAttempt};
use crate::transaction::{AttemptInput, AttemptOperation, LossTransfer, Predecessor, SourceTiming};

/// More steps than the largest publication takes, so a publication that stops moving fails.
const PUBLICATION_STEPS_MAX: u64 = 1 << 20;

pub(super) fn persist_one(
    store: &Store,
    writers: &Writers<'_>,
    registered: &RegisteredRun,
    closed: bool,
    state: &mut RunState,
) -> Result<Progress> {
    if state.source == SourceState::Unavailable {
        return Ok(Progress::Unavailable);
    }
    if registered.identity.recording != store.recording() {
        super::source_unavailable(registered, state, "foreign recording identity");
        return Ok(Progress::Unavailable);
    }
    if state.source == SourceState::Complete {
        return Ok(Progress::Complete);
    }
    let Some(mut reader) = super::persist_one_open(registered, state)? else {
        return Ok(Progress::Unavailable);
    };
    let budget = Arc::new(ByteBudget::new(
        usize::try_from(memory_limit(state)).map_err(|_| crate::Error::Bound("memory limit"))?,
    ));
    if !state.loaded {
        persist_one_load(store, registered, &mut reader, state, &budget)?;
    }

    let mut newest = None;
    let closed =
        super::persist_one_closed(&mut reader, &mut newest, registered.collector_pid, closed)?;
    let mut timing = PublicationTiming::default();
    for _step in 0..reader.shape().slot_count + 2 {
        match reader.read_timing_into(&mut timing)? {
            Found::Nothing => {
                return match super::persist_one_stage_end(&reader, registered, closed, state)? {
                    Staged::Window(_) | Staged::Terminal => {
                        Err(crate::Error::Bound("registered terminal source"))
                    }
                    Staged::Waiting => Ok(Progress::Waiting),
                    Staged::Complete => Ok(Progress::Complete),
                };
            }
            Found::Missed { first, end } => {
                state.losses.publications += end - first;
                state.next_publication = reader.next_publication();
            }
            Found::Publication { publication, .. } => {
                let identity = WindowIdentity {
                    run: registered.identity,
                    window: super::timing_window_number(&reader, &timing)?,
                };
                if !super::window_ready(&reader, newest.as_ref(), &identity, closed)? {
                    state.next_publication = publication;
                    return Ok(Progress::Waiting);
                }

                seal(
                    store,
                    writers,
                    reader,
                    identity,
                    publication,
                    state,
                    &budget,
                )?;
                return Ok(Progress::Sealed);
            }
        }
    }

    Err(crate::Error::Bound("registered source scan"))
}

fn memory_limit(state: &RunState) -> u64 {
    if state.memory_limit == 0 {
        WORKING_BYTES_MAX
    } else {
        state.memory_limit
    }
}

fn persist_one_load(
    store: &Store,
    registered: &RegisteredRun,
    reader: &mut RingReader,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<()> {
    let frontier = store.source_frontier(&registered.identity, budget, &mut ReadWork::default())?;
    if let Some(frontier) = frontier {
        state.next_publication = frontier.publication_end();
        state.next_event = frontier.event_end();
        state.committed = frontier.publication_end() - 1;
    } else {
        state.next_publication = reader.publication_first();
        state.next_event = reader.event_first();
    }
    state.loaded = true;
    reader.seek_publication(state.next_publication)?;

    Ok(())
}

fn seal(
    store: &Store,
    writers: &Writers<'_>,
    mut reader: RingReader,
    identity: WindowIdentity,
    publication: u64,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<()> {
    let started = std::time::Instant::now();
    let input = seal_input(store, &reader, &identity, publication, state, budget)?;
    let attempt = store.register_attempt(&input, &mut reader, budget)?;

    reader.seek_publication(publication)?;
    reader.seek_event(input.losses.event_start)?;
    let window = seal_stage(&attempt, &mut reader, identity, state)?;
    let manifest = seal_write(&attempt, writers, &window, state)?;
    let staged_bytes = window.staged_bytes();
    drop(window);

    let mut proof = attempt.prove_ordinary(&manifest, reader, budget)?;
    for _step in 0..PUBLICATION_STEPS_MAX {
        if proof.step()? == ProofStep::Complete {
            break;
        }
    }
    let mut publishing = proof.into_publication()?;
    let mut idle = false;
    for _step in 0..PUBLICATION_STEPS_MAX {
        if publishing.step()? == PublicationStep::Idle {
            idle = true;
            break;
        }
    }
    if !idle {
        return Err(crate::Error::Bound("registered publication steps"));
    }

    state.committed = manifest.publication_end - 1;
    state.next_publication = manifest.publication_end;
    state.next_event = manifest.events_end;
    state.losses = crate::Losses::default();
    collector::diagnostics::error(format_args!(
        "persister sealed window={} publications={}..{} loss={} events_lost={} seal_ns={} \
        staged_bytes={staged_bytes} tables={}",
        manifest.identity.window,
        manifest.publication_first,
        manifest.publication_end,
        manifest.losses.publications,
        manifest.losses.events,
        started.elapsed().as_nanos(),
        manifest.tables.len()
    ));

    Ok(())
}

fn seal_input(
    store: &Store,
    reader: &RingReader,
    identity: &WindowIdentity,
    publication: u64,
    state: &RunState,
    budget: &Arc<ByteBudget>,
) -> Result<AttemptInput> {
    let frontier = store.source_frontier(&identity.run, budget, &mut ReadWork::default())?;
    let (predecessor, publication_start, event_start, prior_window) = match &frontier {
        Some(frontier) => (
            frontier.predecessor().clone(),
            frontier.publication_end(),
            frontier.event_end(),
            frontier.prior_window(),
        ),
        None => (
            Predecessor::Genesis,
            reader.publication_first(),
            reader.event_first(),
            None,
        ),
    };

    Ok(AttemptInput {
        run: identity.run,
        operation: AttemptOperation::Ordinary {
            window: identity.window,
            attempted_origin: publication,
        },
        captured: reader.dictionary_capacity().is_some(),
        source: SourceTiming::from_reader(reader),
        binding: reader.object_binding()?,
        losses: LossTransfer {
            predecessor,
            publication_start,
            publication_inherited_end: publication,
            event_start,
            event_inherited_end: event_start,
            publications: state.losses.publications,
            events: 0,
            unknown: None,
            prior_window,
        },
    })
}

fn seal_stage(
    attempt: &RegisteredAttempt<'_>,
    reader: &mut RingReader,
    identity: WindowIdentity,
    state: &mut RunState,
) -> Result<Window> {
    let limits = Limits::from_shape(&reader.shape())?;
    let mut tick = crate::tick_buffer(&reader.shape())?;
    let memory_limit = memory_limit(state);

    super::stage_window(
        reader,
        &mut tick,
        identity,
        limits,
        &mut state.losses,
        attempt.events_directory()?,
        memory_limit,
    )
}

fn seal_write(
    attempt: &RegisteredAttempt<'_>,
    writers: &Writers<'_>,
    window: &Window,
    state: &RunState,
) -> Result<crate::Manifest> {
    let directory = attempt.original_directory();
    let mut tables = writers.encoder.encode_into(window, directory)?;
    tables.extend(super::seal_derived(
        writers.derive,
        &Derivation {
            window,
            pending: directory,
        },
    )?);

    let mut manifest = super::seal_manifest(window, tables, state.losses)?;
    manifest.data_directory = format!(
        "data/{}/ready",
        collector::recording::identity_text(attempt.transaction())
    );

    Ok(manifest)
}

#[cfg(test)]
mod tests;
