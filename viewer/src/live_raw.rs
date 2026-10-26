//! Owned native facts of one accepted publication. Nothing here refers to a ring dictionary.
//! Machine rows reuse the recording decoder's vocabulary. Raw tasks, coalitions and numbered
//! events stay separate from App attribution, which is not a lossless raw observation.
use collector::coalitions::Coalition;
use collector::processes::Process;
use collector::ring::{ProcessCreation, RingReader, Tick};
use persistence::{Error, Result};

use crate::cancellation::Operation;
use crate::recording::MachineMinute;

#[path = "live_raw/cells.rs"]
mod cells;
#[path = "live_raw/identity.rs"]
mod identity;
pub(crate) use cells::{Cell, View};
use identity::Identity;

/// A candidate may overlap the 128 MiB accepted cache. App Frame/Sample storage has its own
/// existing bounds; the whole candidate is checked against that same ceiling before commit.
pub(crate) const RAW_BYTES_MAX: usize = 64 * 1024 * 1024;
pub(crate) const PAGE_BYTES_MAX: usize = 32 * 1024 * 1024;
const _: () = {
    // A process/event page has at most two escaped paths and thirty-two integer cells.
    // Its Vec/Option/String metadata and JSON conversion fit independently in PAGE_BYTES_MAX.
    assert!(
        512 * (2 * 4 * persistence::schema::IDENTITY_PATH_BYTES_MAX
            + 32 * (20 + size_of::<Option<String>>() + size_of::<serde_json::Value>()))
            < PAGE_BYTES_MAX
    );
};
const PATH_BYTES: usize = persistence::schema::IDENTITY_PATH_BYTES_MAX;

pub(crate) struct Raw {
    pub machine: MachineMinute,
    pub system: Option<collector::ring::SystemSnapshot>,
    processes: Vec<Process>,
    gpu_ns: Vec<Option<u64>>,
    coalitions: Vec<Coalition>,
    identities: Option<Vec<Identity>>,
    events: Vec<CapturedEvent>,
    arena: Vec<u8>,
    core_count: u32,
    refused: u32,
    gone: u32,
    event_range: std::ops::Range<u64>,
    dropped: collector::ring::Dropped,
}

struct CapturedEvent {
    number: u64,
    tick: u64,
    event: collector::tracker::Event,
    identity: Option<Identity>,
}

impl Raw {
    pub fn capture(
        source: &super::live_apps::Publication<'_>,
        events: &super::live_apps::PublicationEvents<'_>,
        operation: &Operation,
        output: &mut Option<Self>,
    ) -> Result<()> {
        assert!(output.is_none());
        let tick = source.tick;
        capture_validate(source, events)?;
        validate_numbers(tick)?;
        let identity_bytes = identity::preflight(
            source.reader,
            source.identities,
            events.identities,
            operation,
        )?;
        let bytes = capture_bytes(tick, source.identities.len(), events.events.len())?;
        if bytes
            .checked_add(identity_bytes)
            .is_none_or(|bytes| bytes > RAW_BYTES_MAX)
        {
            return Err(Error::Bound("live raw candidate bytes"));
        }

        Self::capture_storage(tick, events, identity_bytes, output)?;
        let raw = output.as_mut().ok_or(Error::Bound("live raw storage"))?;
        let machine_capacity = machine_bytes(&raw.machine);
        raw.machine.push_tick(source.publication, tick);
        assert_eq!(machine_bytes(&raw.machine), machine_capacity);
        if source.reader.dictionary_capacity().is_some() {
            raw.identities = Some(identity::capture(
                source.reader,
                source.identities,
                &mut raw.arena,
                operation,
            )?);
        }
        raw.capture_events(source.reader, events, operation)?;
        assert_eq!(raw.bytes(), bytes + identity_bytes);
        if raw.bytes() > RAW_BYTES_MAX {
            return Err(Error::Bound("live raw retained bytes"));
        }

        // The removed eager core query also ran before acceptance. Check its bounded integer
        // aggregation now, so a refused derived page cannot first expose an invalid candidate.
        let mut view = None;
        raw.view(crate::metrics::core_frequency::CORE_LEVEL_STATES, &mut view)?;
        drop(view);
        operation.check()?;
        Ok(())
    }

