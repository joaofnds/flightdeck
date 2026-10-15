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

    #[must_use]
    pub fn next_event(&self) -> u64 {
        self.opened.events.next
    }

    pub fn read_event(&mut self) -> io::Result<FoundEvent> {
        self.opened.read_event()
    }
}
