//! Bounded physical block-device snapshots, acquired at the collector's tick pace.

pub mod encoding;
mod native;

pub const DISKS_MAX: usize = 16;
pub const NAME_BYTES: usize = 32;

/// Storage and keys are allocated before collection; refresh replaces the whole snapshot.
pub struct Disks(Box<Storage>);

struct Storage {
    source: Box<dyn DiskSource>,
    snapshot: DiskSnapshot,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ScanState {
    Complete,
    #[default]
    Failed,
    Exhausted,
    Partial,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DiskSnapshot {
    pub source_continuous_ns: u64,
    pub state: ScanState,
    pub count: usize,
    pub records: [DiskReading; DISKS_MAX],
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DiskReading {
    pub driver_id: u64,
    pub media_id: u64,
    pub name: [u8; NAME_BYTES],
    pub failed: bool,
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    pub read_operations: Option<u64>,
    pub write_operations: Option<u64>,
    pub read_errors: Option<u64>,
    pub write_errors: Option<u64>,
    pub read_time_ns: Option<u64>,
    pub write_time_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Allocation,
    Native(i32),
    InvalidSnapshot,
    Capacity,
}

pub trait DiskSource {
    fn read_into(&mut self, now_ns: u64, snapshot: &mut DiskSnapshot) -> Result<(), Error>;
}

impl Disks {
    pub fn open() -> Result<Self, Error> {
        Ok(Self::from_source(Box::new(native::Source::open()?)))
    }

    #[must_use]
    pub fn from_source(source: Box<dyn DiskSource>) -> Self {
        Self(Box::new(Storage {
            source,
            snapshot: DiskSnapshot::default(),
        }))
    }

    pub fn refresh(&mut self, now_ns: u64) -> Result<(), Error> {
        self.0.snapshot = DiskSnapshot {
            source_continuous_ns: now_ns,
            ..DiskSnapshot::default()
        };
        if let Err(error) = self.0.source.read_into(now_ns, &mut self.0.snapshot) {
            self.0.snapshot = DiskSnapshot {
                source_continuous_ns: now_ns,
                ..DiskSnapshot::default()
            };

            return Err(error);
        }

        if self.0.snapshot.source_continuous_ns != now_ns {
            self.0.snapshot = DiskSnapshot {
                source_continuous_ns: now_ns,
                ..DiskSnapshot::default()
            };

            return Err(Error::InvalidSnapshot);
        }

        if let Err(error) = encoding::validate(&self.0.snapshot) {
            self.0.snapshot = DiskSnapshot {
                source_continuous_ns: now_ns,
                ..DiskSnapshot::default()
            };

            return Err(error);
        }

        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> &DiskSnapshot {
        &self.0.snapshot
    }
}

const _: () = assert!(size_of::<Disks>() <= 16);
const _: () = assert!(DISKS_MAX == 16);
const _: () = assert!(NAME_BYTES == 32);

#[cfg(test)]
mod tests;
