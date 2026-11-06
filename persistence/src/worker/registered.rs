//! The worker over a registered recording, whose canonical frontier is its progress.

use std::sync::Arc;

use collector::ring::{Found, PublicationTiming, RingReader, RunIdentity};

use super::{
    Derivation, Limits, Progress, RegisteredRun, Result, RunState, SourceState, Store, Window,
    WindowIdentity, Writers,
};
use crate::canonical::{ReadWork, SourceFrontier, SourceFrontierRead};
use crate::event_recovery::Batch;
use crate::resources::{ByteBudget, WORKING_BYTES_MAX};
use crate::store::{
    Interrupted, Kept, LossPublication, NormalProof, ProofStep, PublicationLedger, PublicationStep,
    RegisteredAttempt, RowPublicationStep,
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
    if let Some(progress) = persist_one_settled(store, registered, state)? {
        return Ok(progress);
    }
    let budget = persist_budget(state.memory_limit)?;
    let turn = if state.loaded {
        SlotTurn::Free
    } else {
        persist_one_slot(store, registered, state, &budget)?
    };
    let interrupted = match turn {
        SlotTurn::Answered(progress) => return Ok(progress),
        SlotTurn::Interrupted | SlotTurn::InterruptedLoss => true,
        SlotTurn::Free => false,
    };
    let frontier = if !interrupted && !state.loaded {
        store.source_frontier(&registered.identity, &budget, &mut ReadWork::default())?
    } else {
        None
    };
    if frontier.as_ref().is_some_and(SourceFrontier::complete) {
        state.source = SourceState::Complete;
        return Ok(Progress::Complete);
    }
    let Some(mut reader) = super::persist_one_open(registered, state)? else {
        return Ok(Progress::Unavailable);
    };
    if reader.identity().is_none() {
        // A registered attempt binds the source's identity, so a ring with none proves nothing.
        super::source_unavailable(registered, state, "source names no identity");
        return Ok(Progress::Unavailable);
    }
    // A source no window could hold stays unsaved, and the runs after it go on being saved.
    if Limits::from_shape(&reader.shape()).is_err() {
        super::source_unavailable(registered, state, "source too large to save");
        return Ok(Progress::Unavailable);
    }
    let mut newest = None;
    if super::persist_one_closed(&mut reader, &mut newest, registered.collector_pid, closed)? {
        reader.set_writer_gone();
    }
    if matches!(turn, SlotTurn::InterruptedLoss) {
        finish_loss(store.resume_loss_proof(reader, &budget)?)?;
        return Ok(Progress::Sealed);
    }
    if interrupted {
        if !store.abandon_terminal_attempt(&reader, &budget)? {
            recover_staging(store, writers, registered, reader, state, &budget)?;
        }
        return Ok(Progress::Sealed);
    }
    if !state.loaded {
        persist_one_load(frontier.as_ref(), &mut reader, state)?;
    }

    persist_one_scan(store, writers, registered, reader, newest, state, &budget)
}

