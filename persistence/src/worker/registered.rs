//! The worker over a registered recording, whose canonical frontier is its progress.

use std::sync::Arc;

use collector::ring::{Found, PublicationTiming, RingReader, RunIdentity};

use super::{
    Derivation, Limits, Progress, RegisteredRun, Result, RunState, SourceState, Store, Window,
    WindowIdentity, Writers,
};
use crate::canonical::{ReadWork, SourceFrontierRead};
use crate::event_recovery::Batch;
use crate::resources::{ByteBudget, WORKING_BYTES_MAX};
use crate::store::{
    Interrupted, NormalProof, ProofStep, PublicationLedger, PublicationStep, RegisteredAttempt,
};
use crate::transaction::{
    AttemptInput, AttemptOperation, LossTransfer, Predecessor, SourceTiming, UnknownProvenance,
    UnknownReason,
};

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
    let budget = Arc::new(ByteBudget::new(
        usize::try_from(memory_limit(state)).map_err(|_| crate::Error::Bound("memory limit"))?,
    ));
    let mut interrupted = if state.loaded {
        None
    } else {
        store.interrupted()?
    };
    if interrupted == Some(Interrupted::Publication) {
        finish_publication(store.resume_publication(&budget)?)?;
        return Ok(Progress::Sealed);
    }
    if interrupted.is_some() && store.abandon_empty_attempt(&budget)? {
        interrupted = None;
    }
    if interrupted.is_some_and(|attempt| attempt != Interrupted::Staging(registered.identity)) {
        return Ok(Progress::Deferred);
    }
    if interrupted.is_none() && !state.loaded && persist_one_certified(store, registered, &budget)?
    {
        state.source = SourceState::Complete;
        return Ok(Progress::Complete);
    }
    let Some(mut reader) = super::persist_one_open(registered, state)? else {
        return Ok(Progress::Unavailable);
    };
    let mut newest = None;
    if super::persist_one_closed(&mut reader, &mut newest, registered.collector_pid, closed)? {
        reader.set_writer_gone();
    }
    if interrupted.is_some() {
        if !store.abandon_terminal_attempt(&budget)? {
            recover_staging(store, writers, registered, reader, state, &budget)?;
        }
        return Ok(Progress::Sealed);
    }
    if !state.loaded {
        persist_one_load(store, registered, &mut reader, state, &budget)?;
    }

    persist_one_scan(store, writers, registered, reader, newest, state, &budget)
}

/// Whether a terminal publication already certifies the run's end, which takes no source.
fn persist_one_certified(
    store: &Store,
    registered: &RegisteredRun,
    budget: &Arc<ByteBudget>,
) -> Result<bool> {
    let frontier = store.source_frontier(&registered.identity, budget, &mut ReadWork::default())?;

    Ok(frontier.is_some_and(|frontier| frontier.complete()))
}

