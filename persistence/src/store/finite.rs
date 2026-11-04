//! Fixed finite event construction. Registration is construction authority, not loss proof.
use super::normal_proof::SourceProof;
use super::publication_control::{self, CountedControl};
use super::{
    PublicationLedger, PublicationWork, RegisteredAttempt, RowPublication, RowPublicationStep,
    StagingPermit, Store,
};
use crate::canonical::SourceFrontier;
use crate::event_recovery::{Batch, batch};
use crate::resources::{ByteBudget, ByteReservation};
use crate::transaction::normal::NormalState;
use crate::transaction::{AttemptInput, AttemptOperation, LossTransfer, Predecessor, SourceTiming};
use collector::recording::{
    bootstrap::codec::{self, SlotKind},
    private::{self, PrivateDirectory},
};
use collector::ring::{Found, FoundEvent, RingReader};
use std::{
    io,
    sync::{Arc, atomic::Ordering},
};

#[path = "finite_capture.rs"]
mod capture;
pub use capture::FiniteEventRows;

pub struct FiniteEvents<'store> {
    construction: Option<Box<Construction<'store>>>,
    loss: Option<Box<super::LossPublication<'store>>>,
    publication: Option<Box<RowPublication<'store>>>,
    budget: Arc<ByteBudget>,
    ledger: PublicationLedger,
    _owner_reservation: ByteReservation,
}
struct Construction<'store> {
    attempt: RegisteredAttempt<'store>,
    source: SourceProof,
    rows: Vec<batch::Row>,
    batches: Vec<Batch>,
    next: u64,
    bytes: usize,
    recorded: PublicationWork,
    durable_rows: u64,
    retained: Option<CapturedRows>,
    reservation: ByteReservation,
}
struct CapturedRows {
    input: AttemptInput,
    rows: Vec<batch::Row>,
    bytes: usize,
    reservation: ByteReservation,
}
enum FrontierInput {
    Read,
    Prepared(Option<SourceFrontier>),
}
struct PreparedConstruction {
    input: AttemptInput,
    source: SourceProof,
    retained: Option<CapturedRows>,
    reservation: ByteReservation,
}
impl Store {
    /// Constructs one independently owned finite event prefix before ordinary registration.
    pub fn begin_finite_events(
        &self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<FiniteEvents<'_>> {
        self.begin_finite_inner(reader, budget, ledger, FrontierInput::Read, None, None)
    }
    /// Consumes the completed query; its attempted work joins the same caller ledger.
    pub fn begin_finite_events_from_frontier(
        &self,
        reader: RingReader,
        mut frontier: crate::canonical::SourceFrontierRead<'_, '_>,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<FiniteEvents<'_>> {
        ledger.require_budget(budget)?;
        if !Arc::ptr_eq(frontier.byte_budget(), budget) {
            return Err(io::ErrorKind::InvalidInput.into());
        }

        let result: io::Result<_> = (|| {
            let permit = self.finite_permit()?;
            frontier.validate_owner(&self.directory, &reader.identity().ok_or_else(invalid)?)?;
            Ok((frontier.result()?.cloned(), permit))
        })();
        ledger.update(|work| work.canonical.add(frontier.work()));
        let (selected, permit) = result?;
        drop(frontier);
        self.begin_finite_inner(
            reader,
            budget,
            ledger,
            FrontierInput::Prepared(selected),
            None,
            Some(permit),
        )
    }
    fn begin_finite_inner(
        &self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
        prepared: FrontierInput,
        retained: Option<CapturedRows>,
        permit: Option<StagingPermit>,
    ) -> io::Result<FiniteEvents<'_>> {
        ledger.require_budget(budget)?;
        if !self.protocol_two {
            return Err(io::ErrorKind::Unsupported.into());
        }