    fn capture_storage(
        tick: &Tick,
        events: &super::live_apps::PublicationEvents<'_>,
        identity_bytes: usize,
        output: &mut Option<Self>,
    ) -> Result<()> {
        assert!(output.is_none());
        let core_count = if tick.ioreport_read {
            u32::try_from(tick.core_count()).map_err(|_| Error::Bound("core count"))?
        } else {
            0
        };
        *output = Some(Self {
            machine: MachineMinute::default(),
            system: tick.system,
            processes: tick.processes.clone(),
            gpu_ns: tick.process_gpu_ns().to_vec(),
            coalitions: tick.coalitions.clone(),
            identities: None,
            events: Vec::with_capacity(events.events.len()),
            arena: Vec::with_capacity(identity_bytes),
            core_count,
            refused: tick.processes_refused,
            gone: tick.processes_gone,
            event_range: tick.events.clone(),
            dropped: tick.dropped,
        });

        machine_storage(
            tick,
            &mut output
                .as_mut()
                .ok_or(Error::Bound("live raw storage"))?
                .machine,
        )
    }

    fn capture_events(
        &mut self,
        reader: &RingReader,
        events: &super::live_apps::PublicationEvents<'_>,
        operation: &Operation,
    ) -> Result<()> {
        if self.identities.is_some() {
            if events.identities.len() != events.events.len() {
                return Err(Error::Bound("identity event rows"));
            }
        }
        let mut buffers = ([0; PATH_BYTES], [0; PATH_BYTES]);
        for (index, event) in events.events.iter().enumerate() {
            operation.check()?;
            let mut identity = None;
            if let Some(source) = events.identities.get(index) {
                Identity::capture(reader, source, &mut buffers, &mut self.arena, &mut identity)?;
            }
            self.events.push(CapturedEvent {
                number: event.number,
                tick: event.tick,
                event: copy_event(event.event),
                identity,
            });
        }
        Ok(())
    }

    /// Capacities, not requested lengths, determine the retained charge. Inline data is
    /// charged once in Self; no shared owners or reader-scoped references are retained.
    pub fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.arena.capacity()
            + self.processes.capacity() * size_of::<Process>()
            + self.gpu_ns.capacity() * size_of::<Option<u64>>()
            + self.coalitions.capacity() * size_of::<Coalition>()
            + self
                .identities
                .as_ref()
                .map_or(0, |rows| rows.capacity() * size_of::<Identity>())
            + self.events.capacity() * size_of::<CapturedEvent>()
            + machine_bytes(&self.machine)
    }

    pub fn view<'a>(&'a self, name: &str, output: &mut Option<View<'a>>) -> Result<()> {
        View::native(self, name, output)
    }

    pub fn page(
        &self,
        name: &str,
        offset: u64,
        operation: &Operation,
    ) -> Result<crate::data::Table> {
        let mut view = None;
        self.view(name, &mut view)?;
        let view = view.as_ref().ok_or(Error::Bound("live raw page view"))?;
        let start = usize::try_from(offset).map_err(|_| Error::Bound("page offset"))?;
        if start > view.len() {
            return Err(Error::Bound("page past end"));
        }

        // One ordering index, at most 1,048,576 entries (8 MiB on this target), and one
        // 512-row result bounded by 32 MiB. Neither survives this page operation.
        let order = view.order(operation)?;
        let columns = view.columns();
        let end = (start + 512).min(order.len());
        let mut rows = Vec::with_capacity(end - start);
        let mut bytes = columns.capacity() * size_of::<String>()
            + rows.capacity() * size_of::<Vec<Option<String>>>();
        for column in &columns {
            bytes += column.capacity();
        }
        for index in &order[start..end] {
            operation.check()?;
            let mut row = Vec::with_capacity(columns.len());
            bytes += row.capacity() * size_of::<Option<String>>();
            for column in &columns {
                let cell = view.cell(*index, column)?.format(column)?;
                bytes += cell.as_ref().map_or(0, String::capacity);
                if bytes > PAGE_BYTES_MAX {
                    return Err(Error::Bound("live raw page bytes"));
                }
                row.push(cell);
            }
            rows.push(row);
            #[cfg(test)]
            operation.page_row()?;
        }
        operation.check()?;
        Ok(crate::data::Table { columns, rows })
    }
}

fn capture_validate(
    source: &super::live_apps::Publication<'_>,
    events: &super::live_apps::PublicationEvents<'_>,
) -> Result<()> {
    let limits = persistence::Limits::from_shape(&source.reader.shape())?;
    limits.validate_tick(source.publication, source.tick, events.events)?;
    if source.reader.dictionary_capacity().is_some() {
        limits.validate_identity(
            source.tick,
            events.events,
            &persistence::IdentitySource {
                reader: source.reader,
                processes: source.identities,
                events: events.identities,
            },
        )?;
    } else {
        if !source.tick.process_gpu_ns().is_empty() {
            return Err(Error::Bound("process GPU time without identities"));
        }
    }
    Ok(())
}

