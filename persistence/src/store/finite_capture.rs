//! One bounded retained source batch. No count or writable-original authority.
use super::{
    Arc, AttemptInput, ByteBudget, ByteReservation, CapturedRows, CountedControl, FiniteEvents,
    FrontierInput, PublicationLedger, PublicationWork, RingReader, SourceProof, Store, batch,
    finite_input, finite_publication, invalid, io, memory, publication_control, read_source_row,
};

pub struct FiniteEventRows<'store> {
    store: &'store Store,
    input: AttemptInput,
    source: Option<SourceProof>,
    rows: Option<Vec<batch::Row>>,
    reservation: Option<ByteReservation>,
    budget: Arc<ByteBudget>,
    ledger: PublicationLedger,
    recorded: PublicationWork,
    publication: u64,
    tick: u64,
    events: std::ops::Range<u64>,
    next: u64,
    bytes: usize,
    ready: bool,
}
impl Store {
    /// Retains actual source rows and their proved association before finite registration.
    pub fn capture_finite_events(
        &self,
        reader: RingReader,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<FiniteEventRows<'_>> {
        ledger.require_budget(budget)?;

        let reservation = budget.reserve(batch::WORKING_BYTES).ok_or_else(memory)?;
        let control = CountedControl {
            root: &self.directory,
            ledger: Some(ledger),
        };
        let mut work = PublicationWork::default();
        let result = publication_control::read_optional_normal(&control, self.recording, &mut work);
        ledger.record(&work, &mut PublicationWork::default());
        if result?.is_some()
            || publication_control::pending_finite(&control, self.recording)?.is_some()
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        let input = finite_input(self, &reader, FrontierInput::Read, budget, ledger)?;
        let source_reservation = budget.reserve(64 * 1024).ok_or_else(memory)?;
        let result = SourceProof::new(reader, &input, source_reservation);
        ledger.update(|work| work.source_observations += 1);
        let mut source = result?;
        source.observations.set(0);
        let result = finite_publication(&mut source, &input, budget);
        ledger.update(|work| {
            work.source_reads += source.reads;
            work.source_observations += source.observations.get();
        });
        let (publication, tick, events) = result?;
        let recorded = PublicationWork {
            source_reads: source.reads,
            source_observations: source.observations.get(),
            ..Default::default()
        };
        let next = input.losses.event_start;
        Ok(FiniteEventRows {
            store: self,
            input,
            source: Some(source),
            rows: Some(Vec::with_capacity(crate::event_observation::BATCH_ROWS_MAX)),
            reservation: Some(reservation),
            budget: Arc::clone(budget),
            ledger: ledger.clone(),
            recorded,
            publication,
            tick,
            events,
            next,
            bytes: batch::ROWS_BYTES_MAX,
            ready: false,
        })
    }
}
impl<'store> FiniteEventRows<'store> {
    pub fn step(&mut self) -> io::Result<bool> {
        let result = self.step_inner();
        self.record();
        result
    }
    fn step_inner(&mut self) -> io::Result<bool> {
        if self.ready {
            return Ok(true);
        }

        let source = self.source.as_mut().ok_or_else(invalid)?;
        let rows = self.rows.as_mut().ok_or_else(invalid)?;
        for _ in 0..128 {
            if self.next >= self.events.end
                || rows.len() == crate::event_observation::BATCH_ROWS_MAX
                || self.bytes + batch::PROGRAM_BYTES_MAX > crate::event_observation::BATCH_BYTES_MAX
            {
                self.ready = true;
                return Ok(true);
            }

            self.ledger.update(|work| work.proof_operations += 1);
            let position = self.next;
            let (next, mut row) = read_source_row(source, &self.input, position, self.events.end)?;
            if let Some(row) = &mut row {
                if self.events.contains(&row.event.number) {
                    if row.event.tick != self.tick {
                        return Err(invalid());
                    }

                    row.publication = Some(self.publication);
                }
            }
            if let Some(row) = row {
                self.bytes +=
                    row.name.as_ref().map_or(0, Vec::len) + row.app.as_ref().map_or(0, Vec::len);
                rows.push(row);
            }
            self.next = next;
        }
        self.ready = self.next == self.events.end
            || rows.len() == crate::event_observation::BATCH_ROWS_MAX
            || self.bytes + batch::PROGRAM_BYTES_MAX > crate::event_observation::BATCH_BYTES_MAX;
        Ok(self.ready)
    }
    /// Revalidates the predecessor and selects the current finite endpoint before registration.
    pub fn register(mut self) -> io::Result<FiniteEvents<'store>> {
        if !self.ready {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        self.record();
        let reader = self.source.take().ok_or_else(invalid)?.into_reader();
        let retained = CapturedRows {
            input: self.input.clone(),
            rows: self.rows.take().ok_or_else(invalid)?,
            bytes: self.bytes,
            reservation: self.reservation.take().ok_or_else(invalid)?,
        };
        self.store.begin_finite_inner(
            reader,
            &self.budget,
            &self.ledger,
            FrontierInput::Read,
            Some(retained),
            None,
        )
    }
    fn record(&mut self) {
        if let Some(source) = &self.source {
            self.ledger.record(
                &PublicationWork {
                    source_reads: source.reads,
                    source_observations: source.observations.get(),
                    ..Default::default()
                },
                &mut self.recorded,
            );
        }
    }
}
impl Drop for FiniteEventRows<'_> {
    fn drop(&mut self) {
        self.record();
    }
}