/// The answer when the run's state alone gives one, with no source to read.
fn persist_one_settled(
    store: &Store,
    registered: &RegisteredRun,
    state: &mut RunState,
) -> Result<Option<Progress>> {
    if state.source == SourceState::Unavailable {
        return Ok(Some(Progress::Unavailable));
    }
    if registered.identity.recording != store.recording() {
        super::source_unavailable(registered, state, "foreign recording identity");
        return Ok(Some(Progress::Unavailable));
    }
    if state.source == SourceState::Complete {
        return Ok(Some(Progress::Complete));
    }
    // Nothing but the run that holds the slot changes what is in it, so a deferred run reads only
    // whether it is still held, however long that run takes to come to it.
    if state.deferred && store.slot_occupied()? {
        return Ok(Some(Progress::Deferred));
    }
    state.deferred = false;
    Ok(None)
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

/// What the foreground slot means for a run's turn.
enum SlotTurn {
    /// What the slot held settles the turn.
    Answered(Progress),
    /// The slot holds this run's attempt, which needs its source.
    Interrupted,
    /// The slot holds what this run lost, whose proof needs its source.
    InterruptedLoss,
    Free,
}

/// Finishes what the slot holds if that takes no source, and otherwise says whose it is.
fn persist_one_slot(
    store: &Store,
    registered: &RegisteredRun,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<SlotTurn> {
    let Some(attempt) = store.interrupted()? else {
        return Ok(SlotTurn::Free);
    };
    if attempt == Interrupted::Publication {
        finish_publication(store.resume_publication(budget)?)?;
        return Ok(SlotTurn::Answered(Progress::Sealed));
    }
    if attempt == Interrupted::Rows {
        finish_rows(store, budget)?;
        return Ok(SlotTurn::Answered(Progress::Sealed));
    }
    if let Interrupted::Loss(run) = attempt {
        return persist_one_slot_loss(store, registered, &run, state, budget);
    }
    if store.abandon_empty_attempt(budget)? {
        return Ok(SlotTurn::Free);
    }
    if attempt != Interrupted::Staging(registered.identity) {
        state.deferred = true;
        collector::diagnostics::error(format_args!(
            "persister run deferred: another run's interrupted attempt holds the recording's slot"
        ));
        return Ok(SlotTurn::Answered(Progress::Deferred));
    }

    Ok(SlotTurn::Interrupted)
}

/// Finishes what a run lost if that takes no source, and otherwise says whose source it takes.
fn persist_one_slot_loss(
    store: &Store,
    registered: &RegisteredRun,
    run: &RunIdentity,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<SlotTurn> {
    match store.resume_loss_publication(budget) {
        Ok(publication) => {
            finish_loss(publication)?;
            return Ok(SlotTurn::Answered(Progress::Sealed));
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(error) => return Err(error.into()),
    }
    if *run != registered.identity {
        state.deferred = true;
        return Ok(SlotTurn::Answered(Progress::Deferred));
    }

    Ok(SlotTurn::InterruptedLoss)
}

fn persist_budget(memory_limit: u64) -> Result<Arc<ByteBudget>> {
    let limit = if memory_limit == 0 {
        WORKING_BYTES_MAX
    } else {
        memory_limit
    };

    Ok(Arc::new(ByteBudget::new(
        usize::try_from(limit).map_err(|_| crate::Error::Bound("memory limit"))?,
    )))
}

/// Finishes an attempt every run's turn left in the slot, whose own run cannot finish it because
/// its source is gone or it left the registry, and says whether there was one.
pub(super) fn persist_unowned(store: &Store, memory_limit: u64) -> Result<bool> {
    let Some(attempt) = store.interrupted()? else {
        return Ok(false);
    };
    let budget = persist_budget(memory_limit)?;
    match attempt {
        Interrupted::Publication => finish_publication(store.resume_publication(&budget)?)?,
        Interrupted::Rows => finish_rows(store, &budget)?,
        Interrupted::Staging(_) => keep_saved_rows(store, &budget)?,
        Interrupted::Loss(_) => match store.resume_loss_publication(&budget) {
            Ok(publication) => finish_loss(publication)?,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                store.abandon_loss_attempt()?;
            }
            Err(error) => return Err(error.into()),
        },
    }

    Ok(true)
}

/// Keeps what an attempt saved when its source can no longer supply its window, as a fragment of
/// rows. The next scan counts the window's publications as lost, and finds any later events.
fn keep_saved_rows(store: &Store, budget: &Arc<ByteBudget>) -> Result<()> {
    match store.keep_only_rows(budget)? {
        Kept::Nothing => Ok(()),
        Kept::Rows => finish_rows(store, budget),
        Kept::Proof => finish_publication(store.resume_publication(budget)?),
    }
}

fn finish_rows(store: &Store, budget: &Arc<ByteBudget>) -> Result<()> {
    let ledger = PublicationLedger::new(budget)?;
    let Some(mut publishing) = store.resume_row_publication(None, budget, &ledger)? else {
        return Ok(());
    };
    for _step in 0..PUBLICATION_STEPS_MAX {
        match publishing.step()? {
            RowPublicationStep::Idle => return Ok(()),
            RowPublicationStep::Continue => {}
            RowPublicationStep::Unresolved => {
                return Err(crate::Error::Bound("row publication unresolved"));
            }
        }
    }

    Err(crate::Error::Bound("row publication steps"))
}

fn persist_one_load(
    frontier: Option<&SourceFrontier>,
    reader: &mut RingReader,
    state: &mut RunState,
) -> Result<()> {
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
/// oldest retained publication of a full ring meets. The next scan counts the loss, so a seal that
/// meets this reports none of its own.
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
    let mut clock = SealClock::start();
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
    clock.staged();
    let manifest = seal_write(&attempt, writers, &window, state)?;
    let staged_bytes = window.staged_bytes();
    drop(window);
    clock.written();

    seal_publish(attempt, &manifest, reader, budget, &mut clock)?;
    seal_report(&manifest, &clock, staged_bytes, state);

    Ok(())
}

/// Where a seal's time went, which the diagnostics give for each window saved.
struct SealClock {
    started: std::time::Instant,
    staged_ns: u128,
    written_ns: u128,
    proved_ns: u128,
}

impl SealClock {
    fn start() -> Self {
        Self {
            started: std::time::Instant::now(),
            staged_ns: 0,
            written_ns: 0,
            proved_ns: 0,
        }
    }

    fn staged(&mut self) {
        self.staged_ns = self.started.elapsed().as_nanos();
    }

    fn written(&mut self) {
        self.written_ns = self.started.elapsed().as_nanos();
    }

    fn proved(&mut self) {
        self.proved_ns = self.started.elapsed().as_nanos();
    }
}

impl std::fmt::Display for SealClock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total_ns = self.started.elapsed().as_nanos();
        write!(
            formatter,
            "seal_ns={total_ns} stage_ns={} tables_ns={} proof_ns={} publish_ns={}",
            self.staged_ns,
            self.written_ns - self.staged_ns,
            self.proved_ns - self.written_ns,
            total_ns - self.proved_ns,
        )
    }
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
        unreachable!("a terminal attempt's input has a terminal operation");
    };
    let attempt = store.register_attempt_from_frontier(&input, &mut reader, frontier)?;
    let mut clock = SealClock::start();
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
    clock.staged();
    clock.written();
    // A run that kept no row and can say how much it lost ends in an object of only that. A mark of
    // unknown extent is what such an object cannot say.
    let losses = manifest.recovery.losses;
    let lost = losses.publications != 0 || losses.events != 0;
    if manifest.recovery.rows == 0 && lost && !losses.unknown {
        // The report subtracts this mark from the next, so it counts this proof as publishing.
        clock.proved();
        finish_loss(attempt.prove_loss(reader, budget)?)?;
    } else {
        let proof =
            attempt.prove_terminal(&manifest.recovery, &manifest.batches, reader, budget)?;
        seal_publish_proof(proof, &mut clock)?;
    }
    collector::diagnostics::error(format_args!(
        "persister saved a run's tail rows={} {clock}",
        manifest.recovery.rows
    ));
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
    clock: &mut SealClock,
) -> Result<()> {
    seal_publish_proof(attempt.prove_ordinary(manifest, reader, budget)?, clock)
}