fn capture_bytes(tick: &Tick, identities: usize, events: usize) -> Result<usize> {
    let cores = core_state_count(tick)?;
    // Every Vec is reserved once to its final length, including the singleton tick/memory
    // rows. push_tick therefore never invokes Vec's minimum or geometric growth policy.
    Ok(size_of::<Raw>()
        + size_of::<crate::recording::TickRow>()
        + size_of::<crate::recording::MemoryRow>()
        + tick.cpus.len() * size_of::<crate::recording::CpuRow>()
        + tick.interfaces.len() * size_of::<crate::recording::InterfaceRow>()
        + usize::from(tick.ioreport_read)
            * tick.gpu_active_ns.len()
            * size_of::<crate::recording::GpuStateRow>()
        + cores * size_of::<crate::recording::CoreStateRow>()
        + usize::from(tick.smc_read) * tick.fan_rpm.len() * size_of::<crate::recording::FanRow>()
        + tick.processes.len() * size_of::<Process>()
        + size_of_val(tick.process_gpu_ns())
        + tick.coalitions.len() * size_of::<Coalition>()
        + identities * size_of::<Identity>()
        + events * size_of::<CapturedEvent>())
}

fn core_state_count(tick: &Tick) -> Result<usize> {
    let mut count = 0_usize;
    if tick.ioreport_read {
        for core in 0..tick.core_count() {
            count = count
                .checked_add(tick.core_active_ns(core).len())
                .ok_or(Error::Bound("core state count"))?;
        }
    }
    Ok(count)
}

fn machine_storage(tick: &Tick, output: &mut MachineMinute) -> Result<()> {
    assert_eq!(machine_bytes(output), 0);
    let core_states = core_state_count(tick)?;

    output.ticks.reserve_exact(1);
    output.memory.reserve_exact(1);
    output.cpus.reserve_exact(tick.cpus.len());
    output.interfaces.reserve_exact(tick.interfaces.len());
    output
        .gpu_states
        .reserve_exact(usize::from(tick.ioreport_read) * tick.gpu_active_ns.len());
    output.core_states.reserve_exact(core_states);
    output
        .fans
        .reserve_exact(usize::from(tick.smc_read) * tick.fan_rpm.len());

    Ok(())
}

fn machine_bytes(machine: &MachineMinute) -> usize {
    machine.ticks.capacity() * size_of::<crate::recording::TickRow>()
        + machine.cpus.capacity() * size_of::<crate::recording::CpuRow>()
        + machine.memory.capacity() * size_of::<crate::recording::MemoryRow>()
        + machine.interfaces.capacity() * size_of::<crate::recording::InterfaceRow>()
        + machine.gpu_states.capacity() * size_of::<crate::recording::GpuStateRow>()
        + machine.core_states.capacity() * size_of::<crate::recording::CoreStateRow>()
        + machine.fans.capacity() * size_of::<crate::recording::FanRow>()
}

fn validate_numbers(tick: &Tick) -> Result<()> {
    if tick.smc_read {
        for number in [Some(tick.power_watts), Some(tick.soc_celsius)] {
            if number.is_some_and(|number| !number.is_finite()) {
                return Err(Error::Bound("unsupported or nonfinite cell"));
            }
        }
    }
    for number in [tick.cpu_celsius, tick.gpu_celsius].into_iter().flatten() {
        if collector::smc::usable_celsius(number).is_none() {
            return Err(Error::Bound("temperature reading"));
        }
    }
    if let Some(system) = &tick.system {
        validate_system(system)?;
    }
    if let Some(disks) = &tick.disks {
        let mut words = [0; collector::disks::encoding::WORDS_MAX];
        collector::disks::encoding::encode(disks, &mut words)
            .map_err(|_| Error::Bound("physical disk snapshot"))?;
    }
    Ok(())
}

fn copy_event(event: &collector::tracker::Event) -> collector::tracker::Event {
    use collector::tracker::Event;
    match event {
        Event::Start { pid, coalition } => Event::Start {
            pid: *pid,
            coalition: *coalition,
        },
        Event::Exec { pid } => Event::Exec { pid: *pid },
        Event::Exit { pid, last, noticed } => Event::Exit {
            pid: *pid,
            last: *last,
            noticed: *noticed,
        },
    }
}

fn validate_system(system: &collector::ring::SystemSnapshot) -> Result<()> {
    if let Some(averages) = system.reading.load_average {
        for average in averages {
            if !average.is_finite() {
                return Err(Error::Bound("system load average"));
            }
            if average < 0.0 {
                return Err(Error::Bound("system load average"));
            }
        }
    }
    let machine = &system.machine;
    for text in [
        machine.model.as_ref(),
        machine.chip.as_ref(),
        machine.architecture.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if text.as_str().is_empty() {
            return Err(Error::Bound("machine fact text"));
        }
    }
    if let Some(levels) = &machine.levels {
        for level in levels {
            if level.name.as_str().is_empty() {
                return Err(Error::Bound("machine fact text"));
            }
        }
    }
    Ok(())
}
