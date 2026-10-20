use collector::coalitions::Coalition;
use collector::interfaces::Interface;
use collector::machine::{CPUTicks, Memory};
use collector::processes::Usage;
use collector::ring::{Dropped, RunIdentity, Shape, Tick};
use collector::tracker::{Event, Noticed};

use crate::owned_identity::{IdentitySource, OwnedIdentity, Prepared, Programs, usage_code};
use crate::{Error, Result};

/// One elapsed-minute identity, independent of wall-clock changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowIdentity {
    pub run: RunIdentity,
    pub window: u64,
}

/// Actual observation times; legacy sources keep their absent times as `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    pub publication: u64,
    pub wall_ns: Option<u64>,
    pub continuous_ns: Option<u64>,
    pub awake_ns: Option<u64>,
}

pub struct NumberedEvent<'a> {
    pub number: u64,
    pub tick: u64,
    pub event: &'a Event,
}

// A carried late deadline can join sixty scheduled observations in one elapsed minute.
const WINDOW_TICKS_MAX: usize = 61;
const _: () = assert!(WINDOW_TICKS_MAX == 60 + 1);

/// What a window may stage, its ticks, their events and identities, before it seals.
pub const STAGED_BYTES_MAX: u64 = 768 * 1024 * 1024;

/// Validated per-tick section bounds for one elapsed minute.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub(crate) cpus: usize,
    pub(crate) interfaces: usize,
    pub(crate) cores: usize,
    pub(crate) states: usize,
    pub(crate) fans: usize,
    pub(crate) processes: usize,
    pub(crate) coalitions: usize,
    pub(crate) events: usize,
}

pub struct Window {
    pub identity: WindowIdentity,
    pub(crate) limits: Limits,
    pub(crate) ticks: Vec<StagedTick>,
    bytes: u64,
    pub(crate) programs: Option<Box<Programs>>,
}

pub(crate) struct StagedTick {
    pub battery: Option<collector::battery::BatteryReading>,
    pub disks: Option<collector::disks::DiskSnapshot>,
    pub observation: Observation,
    pub run_tick: u64,
    pub memory: Memory,
    pub cpus: Vec<CPUTicks>,
    pub interfaces: Vec<Interface>,
    pub ioreport_read: bool,
    pub gpu_energy_nj: u64,
    pub gpu_active_ns: Vec<u64>,
    pub cores: Vec<Vec<u64>>,
    pub smc_read: bool,
    pub power_watts: f32,
    pub soc_celsius: f32,
    pub fan_rpm: Vec<f32>,
    pub processes: Vec<collector::processes::Process>,
    pub coalitions: Vec<Coalition>,
    pub processes_refused: u32,
    pub processes_gone: u32,
    pub events_first: u64,
    pub events_end: u64,
    pub dropped: Dropped,
    pub events: Vec<EventRecord>,
    pub identities: Option<IdentityRows>,
}

pub(crate) struct IdentityRows {
    pub processes: Vec<OwnedIdentity>,
    pub events: Vec<OwnedIdentity>,
}

pub(crate) struct EventRecord {
    pub number: u64,
    pub tick: u64,
    pub pid: i32,
    pub kind: u8,
    pub coalition: Option<u64>,
    pub usage: Option<Usage>,
    pub availability: u8,
    pub noticed: Option<u8>,
}

impl Limits {
    pub fn from_shape(shape: &Shape) -> Result<Self> {
        let values = [
            shape.cpu_capacity,
            shape.interface_capacity,
            shape.core_capacity,
            shape.state_capacity,
            shape.fan_capacity,
            shape.process_capacity,
            shape.coalition_capacity,
            shape.event_capacity,
        ];
        let maxima = [256, 16_384, 256, 256, 256, 65_536, 131_072, 1_966_080];
        let mut counts = [0_usize; 8];
        for (index, value) in values.into_iter().enumerate() {
            if value > maxima[index] {
                return Err(Error::Bound("ring section capacity"));
            }
            counts[index] = usize::try_from(value).map_err(|_| Error::Bound("capacity"))?;
        }
        Ok(Self {
            cpus: counts[0],
            interfaces: counts[1],
            cores: counts[2],
            states: counts[3],
            fans: counts[4],
            processes: counts[5],
            coalitions: counts[6],
            events: counts[7].min(collector::tracker::EVENTS_PER_TICK_MAX),
        })
    }
}

