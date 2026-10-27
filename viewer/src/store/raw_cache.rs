//! Bounded native raw facts. Readers keep the original allocation reservation alive.
use std::collections::VecDeque;
use std::sync::Arc;

use super::{ByteBudget, ByteReservation};
use crate::live_app_response::Observation;
use crate::live_raw::Raw;

pub(super) const BYTES_MAX: usize = 128 * 1024 * 1024;
const OBSERVATIONS_MAX: usize = crate::session::OBSERVATIONS_MAX;
const ARC_BYTES: usize = 2 * size_of::<usize>();

pub(super) struct RawCache {
    entries: VecDeque<RawReading>,
    bytes: usize,
    maximum: usize,
    reservation: ByteReservation,
}

#[derive(Clone)]
pub(super) struct RawReading {
    pub observation: Observation,
    raw: Arc<Raw>,
}

impl std::fmt::Debug for RawReading {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RawReading")
            .field("observation", &self.observation)
            .field("bytes", &self.raw.bytes())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Admission {
    Budget,
    Observation,
    EntryBytes,
}

impl RawCache {
    pub(super) fn new(budget: &Arc<ByteBudget>) -> Result<Self, Admission> {
        Self::with_limit(budget, BYTES_MAX)
    }

    fn with_limit(budget: &Arc<ByteBudget>, maximum: usize) -> Result<Self, Admission> {
        let bytes = size_of::<Self>() + OBSERVATIONS_MAX * size_of::<RawReading>();
        if bytes > maximum {
            return Err(Admission::EntryBytes);
        }
        let reservation = budget.reserve(bytes).ok_or(Admission::Budget)?;
        let entries = VecDeque::with_capacity(OBSERVATIONS_MAX);
        assert_eq!(entries.capacity(), OBSERVATIONS_MAX);

        Ok(Self {
            entries,
            bytes,
            maximum,
            reservation,
        })
    }

    /// Preflight is pure so other observation facets can commit with the same admission.
    pub(super) fn admit(&self, observation: &Observation, raw: &Raw) -> Result<(), Admission> {
        if raw.observation().as_ref() != Some(observation) {
            return Err(Admission::Observation);
        }
        let bytes = raw.bytes() + ARC_BYTES;
        if bytes > self.maximum - self.reservation.bytes() {
            return Err(Admission::EntryBytes);
        }

        Ok(())
    }

    /// Admission is checked before eviction, so a refused candidate preserves accepted data.
    pub(super) fn insert(
        &mut self,
        observation: Observation,
        raw: Arc<Raw>,
    ) -> Result<(), Admission> {
        self.admit(&observation, &raw)?;
        if self.find(&observation).is_some() {
            return Ok(());
        }
        let bytes = raw.bytes() + ARC_BYTES;

        for _ in 0..=OBSERVATIONS_MAX {
            if self.entries.len() < OBSERVATIONS_MAX {
                if self.bytes <= self.maximum - bytes {
                    self.bytes += bytes;
                    self.entries.push_back(RawReading { observation, raw });
                    assert!(self.bytes <= self.maximum);

                    return Ok(());
                }
            }
            let removed = self
                .entries
                .pop_front()
                .unwrap_or_else(|| unreachable!("one admitted raw entry fits the empty cache"));
            self.bytes -= removed.raw.bytes() + ARC_BYTES;
        }

        unreachable!("a bounded raw cache can release every accepted entry");
    }

    pub(super) fn find(&self, observation: &Observation) -> Option<RawReading> {
        self.entries
            .iter()
            .rev()
            .find(|reading| reading.observation == *observation)
            .cloned()
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = self.reservation.bytes();
    }

    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(super) fn len(&self) -> u32 {
        u32::try_from(self.entries.len())
            .unwrap_or_else(|_| unreachable!("raw observations fit the fixed cache slots"))
    }
}

impl RawReading {
    pub(super) fn raw(&self) -> &Raw {
        &self.raw
    }
}

#[cfg(test)]
#[path = "raw_cache_tests.rs"]
mod tests;