/// Looks past lost publications for the next ready window of the run, and seals it.
fn persist_one_scan(
    store: &Store,
    writers: &Writers<'_>,
    registered: &RegisteredRun,
    mut reader: RingReader,
    newest: Option<PublicationTiming>,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<Progress> {
    let mut timing = PublicationTiming::default();
    for _step in 0..reader.shape().slot_count + 2 {
        match reader.read_timing_into(&mut timing)? {
            Found::Nothing => {
                if !reader.ended() || reader.next_publication() <= reader.publication_newest()? {
                    return Ok(Progress::Waiting);
                }

                seal_terminal(store, registered, reader, state, budget)?;
                return Ok(Progress::Complete);
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
                if !super::window_ready(&reader, newest.as_ref(), &identity, reader.ended())? {
                    state.next_publication = publication;
                    return Ok(Progress::Waiting);
                }

                let selection = Selection {
                    identity,
                    publication,
                };
                seal(store, writers, reader, selection, state, budget)?;
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

/// The window a scan selected and the publication it starts at.
#[derive(Clone, Copy)]
struct Selection {
    identity: WindowIdentity,
    publication: u64,
}

fn seal(
    store: &Store,
    writers: &Writers<'_>,
    mut reader: RingReader,
    selection: Selection,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<()> {
    let (input, frontier) = seal_input(
        store,
        &reader,
        &selection.identity,
        selection.publication,
        state,
        budget,
    )?;
    let attempt = match store.register_attempt_from_frontier(&input, &mut reader, frontier) {
        Ok(attempt) => attempt,
        Err(refusal) => {
            return if seal_selection_lost(&mut reader, selection.publication)? {
                Ok(())
            } else {
                Err(refusal.into())
            };
        }
    };

    seal_registered(store, writers, attempt, reader, selection, state, budget)
}

/// Whether the ring has overwritten the selected publication, which is what a scan selecting the
/// oldest retained publication of a full ring meets. The next scan counts it, so it stays unsaid.
fn seal_selection_lost(reader: &mut RingReader, publication: u64) -> Result<bool> {
    reader.seek_publication(publication)?;
    let found = reader.read_timing_into(&mut PublicationTiming::default())?;

    Ok(matches!(found, Found::Missed { .. }))
}

fn seal_registered(
    store: &Store,
    writers: &Writers<'_>,
    attempt: RegisteredAttempt<'_>,
    mut reader: RingReader,
    selection: Selection,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<()> {
    let started = std::time::Instant::now();
    let losses_before = state.losses;
    reader.seek_publication(selection.publication)?;
    reader.seek_event(attempt.input().losses.event_start)?;
    let window = seal_stage(
        &attempt,
        &mut reader,
        selection.identity,
        &[],
        state,
        budget,
    )?;
    if window.tick_count() == 0 {
        // The attempt names a publication the ring no longer holds, and an ordinary publication
        // starts at the publication it names. Nothing is saved, so the next scan selects again.
        drop(window);
        drop(attempt);
        state.losses = losses_before;
        return if store.abandon_empty_attempt(budget)? {
            Ok(())
        } else {
            Err(crate::Error::Bound("overwritten selection saved rows"))
        };
    }
    let manifest = seal_write(&attempt, writers, &window, state)?;
    let staged_bytes = window.staged_bytes();
    drop(window);

    seal_publish(attempt, &manifest, reader, budget)?;
    seal_report(&manifest, started, staged_bytes, state);

    Ok(())
}

/// Saves what the finished source holds past the last window, and certifies the run's end.
fn seal_terminal(
    store: &Store,
    registered: &RegisteredRun,
    mut reader: RingReader,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<()> {
    let (input, frontier) = seal_terminal_input(store, registered, &reader, state, budget)?;
    let AttemptOperation::Terminal { event_end } = input.operation else {
        return Err(crate::Error::Bound("terminal attempt operation"));
    };
    let attempt = store.register_attempt_from_frontier(&input, &mut reader, frontier)?;
    let manifest = crate::event_recovery::write_tail_into(
        reader,
        &registered.identity,
        input.losses.event_start..event_end,
        state.losses,
        budget,
        |index| {
            let ordinal = u32::try_from(index).map_err(|_| crate::Error::Bound("event batch"))?;
            Ok(attempt.create_event_batch(ordinal)?)
        },
    )?;

    let mut reader = RingReader::open_generation(&registered.ring_name)?;
    reader.set_writer_gone();
    let proof = attempt.prove_terminal(&manifest.recovery, &manifest.batches, reader, budget)?;
    seal_publish_proof(proof)?;
    state.source = SourceState::Complete;

    Ok(())
}

fn seal_terminal_input<'store>(
    store: &'store Store,
    registered: &RegisteredRun,
    reader: &RingReader,
    state: &RunState,
    budget: &Arc<ByteBudget>,
) -> Result<SealInput<'store>> {
    let event_end = reader
        .event_newest()?
        .checked_add(1)
        .ok_or(crate::Error::Bound("terminal event end"))?;
    let publication_end = reader
        .publication_newest()?
        .checked_add(1)
        .ok_or(crate::Error::Bound("terminal publication end"))?;

    seal_input_of(
        store,
        reader,
        &registered.identity,
        AttemptOperation::Terminal { event_end },
        publication_end,
        state,
        budget,
    )
}

fn seal_publish(
    attempt: RegisteredAttempt<'_>,
    manifest: &crate::Manifest,
    reader: RingReader,
    budget: &Arc<ByteBudget>,
) -> Result<()> {
    seal_publish_proof(attempt.prove_ordinary(manifest, reader, budget)?)
}

fn seal_publish_proof(mut proof: NormalProof<'_>) -> Result<()> {
    for _step in 0..PUBLICATION_STEPS_MAX {
        if proof.step()? == ProofStep::Complete {
            break;
        }
    }

    finish_publication(proof.into_publication()?)
}

fn finish_publication(mut publishing: crate::store::NormalPublication<'_>) -> Result<()> {
    for _step in 0..PUBLICATION_STEPS_MAX {
        if publishing.step()? == PublicationStep::Idle {
            return Ok(());
        }
    }

    Err(crate::Error::Bound("registered publication steps"))
}

/// Finishes the attempt a process left with no durable proof. Complete originals are proved
/// against the source, since saved rows outrank a fresh reading of them. Failing that, the
/// window is staged again from the source, which must still hold all of it.
fn recover_staging(
    store: &Store,
    writers: &Writers<'_>,
    registered: &RegisteredRun,
    reader: RingReader,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<()> {
    let writer_gone = reader.ended();
    let ledger = PublicationLedger::new(budget)?;
    let proved = store
        .resume_ordinary_proof(reader, budget, &ledger)
        .and_then(|mut recovery| {
            for _step in 0..PUBLICATION_STEPS_MAX {
                if recovery.step()? == PublicationStep::Idle {
                    return Ok(());
                }
            }
            Err(std::io::Error::from(std::io::ErrorKind::TimedOut))
        });
    let Err(refusal) = proved else {
        return Ok(());
    };
    collector::diagnostics::error(format_args!(
        "persister interrupted window staged again after {refusal}"
    ));

    let Some(mut reader) = super::persist_one_open(registered, state)? else {
        return Err(crate::Error::Bound("interrupted attempt lost its source"));
    };
    if writer_gone {
        reader.set_writer_gone();
    }
    recover_staging_again(store, writers, reader, state, budget)
}

fn recover_staging_again(
    store: &Store,
    writers: &Writers<'_>,
    mut reader: RingReader,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<()> {
    let started = std::time::Instant::now();
    let mut attempt = store.resume_attempt()?;
    let input = attempt.input().clone();
    let AttemptOperation::Ordinary {
        window,
        attempted_origin,
    } = input.operation
    else {
        return Err(crate::Error::Bound("interrupted attempt operation"));
    };
    let saved = attempt.saved(budget)?;
    attempt.restage(&saved)?;
    state.losses = crate::Losses {
        publications: input.losses.publications,
        events: saved.events_lost(input.losses.event_start)?,
        unknown: input.losses.unknown.is_some(),
    };
    reader.seek_publication(attempted_origin)?;
    reader.seek_event(input.losses.event_start)?;
    let identity = WindowIdentity {
        run: input.run,
        window,
    };
    let window = seal_stage(
        &attempt,
        &mut reader,
        identity,
        &saved.batches,
        state,
        budget,
    )?;
    // Events the ring lost since are the window's to count, but a lost publication would change
    // what the window holds, and the proof needs that unchanged.
    if state.losses.publications != input.losses.publications {
        return Err(crate::Error::Bound("interrupted window overwritten"));
    }

    attempt.restage_tables()?;
    let manifest = seal_write(&attempt, writers, &window, state)?;
    let staged_bytes = window.staged_bytes();
    drop(window);
    seal_publish(attempt, &manifest, reader, budget)?;
    seal_report(&manifest, started, staged_bytes, state);

    Ok(())
}

fn seal_report(
    manifest: &crate::Manifest,
    started: std::time::Instant,
    staged_bytes: u64,
    state: &mut RunState,
) {
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
}

/// An attempt's input and the read of the frontier it was built from, which its registration
/// takes over.
type SealInput<'store> = (AttemptInput, SourceFrontierRead<'store, 'static>);

fn seal_input<'store>(
    store: &'store Store,
    reader: &RingReader,
    identity: &WindowIdentity,
    publication: u64,
    state: &RunState,
    budget: &Arc<ByteBudget>,
) -> Result<SealInput<'store>> {
    let operation = AttemptOperation::Ordinary {
        window: identity.window,
        attempted_origin: publication,
    };

    seal_input_of(
        store,
        reader,
        &identity.run,
        operation,
        publication,
        state,
        budget,
    )
}

/// The attempt that takes the source from its frontier, charging the publications the scan found
/// lost up to `publication_end`.
fn seal_input_of<'store>(
    store: &'store Store,
    reader: &RingReader,
    run: &RunIdentity,
    operation: AttemptOperation,
    publication_end: u64,
    state: &RunState,
    budget: &Arc<ByteBudget>,
) -> Result<SealInput<'store>> {
    let mut frontier = store.source_frontier_read(run, budget)?;
    let (predecessor, publication_start, event_start, prior_window) = match frontier.result()? {
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

    let input = AttemptInput {
        run: *run,
        operation,
        captured: reader.dictionary_capacity().is_some(),
        source: SourceTiming::from_reader(reader),
        binding: reader.object_binding()?,
        losses: LossTransfer {
            predecessor,
            publication_start,
            publication_inherited_end: publication_end,
            event_start,
            event_inherited_end: event_start,
            publications: state.losses.publications,
            events: 0,
            unknown: seal_input_unknown(run, state),
            prior_window,
        },
    };

    Ok((input, frontier))
}

/// Names the last run whose extent could not be read. A registry that only counts its evicted
/// rings names none, so the run after the unknown extent is the last boundary anything observed.
fn seal_input_unknown(run: &RunIdentity, state: &RunState) -> Option<UnknownProvenance> {
    if !state.losses.unknown {
        return None;
    }
    let boundary = state.unknown_boundary.unwrap_or(*run);

    Some(UnknownProvenance {
        reason: UnknownReason::Predecessor,
        run: boundary.run,
        boot: boundary.boot,
    })
}

fn seal_stage(
    attempt: &RegisteredAttempt<'_>,
    reader: &mut RingReader,
    identity: WindowIdentity,
    saved: &[Batch],
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<Window> {
    let limits = Limits::from_shape(&reader.shape())?;
    let mut tick = crate::tick_buffer(&reader.shape())?;
    let events = crate::event_table::writer::Writer::charged(
        attempt.events_directory()?,
        identity,
        reader.next_event(),
        reader.dictionary_capacity().is_some(),
        budget,
    )?
    .resume(saved);
    let mut staged = super::staging::Staging::new(reader, identity, limits, events);
    for _step in 0..=reader.shape().slot_count {
        if !staged.read(reader, &mut tick, &mut state.losses)? || staged.lost_its_start() {
            break;
        }
    }

    staged.finish(reader, &mut state.losses)
}

fn seal_write(
    attempt: &RegisteredAttempt<'_>,
    writers: &Writers<'_>,
    window: &Window,
    state: &RunState,
) -> Result<crate::Manifest> {
    let directory = attempt.original_directory();
    // Recovery accepts a minute with no derived tables, since deriving may fail, and it finishes
    // one whose required tables are complete. Writing these first means a process that ends
    // part way has written some required table last, so its restart derives them all again.
    let derived = super::seal_derived(
        writers.derive,
        &Derivation {
            window,
            pending: directory,
        },
    )?;
    let mut tables = writers.encoder.encode_into(window, directory)?;
    tables.extend(derived);

    let mut manifest = super::seal_manifest(window, tables, state.losses)?;
    // The window's event coverage starts where the frontier ended, before any events it lost.
    manifest.events_first = attempt.input().losses.event_start;
    manifest.data_directory = format!(
        "data/{}/ready",
        collector::recording::identity_text(attempt.transaction())
    );

    Ok(manifest)
}

#[cfg(test)]
mod tests;