impl Window {
    #[must_use]
    pub fn new(identity: WindowIdentity, limits: Limits) -> Self {
        Self {
            identity,
            limits,
            ticks: Vec::with_capacity(WINDOW_TICKS_MAX),
            bytes: collector::u64_from_usize(
                size_of::<Self>() + WINDOW_TICKS_MAX * size_of::<StagedTick>(),
            ),
            programs: None,
        }
    }

    #[must_use]
    pub fn tick_count(&self) -> u64 {
        collector::u64_from_usize(self.ticks.len())
    }

    pub(crate) fn battery_captured(&self) -> bool {
        self.ticks.iter().any(|tick| tick.battery.is_some())
    }

    pub(crate) fn disks_captured(&self) -> bool {
        self.ticks.iter().any(|tick| tick.disks.is_some())
    }

    #[must_use]
    pub fn staged_bytes(&self) -> u64 {
        self.bytes
    }

    /// Owned raw-byte occupancy; descriptor/index reservations are in `staged_bytes`.
    #[must_use]
    pub fn identity_program_usage(&self) -> Option<collector::ring::DictionaryUsage> {
        self.programs.as_ref().map(|programs| programs.usage())
    }

    pub fn observations(&self) -> impl Iterator<Item = &Observation> {
        self.ticks.iter().map(|tick| &tick.observation)
    }

    #[must_use]
    pub fn event_range(&self) -> std::ops::Range<u64> {
        match (self.ticks.first(), self.ticks.last()) {
            (Some(first), Some(last)) => first.events_first..last.events_end,
            _ => 0..0,
        }
    }

    #[must_use]
    pub fn publication_range(&self) -> std::ops::Range<u64> {
        match (self.ticks.first(), self.ticks.last()) {
            (Some(first), Some(last)) => {
                first.observation.publication..last.observation.publication + 1
            }
            _ => 0..0,
        }
    }

