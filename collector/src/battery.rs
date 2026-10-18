//! Battery readings share a continuous timestamp so consumers can reject stale values.

mod native;

/// A startup-owned source, cached keys and one fixed-size reading.
pub struct Battery(Box<Storage>);

const _: () = assert!(size_of::<Battery>() <= 16);

struct Storage {
    source: Box<dyn BatterySource>,
    reading: BatteryReading,
    attempted_ns: Option<u64>,
    state: State,
}

/// An absent battery and a failed read never become a fabricated zero reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Pending,
    Present,
    Absent,
    Failed,
}

/// Errors retain native numeric status without allocating a collection-loop message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Allocation,
    Native(i32),
    MultipleBatteries,
    InvalidSnapshot,
    InvalidPresence,
    TimeReversed,
}

/// A startup-owned battery snapshot source writes into the collector's fixed reading.
pub trait BatterySource {
    fn read_into(&mut self, now_ns: u64, reading: &mut BatteryReading) -> Result<bool, Error>;
}

impl Battery {
    /// Acquires the service and caches every CF key before collection begins.
    pub fn open() -> Result<Self, Error> {
        Ok(Self::from_source(Box::new(native::Source::open()?)))
    }

    /// Takes ownership of a source before collection begins; refresh never allocates storage.
    #[must_use]
    pub fn from_source(source: Box<dyn BatterySource>) -> Self {
        Self(Box::new(Storage {
            source,
            reading: BatteryReading::default(),
            attempted_ns: None,
            state: State::Pending,
        }))
    }

    /// Reads immediately, then at most once per sixty continuous seconds.
    /// A scheduled read clears the preceding value even when acquisition fails.
    pub fn refresh(&mut self, now_ns: u64) -> Result<bool, Error> {
        if !self.0.refresh_due(now_ns)? {
            return Ok(false);
        }

        self.0.refresh_begin(now_ns);
        let present = self.0.source.read_into(now_ns, &mut self.0.reading)?;
        self.0.state = if present {
            State::Present
        } else {
            State::Absent
        };

        Ok(true)
    }

    /// The timestamp is meaningful only within this source run and boot.
    #[must_use]
    pub fn reading(&self, now_ns: u64) -> Option<&BatteryReading> {
        if self.0.state == State::Present {
            if self.0.reading.is_fresh_at(now_ns) {
                return Some(&self.0.reading);
            }
        }

        None
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.0.state
    }
}

impl Storage {
    fn refresh_due(&self, now_ns: u64) -> Result<bool, Error> {
        match self.attempted_ns {
            Some(previous) => now_ns
                .checked_sub(previous)
                .map(|elapsed| elapsed >= READ_INTERVAL_NS)
                .ok_or(Error::TimeReversed),
            None => Ok(true),
        }
    }

    fn refresh_begin(&mut self, now_ns: u64) {
        self.attempted_ns = Some(now_ns);
        self.state = State::Failed;
        self.reading = BatteryReading::default();
    }
}

/// The collection interval and maximum age of a held battery reading.
pub const READ_INTERVAL_NS: u64 = 60_000_000_000;

const _: () = assert!(READ_INTERVAL_NS > 0);

/// Optional fields are independent; capacity ratios use only their matching source pair.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BatteryReading {
    pub source_continuous_ns: u64,
    pub charge_current: Option<u64>,
    pub charge_maximum: Option<u64>,
    pub full_charge_capacity: Option<u64>,
    pub design_capacity: Option<u64>,
    pub cycle_count: Option<u64>,
    pub charging: Option<bool>,
    pub external_power: Option<bool>,
    pub voltage_mv: Option<u64>,
    pub current_ma: Option<i64>,
    pub adapter_watts: Option<u64>,
}

impl BatteryReading {
    /// Continuous time includes sleep. A future reading is never usable.
    #[must_use]
    pub fn is_fresh_at(&self, now_ns: u64) -> bool {
        now_ns
            .checked_sub(self.source_continuous_ns)
            .is_some_and(|age| age <= READ_INTERVAL_NS)
    }
}

#[cfg(test)]
mod tests;
