use collector::coalitions::Coalition;
use collector::interfaces::Interface;
use collector::machine::{CPUTicks, Memory};
use collector::processes::Usage;
use collector::ring::{Dropped, RunIdentity, Shape, Tick};
use collector::tracker::{Event, Noticed};

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
}

pub(crate) struct StagedTick {
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
            bytes: 0,
        }
    }

    #[must_use]
    pub fn tick_count(&self) -> u64 {
        collector::u64_from_usize(self.ticks.len())
    }

    #[must_use]
    pub fn staged_bytes(&self) -> u64 {
        self.bytes
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
        self.push_tick_validate(observation, tick, events)?;
        let bytes = staging_bytes(tick, events.len());
        let total = self
            .bytes
            .checked_add(bytes)
            .ok_or(Error::Bound("staging bytes"))?;
        if total > 768 * 1024 * 1024 {
            return Err(Error::Bound("staging bytes"));
        }
        self.ticks.push(StagedTick::copy(observation, tick, events));
        self.bytes = total;
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
        }
    }
}

fn staging_bytes(tick: &Tick, event_count: usize) -> u64 {
    let mut bytes = size_of::<StagedTick>()
        + tick.cpus.len() * size_of::<CPUTicks>()
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