    pub fn push_tick(
        &mut self,
        observation: &Observation,
        tick: &Tick,
        events: &[NumberedEvent<'_>],
    ) -> Result<()> {
        if self.programs.is_some() {
            return Err(Error::Bound("mixed window identity mode"));
        }

        self.push_tick_validate(observation, tick, events)?;

        let bytes = staging_bytes(tick, events.len());
        let total = self
            .bytes
            .checked_add(bytes)
            .ok_or(Error::Bound("staging bytes"))?;
        if total > STAGED_BYTES_MAX {
            return Err(Error::Bound("staging bytes"));
        }

        self.ticks.push(StagedTick::copy(observation, tick, events));
        self.bytes = total;

        Ok(())
    }

    /// Retains identity bytes while their exact decoding reader is still alive.
    /// A refusal leaves all accepted ticks, dictionary bytes and accounting unchanged.
    pub fn push_tick_with_identity(
        &mut self,
        observation: &Observation,
        tick: &Tick,
        events: &[NumberedEvent<'_>],
        source: &IdentitySource<'_>,
    ) -> Result<()> {
        if self.programs.is_none() {
            if !self.ticks.is_empty() {
                return Err(Error::Bound("mixed window identity mode"));
            }
        }

        self.push_tick_validate(observation, tick, events)?;
        self.push_tick_validate_identity(tick, events, source)?;

        let row_bytes = (source.processes.len() + source.events.len()) * size_of::<OwnedIdentity>();
        let dictionary_bytes = if self.programs.is_none() {
            Programs::allocation_bytes()
        } else {
            0
        };
        let total = self.bytes
            + staging_bytes(tick, events.len())
            + collector::u64_from_usize(row_bytes + dictionary_bytes);
        let peak = total + collector::u64_from_usize(Prepared::peak_bytes(source));
        if peak > STAGED_BYTES_MAX {
            return Err(Error::Bound("staging bytes"));
        }

        let mut prepared = None;
        Prepared::initialize(&mut prepared, source, self.programs.as_deref())?;
        let Some(prepared) = prepared else {
            unreachable!("successful identity preparation initializes output");
        };

        let mut staged = StagedTick::copy(observation, tick, events);
        if self.programs.is_none() {
            Programs::initialize(&mut self.programs);
        }
        let Some(programs) = self.programs.as_mut() else {
            unreachable!("identity dictionary initialized before commit");
        };

        programs.commit(&prepared);
        staged.identities = Some(IdentityRows {
            processes: prepared.processes,
            events: prepared.events,
        });
        self.ticks.push(staged);
        self.bytes = total;

        Ok(())
    }

    fn push_tick_validate_identity(
        &self,
        tick: &Tick,
        events: &[NumberedEvent<'_>],
        source: &IdentitySource<'_>,
    ) -> Result<()> {
        if source.reader.source_layout_version() != 4 {
            return Err(Error::Bound("identity source layout"));
        }

        if source.reader.identity() != Some(self.identity.run) {
            return Err(Error::Bound("identity source run"));
        }

        if source.processes.len() > self.limits.processes {
            return Err(Error::Bound("identity process rows"));
        }

        if source.events.len() != events.len() {
            return Err(Error::Bound("identity event rows"));
        }

        self.push_tick_validate_identity_rows(source)?;
        push_tick_validate_identity_processes(tick, source.processes)?;

        for (identity, event) in source.events.iter().zip(events) {
            let numeric = EventRecord::copy(event);
            if identity.pid != numeric.pid {
                return Err(Error::Bound("identity event pid"));
            }

            if usage_code(identity.usage) != numeric.availability {
                return Err(Error::Bound("identity event usage"));
            }
        }

        Ok(())
    }

    fn push_tick_validate_identity_rows(&self, source: &IdentitySource<'_>) -> Result<()> {
        let mut processes = collector::u64_from_usize(source.processes.len());
        let mut events = collector::u64_from_usize(source.events.len());
        for tick in &self.ticks {
            let Some(identities) = &tick.identities else {
                unreachable!("consistent window identity mode");
            };

            processes += collector::u64_from_usize(identities.processes.len());
            events += collector::u64_from_usize(identities.events.len());
        }

        if processes > crate::schema::IDENTITY_ROWS_MAX {
            return Err(Error::Bound("process identity rows"));
        }

        if events > crate::schema::IDENTITY_ROWS_MAX {
            return Err(Error::Bound("process identity rows"));
        }

        Ok(())
    }

    fn push_tick_validate(
        &self,
        observation: &Observation,
        tick: &Tick,
        events: &[NumberedEvent<'_>],
    ) -> Result<()> {
        if observation.publication == 0 {
            return Err(Error::Bound("publication zero"));
        }
        if observation.publication == u64::MAX {
            return Err(Error::Bound("publication end"));
        }
        if tick.events.start > tick.events.end {
            return Err(Error::Bound("event range"));
        }
        if self.ticks.len() == WINDOW_TICKS_MAX {
            return Err(Error::Bound("window ticks"));
        }
        if let Some(last) = self.ticks.last() {
            if observation.publication <= last.observation.publication {
                return Err(Error::Bound("publication order"));
            }
        }
        let sizes = [
            tick.cpus.len(),
            tick.interfaces.len(),
            tick.core_count(),
            tick.gpu_active_ns.len(),
            tick.fan_rpm.len(),
            tick.processes.len(),
            tick.coalitions.len(),
            events.len(),
        ];
        let bounds = [
            self.limits.cpus,
            self.limits.interfaces,
            self.limits.cores,
            self.limits.states,
            self.limits.fans,
            self.limits.processes,
            self.limits.coalitions,
            self.limits.events,
        ];
        for (size, bound) in sizes.into_iter().zip(bounds) {
            if size > bound {
                return Err(Error::Bound("tick section rows"));
            }
        }
        for index in 0..tick.core_count() {
            if tick.core_active_ns(index).len() > self.limits.states {
                return Err(Error::Bound("core states"));
            }
        }
        validate_events(tick, events)?;
        Ok(())
    }
}

fn validate_events(tick: &Tick, events: &[NumberedEvent<'_>]) -> Result<()> {
    let mut previous = None;
    for event in events {
        if event.number < tick.events.start {
            return Err(Error::Bound("event range"));
        }
        if event.number >= tick.events.end {
            return Err(Error::Bound("event range"));
        }
        if event.tick != tick.run_tick {
            return Err(Error::Bound("event tick"));
        }
        if let Some(previous) = previous {
            if event.number <= previous {
                return Err(Error::Bound("event order"));
            }
        }
        previous = Some(event.number);
    }
    Ok(())
}

impl StagedTick {
    fn copy(observation: &Observation, tick: &Tick, events: &[NumberedEvent<'_>]) -> Self {
        let mut cores = Vec::with_capacity(tick.core_count());
        for index in 0..tick.core_count() {
            cores.push(tick.core_active_ns(index).to_vec());
        }
        Self {
            battery: tick.battery,
            disks: tick.disks,
            observation: *observation,
            run_tick: tick.run_tick,
            memory: tick.memory,
            cpus: tick.cpus.clone(),
            interfaces: tick.interfaces.clone(),
            ioreport_read: tick.ioreport_read,
            gpu_energy_nj: tick.gpu_energy_nj,
            gpu_active_ns: tick.gpu_active_ns.clone(),
            cores,
            smc_read: tick.smc_read,
            power_watts: tick.power_watts,
            soc_celsius: tick.soc_celsius,
            fan_rpm: tick.fan_rpm.clone(),
            processes: tick.processes.clone(),
            coalitions: tick.coalitions.clone(),
            processes_refused: tick.processes_refused,
            processes_gone: tick.processes_gone,
            events_first: tick.events.start,
            events_end: tick.events.end,
            dropped: tick.dropped,
            events: events.iter().map(EventRecord::copy).collect(),
            identities: None,
        }
    }
}

fn staging_bytes(tick: &Tick, event_count: usize) -> u64 {
    let mut bytes = tick.cpus.len() * size_of::<CPUTicks>()
        + tick.interfaces.len() * size_of::<Interface>()
        + tick.gpu_active_ns.len() * 8
        + tick.fan_rpm.len() * 4
        + tick.processes.len() * size_of::<collector::processes::Process>()
        + tick.coalitions.len() * size_of::<Coalition>()
        + event_count * size_of::<EventRecord>();
    for index in 0..tick.core_count() {
        bytes += size_of::<Vec<u64>>() + tick.core_active_ns(index).len() * 8;
    }
    collector::u64_from_usize(bytes)
}

impl EventRecord {
    fn copy(source: &NumberedEvent<'_>) -> Self {
        use crate::schema::{availability, event_kind, noticed};
        let (pid, kind, coalition, usage, availability, noticed) = match source.event {
            Event::Start { pid, coalition } => (
                *pid,
                event_kind::START,
                *coalition,
                None,
                availability::NOT_APPLICABLE,
                None,
            ),
            Event::Exec { pid } => (
                *pid,
                event_kind::EXEC,
                None,
                None,
                availability::NOT_APPLICABLE,
                None,
            ),
            Event::Exit { pid, last, noticed } => {
                let (usage, availability) = match last {
                    Ok(usage) => (Some(*usage), availability::MEASURED),
                    Err(collector::processes::Unreadable::Refused) => (None, availability::REFUSED),
                    Err(collector::processes::Unreadable::Gone) => (None, availability::GONE),
                };
                let noticed = match noticed {
                    Noticed::OnExit => noticed::ON_EXIT,
                    Noticed::AlreadyExited => noticed::ALREADY_EXITED,
                };
                (
                    *pid,
                    event_kind::EXIT,
                    None,
                    usage,
                    availability,
                    Some(noticed),
                )
            }
        };
        Self {
            number: source.number,
            tick: source.tick,
            pid,
            kind,
            coalition,
            usage,
            availability,
            noticed,
        }
    }
}

fn push_tick_validate_identity_processes(
    tick: &Tick,
    identities: &[collector::ring::IdentityObservation],
) -> Result<()> {
    use collector::ring::UsageAvailability;
    let mut measured = 0;
    let mut refused = 0;
    let mut gone = 0;
    let mut previous = None;

    for identity in identities {
        if let Some(previous) = previous {
            if identity.pid <= previous {
                return Err(Error::Bound("identity process order"));
            }
        }

        previous = Some(identity.pid);
        match identity.usage {
            UsageAvailability::Measured => {
                let Some(process) = tick.processes.get(measured) else {
                    return Err(Error::Bound("identity measured process"));
                };

                if process.pid != identity.pid {
                    return Err(Error::Bound("identity measured pid"));
                }

                measured += 1;
            }
            UsageAvailability::Refused => refused += 1,
            UsageAvailability::Gone => gone += 1,
            UsageAvailability::NotApplicable => return Err(Error::Bound("identity process usage")),
        }
    }

    if measured != tick.processes.len() {
        return Err(Error::Bound("identity measured count"));
    }

    if refused != tick.processes_refused {
        return Err(Error::Bound("identity refused count"));
    }

    if gone != tick.processes_gone {
        return Err(Error::Bound("identity gone count"));
    }

    Ok(())
}