        let owner_reservation = budget.reserve(owner_bytes()).ok_or_else(memory)?;
        let permit = if let Some(permit) = permit {
            permit
        } else {
            self.finite_permit()?
        };
        let control = CountedControl {
            root: &self.directory,
            ledger: Some(ledger),
        };
        let result = self.finite_visible(&control, ledger);
        let visible = result?;
        let pending = visible.is_none();
        let state = if pending {
            publication_control::pending_finite(&control, self.recording)?
        } else {
            visible
        };
        if let Some(state) = state {
            if retained.is_some() {
                return Err(io::ErrorKind::WouldBlock.into());
            }

            let reader = if pending {
                self.adopt_finite_registration(reader, &state, budget, ledger)?
            } else {
                reader
            };
            drop(permit);
            drop(owner_reservation);
            return self.finite_occupied(&state, Some(reader), budget, ledger);
        }

        let prepared =
            self.prepare_finite_construction(reader, prepared, retained, budget, ledger)?;
        let attempt = self.register_finite(&prepared.input, permit, ledger)?;
        FiniteEvents::constructed(attempt, prepared, owner_reservation, budget, ledger)
    }
    fn finite_visible(
        &self,
        control: &CountedControl<'_>,
        ledger: &PublicationLedger,
    ) -> io::Result<Option<NormalState>> {
        let mut work = PublicationWork::default();
        let result = publication_control::read_optional_normal(control, self.recording, &mut work);
        ledger.record(&work, &mut PublicationWork::default());
        result
    }
    fn finite_permit(&self) -> io::Result<StagingPermit> {
        self.staging
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        Ok(StagingPermit(Arc::clone(&self.staging)))
    }
    fn adopt_finite_registration(
        &self,
        reader: RingReader,
        state: &NormalState,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<RingReader> {
        let control = CountedControl {
            root: &self.directory,
            ledger: Some(ledger),
        };
        let reservation = budget.reserve(64 * 1024).ok_or_else(memory)?;
        let result = SourceProof::new(reader, &state.input, reservation);
        ledger.update(|work| work.source_observations += 1);
        let source = result?;
        let mut canonical = crate::canonical::ReadWork::default();
        let result = self.source_frontier(&state.input.run, budget, &mut canonical);
        ledger.update(|work| work.canonical.add(canonical));
        if let Some(frontier) = result? {
            frontier.matches_input(&state.input)?;
        } else if state.input.losses.predecessor != Predecessor::Genesis {
            return Err(invalid());
        }

        let mut old = [0; 4096];
        let mut new = [0; 4096];
        codec::encode_idle(self.recording, SlotKind::Foreground, &mut old)?;
        state.encode(&mut new)?;
        let mut syncs = 0;
        let result = publication_control::replace(&control, "foreground", &old, &new, &mut syncs);
        ledger.update(|work| work.full_syncs += syncs);
        result?;
        Ok(source.into_reader())
    }
    fn prepare_finite_construction(
        &self,
        reader: RingReader,
        prepared: FrontierInput,
        retained: Option<CapturedRows>,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<PreparedConstruction> {
        let mut input = finite_input(self, &reader, prepared, budget, ledger)?;
        if retained
            .as_ref()
            .is_some_and(|retained| retained.input != input)
        {
            return Err(invalid());
        }

        let reservation = budget.reserve(batch::WORKING_BYTES).ok_or_else(memory)?;
        let proof_reservation = budget.reserve(64 * 1024).ok_or_else(memory)?;
        let result = SourceProof::new(reader, &input, proof_reservation);
        ledger.update(|work| work.source_observations += 1);
        let mut source = result?;
        source.observations.set(0);
        let endpoint = finite_endpoint(&mut source, &input, budget);
        ledger.update(|work| {
            work.source_reads += source.reads;
            work.source_observations += source.observations.get();
        });
        let (observed_event_end, observed_source_first) = endpoint?;
        if retained.as_ref().is_some_and(|retained| {
            retained
                .rows
                .first()
                .is_some_and(|row| row.event.number < input.losses.event_start)
                || retained
                    .rows
                    .last()
                    .is_some_and(|row| row.event.number >= observed_event_end)
        }) {
            return Err(invalid());
        }

        if let AttemptOperation::Finite {
            event_end,
            source_first,
            ..
        } = &mut input.operation
        {
            *event_end = observed_event_end;
            *source_first = observed_source_first;
        }
        input.validate()?;
        Ok(PreparedConstruction {
            input,
            source,
            retained,
            reservation,
        })
    }
    fn register_finite(
        &self,
        input: &AttemptInput,
        permit: StagingPermit,
        ledger: &PublicationLedger,
    ) -> io::Result<RegisteredAttempt<'_>> {
        let control = CountedControl {
            root: &self.directory,
            ledger: Some(ledger),
        };
        let old = publication_control::read(&control, "foreground", 4096)?;
        if !matches!(
            codec::decode_slot(&old, self.recording, SlotKind::Foreground)?,
            codec::Slot::Idle
        ) {
            return Err(invalid());
        }

        let transaction = super::attempt::registered_transaction(&control, input, &old)?;
        let mut bytes = [0; 4096];
        input.encode_registered(transaction, &mut bytes)?;
        let mut syncs = 0;
        let result = publication_control::replace(&control, "foreground", &old, &bytes, &mut syncs);
        ledger.update(|work| work.full_syncs += syncs);
        result?;
        ledger.update(|work| work.inventory.open_attempts += 1);
        let base = self.directory.open_directory("recovery-data")?;
        ledger.update(|work| work.inventory.open_attempts += 1);
        let mut syncs = 0;
        let result = base.create_directory_counted(
            &collector::recording::identity_text(transaction).to_string(),
            &mut syncs,
        );
        ledger.update(|work| work.full_syncs += syncs);
        let object = result?;
        ledger.update(|work| work.full_syncs += 1);
        base.full_sync()?;
        ledger.update(|work| work.inventory.open_attempts += 1);
        let mut syncs = 0;
        let result = object.create_directory_counted("pending", &mut syncs);
        ledger.update(|work| work.full_syncs += syncs);
        let directory = result?;
        ledger.update(|work| work.full_syncs += 1);
        object.full_sync()?;
        Ok(RegisteredAttempt {
            transaction,
            input: input.clone(),
            directory,
            resumed: false,
            candidates: false,
            _permit: permit,
            owner: &self.owner,
            root: &self.directory,
            placement: super::OriginalPlacement::Pending,
        })
    }
    fn finite_occupied(
        &self,
        state: &NormalState,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<FiniteEvents<'_>> {
        if !state.input.is_finite() {
            return Err(invalid());
        }

        let owner_reservation = budget.reserve(owner_bytes()).ok_or_else(memory)?;
        let (loss, publication) = if state.salvage.is_some() {
            let loss = match reader {
                Some(reader) => self.resume_loss_proof_with_ledger(reader, budget, ledger)?,
                None => self.resume_loss_publication_with_ledger(budget, ledger)?,
            };
            (Some(loss), None)
        } else {
            (
                None,
                Some(
                    self.resume_row_publication(reader, budget, ledger)?
                        .ok_or_else(invalid)?,
                ),
            )
        };
        Ok(FiniteEvents {
            loss: loss.map(Box::new),
            publication: publication.map(Box::new),
            construction: None,
            budget: Arc::clone(budget),
            ledger: ledger.clone(),
            _owner_reservation: owner_reservation,
        })
    }
    /// Reacquires consuming authority. Missing transport never issues new construction rights.
    pub fn resume_finite_events(
        &self,
        reader: Option<RingReader>,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<Option<FiniteEvents<'_>>> {
        ledger.require_budget(budget)?;
        let admission = budget.reserve(owner_bytes()).ok_or_else(memory)?;
        let mut work = PublicationWork::default();
        let result = publication_control::read_optional_normal(
            &CountedControl {
                root: &self.directory,
                ledger: Some(ledger),
            },
            self.recording,
            &mut work,
        );
        ledger.record(&work, &mut PublicationWork::default());
        let state = result?;
        drop(admission);
        state
            .map(|state| self.finite_occupied(&state, reader, budget, ledger))
            .transpose()
    }
}
impl<'store> FiniteEvents<'store> {
    #[cfg(test)]
    pub(super) fn construction_position(&self) -> Option<u64> {
        self.construction.as_ref().map(|owner| owner.next)
    }
    fn constructed(
        attempt: RegisteredAttempt<'store>,
        prepared: PreparedConstruction,
        owner_reservation: ByteReservation,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<Self> {
        let PreparedConstruction {
            input,
            source,
            retained,
            reservation,
        } = prepared;
        let next = input.finite_plan().ok_or_else(invalid)?.source_first;
        let recorded = PublicationWork {
            source_reads: source.reads,
            source_observations: source.observations.get(),
            ..Default::default()
        };
        Ok(FiniteEvents {
            loss: None,
            publication: None,
            construction: Some(Box::new(Construction {
                attempt,
                source,
                rows: Vec::with_capacity(crate::event_observation::BATCH_ROWS_MAX),
                batches: Vec::with_capacity(4096),
                next,
                bytes: batch::ROWS_BYTES_MAX,
                recorded,
                durable_rows: 0,
                retained,
                reservation,
            })),
            budget: Arc::clone(budget),
            ledger: ledger.clone(),
            _owner_reservation: owner_reservation,
        })
    }

    pub fn step(&mut self) -> io::Result<RowPublicationStep> {
        if let Some(loss) = &mut self.loss {
            return loss.step().map(|step| {
                if step == super::PublicationStep::Idle {
                    RowPublicationStep::Idle
                } else {
                    RowPublicationStep::Continue
                }
            });
        }
        if let Some(publication) = &mut self.publication {
            return publication.step();
        }

        let construction = self.construction.as_mut().ok_or_else(invalid)?;
        let result = construction.step(&self.budget, &self.ledger);
        self.ledger.record(
            &PublicationWork {
                source_reads: construction.source.reads,
                source_observations: construction.source.observations.get(),
                ..Default::default()
            },
            &mut construction.recorded,
        );
        if result? {
            let construction = self.construction.take().ok_or_else(invalid)?;
            let Construction {
                attempt,
                source,
                reservation,
                ..
            } = *construction;
            drop(reservation);
            self.publication = Some(Box::new(attempt.recover_rows(
                source.into_reader(),
                &self.budget,
                &self.ledger,
            )?));
        }
        Ok(RowPublicationStep::Continue)
    }
}
impl Construction<'_> {
    fn step(&mut self, budget: &Arc<ByteBudget>, ledger: &PublicationLedger) -> io::Result<bool> {
        let input = &self.attempt.input;
        let state = publication_control::read_normal(
            &CountedControl {
                root: self.attempt.root,
                ledger: Some(ledger),
            },
            input.run.recording,
        )?;
        if state != NormalState::registered(self.attempt.transaction, input.clone()) {
            return Err(invalid());
        }

        let AttemptOperation::Finite { event_end, .. } = input.operation else {
            return Err(invalid());
        };
        if self.retained.is_some() {
            self.adopt_retained()?;
            return Ok(false);
        }

        self.fill(event_end, ledger)?;
        if !self.rows.is_empty() && (self.finished(event_end) || self.full()) {
            let fact = self.write_rows(budget, ledger)?;
            self.durable_rows += fact.rows;
            self.batches.push(fact);
            self.rows.clear();
            self.bytes = batch::ROWS_BYTES_MAX;
            return Ok(false);
        }

        if !self.finished(event_end) {
            return Ok(false);
        }

        self.write_index(event_end, ledger)?;
        Ok(true)
    }
    fn adopt_retained(&mut self) -> io::Result<()> {
        self.source.check(&self.attempt.input)?;
        assert!(self.rows.is_empty());
        let retained = self.retained.take().ok_or_else(invalid)?;
        let end = retained
            .rows
            .last()
            .map(|row| row.event.number.checked_add(1).ok_or_else(invalid))
            .transpose()?
            .unwrap_or(self.next);
        self.next = self.next.max(end);
        self.rows = retained.rows;
        self.bytes = retained.bytes;
        drop(retained.reservation);
        Ok(())
    }
    fn fill(&mut self, event_end: u64, ledger: &PublicationLedger) -> io::Result<()> {
        for _ in 0..128 {
            if self.finished(event_end) || self.full() {
                break;
            }
            let next = self.next;
            ledger.update(|work| work.proof_operations += 1);
            let result = read_source_row(&mut self.source, &self.attempt.input, next, event_end)?;
            if let Some(row) = result.1 {
                self.bytes +=
                    row.name.as_ref().map_or(0, Vec::len) + row.app.as_ref().map_or(0, Vec::len);
                self.rows.push(row);
            }
            self.next = result.0;
        }
        Ok(())
    }
    fn full(&self) -> bool {
        self.rows.len() == crate::event_observation::BATCH_ROWS_MAX
            || self.bytes + batch::PROGRAM_BYTES_MAX > crate::event_observation::BATCH_BYTES_MAX
    }
    fn write_rows(
        &self,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<Batch> {
        if self.batches.len() >= 4096 {
            return Err(invalid());
        }

        let mut frame = batch::frame(&self.rows, self.attempt.input.captured)
            .map_err(crate::canonical::rows::source_error)?;
        let directory = self.events(ledger)?;
        write_original(
            &directory,
            self.batches.len(),
            &self.rows,
            &mut frame,
            self.attempt.input.captured,
            budget,
            ledger,
        )?;
        Ok(Batch {
            first: self.rows[0].event.number,
            end: self.rows.last().ok_or_else(invalid)?.event.number + 1,
            rows: collector::u64_from_usize(self.rows.len()),
        })
    }
    fn write_index(&self, event_end: u64, ledger: &PublicationLedger) -> io::Result<()> {
        let directory = self.events(ledger)?;
        let index = crate::event_table::index::Index {
            identity: crate::WindowIdentity {
                run: self.attempt.input.run,
                window: 0,
            },
            first: self.attempt.input.losses.event_start,
            end: event_end,
            rows: self.durable_rows,
            identities: self.attempt.input.captured,
            batches: self.batches.clone(),
        };
        let state = publication_control::read_normal(
            &CountedControl {
                root: self.attempt.root,
                ledger: Some(ledger),
            },
            self.attempt.input.run.recording,
        )?;
        let mut work = PublicationWork::default();
        let result = super::publication::write_matching_at(
            &self.attempt,
            &directory,
            "index",
            &mut work,
            &state,
            Some(ledger),
            |mut file| {
                index
                    .write_file(&mut file)
                    .map_err(crate::canonical::rows::source_error)
            },
        );
        ledger.record(&work, &mut PublicationWork::default());
        result?;
        ledger.update(|work| work.full_syncs += 1);
        self.attempt.directory.full_sync()?;
        Ok(())
    }
    fn finished(&self, end: u64) -> bool {
        self.next == end
            || self.durable_rows + collector::u64_from_usize(self.rows.len())
                == collector::u64_from_usize(crate::event_observation::ROWS_MAX)
    }
    fn events(&self, ledger: &PublicationLedger) -> io::Result<PrivateDirectory> {
        ledger.update(|work| work.inventory.open_attempts += 1);
        let mut syncs = 0;
        let result = self
            .attempt
            .directory
            .create_directory_counted("events", &mut syncs);
        ledger.update(|work| work.full_syncs += syncs);
        result
    }
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn memory() -> io::Error {
    io::ErrorKind::OutOfMemory.into()
}

fn owner_bytes() -> usize {
    size_of::<Construction>()
        + size_of::<RowPublication>()
        + size_of::<super::LossPublication>()
        + size_of::<FiniteEvents>()
        + 16 * 1024
        + 256
}

fn finite_endpoint(
    source: &mut SourceProof,
    input: &AttemptInput,
    budget: &Arc<ByteBudget>,
) -> io::Result<(u64, u64)> {
    let event_first = input.losses.event_start;
    let (_, _, events) = finite_publication(source, input, budget)?;
    let event_end = events.start;
    source.checked_read(input, |reader| {
        let produced = reader.event_newest()?.checked_add(1).ok_or_else(invalid)?;
        if event_end < event_first || events.end < event_end || events.end > produced {
            return Err(invalid());
        }

        if event_end == event_first {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "empty event prefix uses ordinary registration",
            ));
        }
        reader.seek_event(event_first)?;
        let source_first = match reader.read_event()? {
            FoundEvent::Missed { first, end } if first == event_first && end > event_first => {
                end.min(event_end)
            }
            FoundEvent::Event { number, .. } if number == event_first => event_first,
            _ => return Err(io::ErrorKind::WouldBlock.into()),
        };
        if event_end - source_first > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
        {
            return Err(invalid());
        }

        Ok((event_end, source_first))
    })
}
fn finite_publication(
    source: &mut SourceProof,
    input: &AttemptInput,
    budget: &Arc<ByteBudget>,
) -> io::Result<(u64, u64, std::ops::Range<u64>)> {
    let publication_first = input.losses.publication_start;
    let shape = source.shape();
    let limits = crate::Limits::from_shape(&shape).map_err(crate::canonical::rows::source_error)?;
    let bytes = super::normal_proof::tick_workspace_bytes(&limits);
    let _workspace = budget.reserve(bytes).ok_or_else(memory)?;
    let mut tick = crate::tick_buffer(&shape).map_err(crate::canonical::rows::source_error)?;
    let first = publication_first.max(input.binding.publication_first);
    let found = source.checked_read(input, |reader| {
        reader.seek_publication(first)?;
        reader.read_into(&mut tick)
    })?;
    let publication = match found {
        Found::Missed {
            first: observed,
            end,
        } if observed == first && end > first => {
            let found = source.checked_read(input, |reader| {
                reader.seek_publication(end)?;
                reader.read_into(&mut tick)
            })?;
            if !matches!(found, Found::Publication { publication, .. } if publication == end) {
                return Err(io::ErrorKind::WouldBlock.into());
            }

            end
        }
        Found::Publication { publication, .. } if publication == first => publication,
        _ => return Err(io::ErrorKind::WouldBlock.into()),
    };
    input
        .source
        .window(tick.run_tick, tick.times.map(|time| time.continuous_ns))?;
    let produced = source.checked_read(input, |reader| {
        reader.event_newest()?.checked_add(1).ok_or_else(invalid)
    })?;
    if tick.events.end < tick.events.start || tick.events.end > produced {
        return Err(invalid());
    }

    Ok((publication, tick.run_tick, tick.events))
}

