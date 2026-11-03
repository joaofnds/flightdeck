//! Source complement proof; complete metadata or a fixed count0 plan persists its result.
use crate::canonical::rows::TickSpan;
use crate::resources::ByteReservation;
use crate::transaction::{AttemptInput, AttemptOperation, SourceTiming};
use collector::ring::{Found, FoundEvent, PublicationTiming, RingReader};
use polars::prelude::DataFrame;
use std::io;

const OPERATIONS: usize = 16384;
pub(super) struct SourceProof {
    reader: RingReader,
    generation: u64,
    publication: u64,
    event: u64,
    tick: usize,
    row: usize,
    boundary: u64,
    remaining: usize,
    pub losses: [u64; 4],
    pub reads: u64,
    pub observations: std::cell::Cell<u64>,
    pub operations: u64,
    pub frame: Option<DataFrame>,
    pub workspace: Option<ByteReservation>,
    reservation: ByteReservation,
}
const _: () = assert!(size_of::<SourceProof>() < 64 * 1024);
impl SourceProof {
    pub fn new(
        reader: RingReader,
        input: &AttemptInput,
        reservation: ByteReservation,
    ) -> io::Result<Self> {
        let generation = input.binding.generation;
        let proof = Self {
            reader,
            generation,
            publication: input.losses.publication_start,
            event: input.losses.event_start,
            tick: 0,
            row: 0,
            boundary: 0,
            remaining: OPERATIONS,
            losses: [0; 4],
            reads: 0,
            observations: std::cell::Cell::new(0),
            operations: 0,
            frame: None,
            workspace: None,
            reservation,
        };
        proof.check(input)?;
        Ok(proof)
    }
    pub(super) fn release_workspace(&mut self) {
        self.frame = None;
        self.workspace = None;
        self.reservation.shrink_to(64 * 1024);
    }
    pub(super) fn ordinary_end(
        &mut self,
        input: &AttemptInput,
        events_end: u64,
        budget: &std::sync::Arc<crate::resources::ByteBudget>,
    ) -> io::Result<u64> {
        let end = self.loss_publication_end(input)?;
        let AttemptOperation::Ordinary { window, .. } = input.operation else {
            return Err(invalid());
        };
        self.checked_read(input, |reader| {
            let shape = reader.shape();
            let limits = crate::Limits::from_shape(&shape)
                .map_err(|error| io::Error::other(error.to_string()))?;
            let bytes = 4096
                + limits.cpus * size_of::<collector::machine::CPUTicks>()
                + limits.interfaces * size_of::<collector::interfaces::Interface>()
                + (limits.states + limits.cores * limits.states) * size_of::<u64>()
                + limits.cores * size_of::<usize>()
                + limits.fans * size_of::<f32>()
                + limits.processes * size_of::<collector::processes::Process>()
                + limits.coalitions * size_of::<collector::coalitions::Coalition>();
            let _workspace = budget
                .reserve(bytes)
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
            let mut tick =
                crate::tick_buffer(&shape).map_err(|error| io::Error::other(error.to_string()))?;
            reader.seek_publication(end)?;
            match reader.read_into(&mut tick)? {
                Found::Publication { publication, .. }
                    if publication == end
                        && input
                            .source
                            .window(tick.run_tick, tick.times.map(|time| time.continuous_ns))?
                            > window
                        && tick.events.start == events_end =>
                {
                    Ok(end)
                }
                _ => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        })
    }
    pub fn loss_publication_end(&mut self, input: &AttemptInput) -> io::Result<u64> {
        if matches!(input.operation, AttemptOperation::Terminal { .. }) {
            self.checked_read(input, |reader| {
                super::attempt::validate_terminal_source(input, reader)
            })?;
            return Ok(input.losses.publication_inherited_end);
        }
        let first = input.losses.publication_start;
        if first < input.binding.publication_first {
            return Err(invalid());
        }
        self.checked_read(input, |reader| {
            reader.seek_publication(first)?;
            match reader.read_timing_into(&mut PublicationTiming::default())? {
                Found::Missed {
                    first: observed,
                    end,
                } if observed == first && end > first => Ok(end),
                Found::Publication { publication, .. } if publication == first => Ok(first),
                Found::Nothing
                    if reader.completed()
                        && reader.publication_newest()?.checked_add(1) == Some(first) =>
                {
                    Ok(first)
                }
                _ => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        })
    }
    pub fn loss_event_end(&mut self, input: &AttemptInput) -> io::Result<Option<u64>> {
        if !self.operation() {
            return Ok(None);
        }
        self.checked_read(input, |reader| {
            if !reader.completed() {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            let publication_end = reader
                .publication_newest()?
                .checked_add(1)
                .ok_or_else(invalid)?;
            let end = reader.event_newest()?.checked_add(1).ok_or_else(invalid)?;
            if reader.publication_newest()?.checked_add(1) != Some(publication_end)
                || reader.event_newest()?.checked_add(1) != Some(end)
            {
                return Err(invalid());
            }
            Ok(Some(end))
        })
    }
    pub fn loss_snapshot(
        &mut self,
        input: &AttemptInput,
        end: u64,
    ) -> io::Result<Option<(u64, u64)>> {
        if !self.operation() {
            return Ok(None);
        }
        self.checked_read(input, |reader| {
            reader.seek_event(end)?;
            let first = match reader.read_event()? {
                FoundEvent::Missed { first, end: next } if first == end && next > end => next,
                FoundEvent::Event { number, .. } if number == end => end,
                FoundEvent::Nothing => end,
                _ => return Err(invalid()),
            };
            let newest = reader.event_newest()?.checked_add(1).ok_or_else(invalid)?;
            if newest < first || newest - first > reader.shape().event_capacity {
                return Err(invalid());
            }
            Ok(Some((first, newest)))
        })
    }
    pub fn begin_pass(&mut self) {
        self.remaining = OPERATIONS;
    }
    fn operation(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        self.operations += 1;
        true
    }
    pub fn positions(&self) -> (u64, u64) {
        (self.publication, self.event)
    }
    pub fn check(&self, input: &AttemptInput) -> io::Result<()> {
        self.observations.set(self.observations.get() + 1);
        if self.reader.object_binding()? != input.binding
            || self.reader.generation() != self.generation
            || self.reader.identity() != Some(input.run)
            || SourceTiming::from_reader(&self.reader) != input.source
            || self.reader.dictionary_capacity().is_some() != input.captured
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn checked_read<T>(
        &mut self,
        input: &AttemptInput,
        operation: impl FnOnce(&mut RingReader) -> io::Result<T>,
    ) -> io::Result<T> {
        self.check(input)?;
        self.reads += 1;
        let result = operation(&mut self.reader);
        self.check(input)?;
        result
    }
    fn missed(
        &mut self,
        input: &AttemptInput,
        first: u64,
        end: u64,
        publication: bool,
    ) -> io::Result<u64> {
        let origin = if publication {
            input.binding.publication_first
        } else {
            input.binding.event_first
        };
        if first < origin {
            return Err(invalid());
        }
        let result = self.checked_read(input, |reader| {
            if publication {
                reader.seek_publication(first)?;
                reader
                    .read_timing_into(&mut PublicationTiming::default())
                    .map(|found| match found {
                        Found::Missed { first, end } => Some((first, end)),
                        _ => None,
                    })
            } else {
                reader.seek_event(first)?;
                reader.read_event().map(|found| match found {
                    FoundEvent::Missed { first, end } => Some((first, end)),
                    _ => None,
                })
            }
        });
        let (observed, missed_end) =
            result?.ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
        if observed != first || missed_end <= first {
            return Err(invalid());
        }
        Ok(missed_end.min(end))
    }
    fn charge(&mut self, first: u64, end: u64, inherited: u64, offset: usize) -> io::Result<()> {
        let old = end.min(inherited).saturating_sub(first.min(inherited));
        let new = end
            .checked_sub(first)
            .ok_or_else(invalid)?
            .checked_sub(old)
            .ok_or_else(invalid)?;
        self.losses[offset] = self.losses[offset].checked_add(old).ok_or_else(invalid)?;
        self.losses[offset + 1] = self.losses[offset + 1]
            .checked_add(new)
            .ok_or_else(invalid)?;
        Ok(())
    }
    pub fn publications(
        &mut self,
        input: &AttemptInput,
        ticks: &[TickSpan],
        end: u64,
    ) -> io::Result<bool> {
        self.check(input)?;
        for _ in 0..OPERATIONS {
            if !self.operation() {
                return Ok(false);
            }
            if self.publication == end {
                return Ok(true);
            }
            let next = ticks.get(self.tick).map_or(end, |tick| tick.publication);
            if next < self.publication || next >= end && self.tick < ticks.len() {
                return Err(invalid());
            }
            if next == self.publication {
                self.publication = next.checked_add(1).ok_or_else(invalid)?;
                self.tick += 1;
            } else {
                let proved = self.missed(input, self.publication, next, true)?;
                self.charge(
                    self.publication,
                    proved,
                    input.losses.publication_inherited_end,
                    0,
                )?;
                self.publication = proved;
            }
        }
        Ok(false)
    }
    pub fn events(&mut self, input: &AttemptInput, end: u64) -> io::Result<bool> {
        self.check(input)?;
        for _ in 0..OPERATIONS {
            if !self.operation() {
                return Ok(false);
            }
            let next = if let Some(frame) = &self.frame {
                if self.row == frame.height() {
                    self.frame = None;
                    self.workspace = None;
                    self.row = 0;
                    return Ok(true);
                }
                frame
                    .column("event")
                    .map_err(invalid_value)?
                    .u64()
                    .map_err(invalid_value)?
                    .get(self.row)
                    .ok_or_else(invalid)?
            } else {
                end
            };
            if next < self.event || next > end {
                return Err(invalid());
            }
            if self.frame.is_none() && self.event == end {
                return Ok(true);
            }
            if next == self.event {
                self.event = next.checked_add(1).ok_or_else(invalid)?;
                self.row += 1;
            } else {
                let proved = self.missed(input, self.event, next, false)?;
                self.charge(self.event, proved, input.losses.event_inherited_end, 2)?;
                self.event = proved;
            }
        }
        Ok(false)
    }
    pub fn boundary(&mut self, input: &AttemptInput, end: u64) -> io::Result<bool> {
        self.check(input)?;
        if !self.operation() {
            return Ok(false);
        }
        if matches!(input.operation, AttemptOperation::Terminal { .. }) {
            return self
                .checked_read(input, |reader| {
                    super::attempt::validate_terminal_source(input, reader)
                })
                .map(|()| true);
        }
        let AttemptOperation::Ordinary { window, .. } = input.operation else {
            return Err(invalid());
        };
        if self.boundary == 0 {
            self.boundary = end;
        }
        let boundary = self.boundary;
        let mut timing = PublicationTiming::default();
        let result = self.checked_read(input, |reader| {
            reader.seek_publication(boundary)?;
            reader.read_timing_into(&mut timing)
        });
        match result? {
            Found::Publication { .. }
                if input
                    .source
                    .window(timing.run_tick, timing.times.map(|time| time.continuous_ns))?
                    > window =>
            {
                Ok(true)
            }
            Found::Missed { first, end } if first == self.boundary && end > first => {
                self.boundary = end;
                Ok(false)
            }
            Found::Nothing if self.reader.completed() => {
                let final_end = self
                    .checked_read(input, |reader| reader.publication_newest())?
                    .checked_add(1)
                    .ok_or_else(invalid)?;
                if final_end == end {
                    Ok(true)
                } else {
                    Err(invalid())
                }
            }
            _ => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
fn invalid_value(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
