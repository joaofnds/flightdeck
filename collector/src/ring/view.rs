//! Allocation-free ring mapping and reads into storage supplied by the caller.

use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;

use super::{
    EVENT_SEQUENCE, Found, FoundEvent, HEADER_EVENT_NEWEST, HEADER_PUBLICATION_NEWEST, LockedName,
    Opened, Reader, RunIdentity, SLOT_SEQUENCE, Shape, Step, Tick, layout, load_event, load_tick,
    read_finished, read_identity, read_newest,
};
use crate::coalitions::Coalition;
use crate::interfaces::Interface;
use crate::machine::CPUTicks;
use crate::processes::Process;

/// Storage allocated by the caller, outside shared-library generation changes.
pub struct TickBuffers {
    pub cpus: Vec<CPUTicks>,
    pub interfaces: Vec<Interface>,
    pub gpu_active_ns: Vec<u64>,
    /// Initialized to at least core capacity times `state_capacity` words.
    pub core_active_ns: Vec<u64>,
    pub core_state_counts: Vec<usize>,
    pub state_capacity: usize,
    pub fan_rpm: Vec<f32>,
    pub processes: Vec<Process>,
    pub coalitions: Vec<Coalition>,
}

/// A mapping and cursors, without owned reading buffers.
pub struct RingReader {
    opened: Opened,
}

impl RingReader {
    /// Positions the next read without rescanning earlier publications.
    pub fn seek_publication(&mut self, next: u64) -> io::Result<()> {
        if next == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if next > super::NUMBER_MAX {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        self.opened.publications.next = next;
        Ok(())
    }

    pub fn seek_event(&mut self, next: u64) -> io::Result<()> {
        if next == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if next > super::NUMBER_MAX {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        self.opened.events.next = next;
        Ok(())
    }

    #[must_use]
    pub fn next_publication(&self) -> u64 {
        self.opened.publications.next
    }

    #[must_use]
    pub fn publication_first(&self) -> u64 {
        self.opened.publications.first
    }

    pub fn publication_newest(&self) -> io::Result<u64> {
        read_newest(&self.opened.mapping.words()[HEADER_PUBLICATION_NEWEST])
    }

    #[must_use]
    pub fn mapped_size_bytes(&self) -> usize {
        self.opened.layout.size_bytes()
    }

    /// Opens the current ring without allocating reading storage.
    pub fn open(lock_path: &Path) -> io::Result<Self> {
        Ok(Self {
            opened: Reader::open_named(lock_path)?,
        })
    }

    /// Opens an explicitly registered retained generation without consulting a lock file.
    pub fn open_generation(name: &str) -> io::Result<Self> {
        Ok(Self {
            opened: Reader::open_name(LockedName::from_str(name)?)?,
        })
    }

    #[must_use]
    pub fn shape(&self) -> Shape {
        self.opened.layout.shape
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.opened.name.as_str()
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.opened.mapping.words()[layout::HEADER_GENERATION].load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn identity(&self) -> Option<RunIdentity> {
        read_identity(self.opened.mapping.words(), &self.opened.layout)
    }

    #[must_use]
    pub fn run_start_continuous_ns(&self) -> Option<u64> {
        if self.identity().is_some() {
            Some(self.opened.mapping.words()[30].load(Ordering::Relaxed))
        } else {
            None
        }
    }

    #[must_use]
    pub fn completed(&self) -> bool {
        if self.opened.layout.version < 3 {
            return false;
        }
        self.opened.mapping.words()[layout::HEADER_COMPLETED].load(Ordering::Acquire) == 1
    }

    /// Reads into caller storage. Only a returned publication makes its contents valid.
    /// A capacity refusal leaves the publication cursor unchanged.
    pub fn read_into(&mut self, tick: &mut Tick) -> io::Result<Found> {
        if !tick.fits(&self.opened.layout) {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let words = self.opened.mapping.words();
        let newest = read_newest(&words[HEADER_PUBLICATION_NEWEST])?;
        let publication = match self
            .opened
            .publications
            .step(self.opened.layout.publications_held(newest))
        {
            Step::Nothing => return Ok(Found::Nothing),
            Step::Missed { first, end } => return Ok(Found::Missed { first, end }),
            Step::Read(publication) => publication,
        };
        let slot = &words[self.opened.layout.slot(publication)];
        let loaded = read_finished(slot, SLOT_SEQUENCE, publication, || {
            load_tick(slot, &self.opened.layout, tick).then_some(())
        })?;
        if loaded.is_some() {
            Ok(Found::Publication {
                publication,
                tick: tick.run_tick,
            })
        } else {
            Ok(Found::Missed {
                first: publication,
                end: publication + 1,
            })
        }
    }

    #[must_use]
    pub fn next_event(&self) -> u64 {
        self.opened.events.next
    }

    pub fn read_event(&mut self) -> io::Result<FoundEvent> {
        let words = self.opened.mapping.words();
        let newest = read_newest(&words[HEADER_EVENT_NEWEST])?;
        let number = match self
            .opened
            .events
            .step(self.opened.layout.events_held(newest))
        {
            Step::Nothing => return Ok(FoundEvent::Nothing),
            Step::Missed { first, end } => return Ok(FoundEvent::Missed { first, end }),
            Step::Read(number) => number,
        };
        let record = &words[self.opened.layout.event(number)];
        match read_finished(record, EVENT_SEQUENCE, number, || load_event(record))? {
            Some((tick, event)) => Ok(FoundEvent::Event {
                number,
                tick,
                event,
            }),
            None => Ok(FoundEvent::Missed {
                first: number,
                end: number + 1,
            }),
        }
    }
}
