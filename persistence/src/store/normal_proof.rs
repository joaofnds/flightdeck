//! Volatile source complement proof. Only complete immutable metadata persists its result.
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
    pub operations: u64,
    pub frame: Option<DataFrame>,
    pub workspace: Option<ByteReservation>,
    _reservation: ByteReservation,
}
const _: () = assert!(size_of::<SourceProof>() < 64 * 1024);
impl SourceProof {
    pub fn new(
        reader: RingReader,
        input: &AttemptInput,
        reservation: ByteReservation,
    ) -> io::Result<Self> {
        let generation = reader.generation();
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
            operations: 0,
            frame: None,
            workspace: None,
            _reservation: reservation,
        };
        proof.check(input)?;
        Ok(proof)
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
        if self.reader.generation() != self.generation
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