fn finite_input(
    store: &Store,
    reader: &RingReader,
    prepared: FrontierInput,
    budget: &Arc<ByteBudget>,
    ledger: &PublicationLedger,
) -> io::Result<AttemptInput> {
    let run = reader.identity().ok_or_else(invalid)?;
    if run.recording != store.recording {
        return Err(invalid());
    }

    let frontier = if let FrontierInput::Prepared(frontier) = prepared {
        frontier
    } else {
        let mut read_work = crate::canonical::ReadWork::default();
        let result = store.source_frontier(&run, budget, &mut read_work);
        ledger.update(|work| work.canonical.add(read_work));
        result?
    };
    if frontier
        .as_ref()
        .is_some_and(|frontier| frontier.complete() || frontier.trailing_unknown())
    {
        return Err(io::ErrorKind::Unsupported.into());
    }

    let event_first = frontier
        .as_ref()
        .map_or(reader.event_first(), SourceFrontier::event_end);
    let publication_first = frontier
        .as_ref()
        .map_or(reader.publication_first(), |frontier| {
            frontier.publication_end()
        });
    ledger.update(|work| work.source_observations += 1);
    let binding = reader.object_binding()?;
    let input = AttemptInput {
        run,
        captured: reader.dictionary_capacity().is_some(),
        source: SourceTiming::from_reader(reader),
        binding,
        operation: AttemptOperation::Finite {
            event_end: event_first.checked_add(1).ok_or_else(invalid)?,
            order_floor: frontier.as_ref().map_or(0, SourceFrontier::order_floor),
            source_first: event_first,
        },
        losses: finite_input_losses(frontier.as_ref(), event_first, publication_first),
    };
    if event_first < input.binding.event_first {
        return Err(invalid());
    }

    if let Some(frontier) = frontier {
        frontier.matches_input(&input)?;
    }
    Ok(input)
}