fn finish_loss(mut publishing: LossPublication<'_>) -> Result<()> {
    for _step in 0..PUBLICATION_STEPS_MAX {
        if publishing.step()? == PublicationStep::Idle {
            return Ok(());
        }
    }

    Err(crate::Error::Bound("registered loss steps"))
}

fn seal_publish_proof(mut proof: NormalProof<'_>, clock: &mut SealClock) -> Result<()> {
    for _step in 0..PUBLICATION_STEPS_MAX {
        if proof.step()? == ProofStep::Complete {
            clock.proved();
            return finish_publication(proof.into_publication()?);
        }
    }

    Err(crate::Error::Bound("registered proof steps"))
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
    let Some(refusal) = recover_staging_proof(store, reader, budget, &ledger)? else {
        return Ok(());
    };
    collector::diagnostics::error(format_args!(
        "persister interrupted window staged again after {refusal}"
    ));

    let Some(mut reader) = super::persist_one_open(registered, state)? else {
        return keep_saved_rows(store, budget);
    };
    if writer_gone {
        reader.set_writer_gone();
    }
    recover_staging_again(store, writers, reader, state, budget)
}

/// Proves and publishes what the attempt saved, or says why its originals prove nothing. Only a
/// refusal of the originals' contents leads to staging again. Any other failure stops the worker,
/// as does any failure once publication has begun, since it then needs every original.
fn recover_staging_proof(
    store: &Store,
    reader: RingReader,
    budget: &Arc<ByteBudget>,
    ledger: &PublicationLedger,
) -> Result<Option<std::io::Error>> {
    let mut recovery = match store.resume_ordinary_proof(reader, budget, ledger) {
        Ok(recovery) => recovery,
        Err(error) if recover_staging_refusal(&error) => return Ok(Some(error)),
        Err(error) => return Err(error.into()),
    };
    for _step in 0..PUBLICATION_STEPS_MAX {
        match recovery.step() {
            Ok(PublicationStep::Idle) => return Ok(None),
            Ok(_) => {}
            Err(error) if !recovery.publishing() && recover_staging_refusal(&error) => {
                return Ok(Some(error));
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(crate::Error::Bound("registered recovery steps"))
}

/// Whether the error says what the attempt saved cannot be proved, as opposed to the machine
/// refusing the work, which says nothing about what was saved.
fn recover_staging_refusal(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::Unsupported
            | std::io::ErrorKind::InvalidData
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::NotFound
            | std::io::ErrorKind::WouldBlock
    )
}

fn recover_staging_again(
    store: &Store,
    writers: &Writers<'_>,
    mut reader: RingReader,
    state: &mut RunState,
    budget: &Arc<ByteBudget>,
) -> Result<()> {
    let mut clock = SealClock::start();
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
    if seal_selection_lost(&mut reader, attempted_origin)? {
        drop(attempt);
        return keep_saved_rows(store, budget);
    }
    attempt.restage(&saved)?;
    let unknown_since = state.losses.unknown && input.losses.unknown.is_none();
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
        drop(window);
        drop(attempt);
        state.losses = crate::Losses {
            publications: 0,
            events: 0,
            unknown: unknown_since,
        };
        return keep_saved_rows(store, budget);
    }

    clock.staged();
    attempt.restage_tables()?;
    let manifest = seal_write(&attempt, writers, &window, state)?;
    let staged_bytes = window.staged_bytes();
    drop(window);
    clock.written();
    seal_publish(attempt, &manifest, reader, budget, &mut clock)?;
    seal_report(&manifest, &clock, staged_bytes, state);
    // The attempt registered before that mark was raised, so the next minute carries it.
    state.losses.unknown = unknown_since;

    Ok(())
}

fn seal_report(
    manifest: &crate::Manifest,
    clock: &SealClock,
    staged_bytes: u64,
    state: &mut RunState,
) {
    state.committed = manifest.publication_end - 1;
    state.next_publication = manifest.publication_end;
    state.next_event = manifest.events_end;
    state.losses = crate::Losses::default();
    collector::diagnostics::error(format_args!(
        "persister sealed window={} publications={}..{} loss={} events_lost={} {clock} \
        staged_bytes={staged_bytes} tables={}",
        manifest.identity.window,
        manifest.publication_first,
        manifest.publication_end,
        manifest.losses.publications,
        manifest.losses.events,
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
    let mut frontier = store.source_frontier_rows_held(run, budget)?;
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
            return staged.finish(reader, &mut state.losses);
        }
    }

    Err(crate::Error::Bound("registered window ticks"))
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

    let event_index = window
        .event_index
        .as_ref()
        .ok_or(crate::Error::Bound("sealed window without an event index"))?;
    let event_batches = u32::try_from(event_index.batches.len())
        .map_err(|_| crate::Error::Bound("event part count"))?;
    let mut manifest = super::seal_manifest(window, tables, state.losses, event_batches);
    // The window's event coverage starts where the frontier ended, before any events it lost.
    manifest.events_first = attempt.input().losses.event_start;
    manifest.data_directory = format!(
        "data/{}/ready",
        collector::recording::identity_text(attempt.transaction())
    );

    Ok(manifest)
}

/// Staging of a window a caller steps through itself, with the memory a persister has.
#[cfg(any(test, feature = "fixtures"))]
fn staging_by_hand(
    attempt: &RegisteredAttempt<'_>,
    reader: &RingReader,
    identity: WindowIdentity,
) -> Result<super::staging::Staging> {
    let events = crate::event_table::writer::Writer::charged(
        attempt.events_directory()?,
        identity,
        reader.next_event(),
        reader.dictionary_capacity().is_some(),
        &Arc::new(ByteBudget::new(256 * 1024 * 1024)),
    )?;

    Ok(super::staging::Staging::new(
        reader,
        identity,
        Limits::from_shape(&reader.shape())?,
        events,
    ))
}

/// Saves a registered run's first window, which starts at its first publication, with `overrun`
/// called after the persister reads that publication, so the ring can overwrite the next ones
/// before the persister reaches them, and the window keeps what it read on either side of them.
#[cfg(any(test, feature = "fixtures"))]
pub fn seal_first_window_overrun(
    store: &Store,
    registered: &RegisteredRun,
    derive: &dyn super::Derive,
    overrun: &mut dyn FnMut(),
) -> Result<()> {
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let writers = Writers {
        encoder: crate::Encoder,
        derive,
    };
    let mut state = RunState {
        next_publication: 1,
        loaded: true,
        ..RunState::default()
    };
    let identity = WindowIdentity {
        run: registered.identity,
        window: 0,
    };
    let mut reader = RingReader::open_generation(&registered.ring_name)?;
    let input = seal_input(store, &reader, &identity, 1, &state, &budget)?.0;
    let attempt = store.register_attempt(&input, &mut reader, &budget)?;
    reader.seek_publication(1)?;
    reader.seek_event(input.losses.event_start)?;
    let shape = reader.shape();
    let mut tick = crate::tick_buffer(&shape)?;
    let mut staged = staging_by_hand(&attempt, &reader, identity)?;
    let mut reading = staged.read(&mut reader, &mut tick, &mut state.losses)?;
    overrun();
    for _step in 0..=shape.slot_count {
        if !reading {
            break;
        }
        reading = staged.read(&mut reader, &mut tick, &mut state.losses)?;
    }
    let window = staged.finish(&mut reader, &mut state.losses)?;
    let manifest = seal_write(&attempt, &writers, &window, &state)?;
    drop(window);
    seal_publish(attempt, &manifest, reader, &budget, &mut SealClock::start())
}

#[cfg(test)]
mod loss_tests;
#[cfg(test)]
mod tests;
