//! Allocation-free ring mapping and reads into storage supplied by the caller.

use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;

use super::{
    Found, FoundEvent, HEADER_PUBLICATION_NEWEST, LockedName, Opened, RunIdentity, Shape, Tick,
    layout, read_identity, read_newest,
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
            opened: Opened::open_named(lock_path)?,
        })
    }

    /// Opens an explicitly registered retained generation without consulting a lock file.
    pub fn open_generation(name: &str) -> io::Result<Self> {
        Ok(Self {
            opened: Opened::open_name(LockedName::from_str(name)?)?,
        })
    }

    /// The exact wire layout, including legacy2/3 versus optional identity4.
    #[must_use]
    pub fn source_layout_version(&self) -> u64 {
        self.opened.layout.version
    }

    #[must_use]
    pub fn dictionary_capacity(&self) -> Option<super::DictionaryCapacity> {
        let layout = &self.opened.layout;
        if layout.version != layout::LAYOUT_VERSION_IDENTITY {
            return None;
        }

        Some(super::DictionaryCapacity {
            entries: crate::u64_from_usize(layout.dictionary.capacity),
            arena_bytes: crate::u64_from_usize(layout.arena_bytes),
        })
    }

    /// Committed immutable entries and consumed arena bytes, including alignment padding.
    pub fn dictionary_usage(&self) -> io::Result<Option<super::DictionaryUsage>> {
        super::identity::dictionary_usage(self.opened.mapping.words(), &self.opened.layout)
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
        if self.opened.layout.version < layout::LAYOUT_VERSION_OBSERVED {
            return None;
        }
        let words = self.opened.mapping.words();
        match words[layout::HEADER_ORIGIN_STATE].load(Ordering::Relaxed) {
            layout::ORIGIN_IDENTIFIED | layout::ORIGIN_UNREGISTERED => {
                Some(words[layout::HEADER_RUN_START_CONTINUOUS_NS].load(Ordering::Relaxed))
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn completed(&self) -> bool {
        if self.opened.layout.version < layout::LAYOUT_VERSION_OBSERVED {
            return false;
        }
        self.opened.mapping.words()[layout::HEADER_COMPLETED].load(Ordering::Acquire) == 1
    }

    /// Reads into caller storage. Only a returned publication makes its contents valid.
    /// A capacity refusal leaves the publication cursor unchanged.
    pub fn read_into(&mut self, tick: &mut Tick) -> io::Result<Found> {
        self.opened.read_into(tick)
    }

    /// Reads numeric and identity rows under the same publication sequence check.
    /// Identity rows are ordered by PID; measured rows follow numeric process order.
    pub fn read_identity_into(
        &mut self,
        tick: &mut Tick,
        identities: &mut Vec<super::IdentityObservation>,
    ) -> io::Result<Found> {
        self.opened.read_into_identity(tick, Some(identities))
    }

    /// Copies immutable bytes from a reference decoded by this exact reader handle.
    /// Reopened handles reject the old reference, even when they map the same generation.
    pub fn copy_program(
        &self,
        program: &super::ProgramReference,
        buffers: &mut super::ProgramBuffers,
    ) -> io::Result<(usize, usize)> {
        if self.opened.layout.version != layout::LAYOUT_VERSION_IDENTITY {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }

        super::identity::copy_program(
            self.opened.mapping.words(),
            &self.opened.layout,
            program,
            self.opened.reference_owner,
            buffers,
        )
    }

    /// Reads event metadata under the event's own sequence, independent of slot lifetime.
    /// Only a returned event makes the caller's identity output valid.
    pub fn read_event_identity(
        &mut self,
        identity: &mut super::IdentityObservation,
    ) -> io::Result<FoundEvent> {
        let words = self.opened.mapping.words();
        let newest = read_newest(&words[layout::HEADER_EVENT_NEWEST])?;
        let number = match self
            .opened
            .events
            .step(self.opened.layout.events_held(newest))
        {
            super::Step::Nothing => return Ok(FoundEvent::Nothing),
            super::Step::Missed { first, end } => {
                return Ok(FoundEvent::Missed { first, end });
            }
            super::Step::Read(number) => number,
        };
        let record = &words[self.opened.layout.event(number)];
        let loaded = super::read_finished(record, layout::EVENT_SEQUENCE, number, || {
            let (tick, event) = super::load_event(&record[..layout::EVENT_WORDS])?;
            match super::identity::load_event_identity(
                &super::identity::IdentityRecord { words, record },
                &self.opened.layout,
                &event,
                self.opened.reference_owner,
                identity,
            ) {
                Ok(()) => Some((tick, event)),
                Err(_) => None,
            }
        })?;

        match loaded {
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

    #[must_use]
    pub fn next_event(&self) -> u64 {
        self.opened.events.next
    }

    pub fn read_event(&mut self) -> io::Result<FoundEvent> {
        self.opened.read_event()
    }
}