fn finite_input_losses(
    frontier: Option<&SourceFrontier>,
    event_first: u64,
    publication_first: u64,
) -> LossTransfer {
    LossTransfer {
        predecessor: frontier.map_or(Predecessor::Genesis, |frontier| {
            frontier.predecessor().clone()
        }),
        publication_start: publication_first,
        publication_inherited_end: publication_first,
        publications: 0,
        event_start: event_first,
        event_inherited_end: event_first,
        events: 0,
        unknown: frontier.and_then(|frontier| frontier.unknown_provenance().cloned()),
        prior_window: frontier.and_then(SourceFrontier::prior_window),
    }
}

fn write_original(
    directory: &PrivateDirectory,
    ordinal: usize,
    rows: &[batch::Row],
    frame: &mut polars::prelude::DataFrame,
    captured: bool,
    budget: &Arc<ByteBudget>,
    ledger: &PublicationLedger,
) -> io::Result<()> {
    if ordinal >= 4096 {
        return Err(invalid());
    }

    let name = crate::event_recovery::batch_name(ordinal);
    let identity =
        super::row_candidates::write_batch(directory, &name, rows, frame, captured, budget, ledger)
            .map_err(crate::canonical::rows::source_error)?;
    ledger.update(|work| work.inventory.open_attempts += 1);
    let file = directory.open_file(&name)?;
    if super::FileIdentity::read(&file)? != identity {
        return Err(invalid());
    }

    ledger.update(|work| work.full_syncs += 1);
    private::full_sync(&file)?;
    ledger.update(|work| work.full_syncs += 1);
    directory.full_sync()?;
    Ok(())
}

fn read_source_row(
    source: &mut SourceProof,
    input: &AttemptInput,
    next: u64,
    event_end: u64,
) -> io::Result<(u64, Option<batch::Row>)> {
    source.checked_read(input, |reader| {
        reader.seek_event(next)?;
        let mut identity = collector::ring::IdentityObservation::EMPTY;
        let found = if input.captured {
            reader.read_event_identity(&mut identity)?
        } else {
            reader.read_event()?
        };
        match found {
            FoundEvent::Event {
                number,
                tick,
                event,
            } if number == next => {
                let row = batch::copy_row(
                    reader,
                    &crate::NumberedEvent {
                        number,
                        tick,
                        event: &event,
                    },
                    input.captured.then_some(&identity),
                )
                .map_err(crate::canonical::rows::source_error)?;
                Ok((next.checked_add(1).ok_or_else(invalid)?, Some(row)))
            }
            FoundEvent::Missed { first, end } if first == next && end > first => {
                Ok((end.min(event_end), None))
            }
            _ => Err(io::ErrorKind::WouldBlock.into()),
        }
    })
}
