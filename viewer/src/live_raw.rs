//! Owned native facts of one accepted publication. Nothing here refers to a ring dictionary.
//! Machine rows reuse the recording decoder's vocabulary. Raw tasks, coalitions and numbered
//! events stay separate from App attribution, which is not a lossless raw observation.
use collector::coalitions::Coalition;
use collector::processes::Process;
use collector::ring::{ProcessCreation, RingReader, RunIdentity, Tick};
use persistence::{Error, Result};
use std::sync::Arc;

use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::cancellation::Operation;
use crate::recording::MachineMinute;

#[path = "live_raw/cells.rs"]
mod cells;
#[path = "live_raw/identity.rs"]
mod identity;
#[path = "live_raw/page.rs"]
mod page;
pub(crate) use cells::{Cell, View};
use identity::Identity;
pub(crate) use page::AdmittedPage;

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
    identity: Option<RunIdentity>,
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
    reservation: Option<ByteReservation>,
}

#[derive(Debug)]
pub(crate) enum CaptureError {
    Budget,
    Capture(Error),
}

impl From<Error> for CaptureError {
    fn from(error: Error) -> Self {
        Self::Capture(error)
    }
}

struct CapturePlan {
    bytes: usize,
    identity_bytes: usize,
    reservation: Option<ByteReservation>,
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
        match Self::capture_budgeted(source, events, operation, None, output) {
            Ok(()) => Ok(()),
            Err(CaptureError::Budget) => Err(Error::Bound("live raw admission bytes")),
            Err(CaptureError::Capture(error)) => Err(error),
        }
    }

    pub(crate) fn capture_budgeted(
        source: &super::live_apps::Publication<'_>,
        events: &super::live_apps::PublicationEvents<'_>,
        operation: &Operation,
        budget: Option<&Arc<ByteBudget>>,
        output: &mut Option<Self>,
    ) -> std::result::Result<(), CaptureError> {
        assert!(output.is_none());
        let mut candidate = None;
        Self::capture_inner(source, events, operation, budget, &mut candidate)?;
        *output = candidate;

        Ok(())
    }

    fn capture_inner(
        source: &super::live_apps::Publication<'_>,
        events: &super::live_apps::PublicationEvents<'_>,
        operation: &Operation,
        budget: Option<&Arc<ByteBudget>>,
        output: &mut Option<Self>,
    ) -> std::result::Result<(), CaptureError> {
        let tick = source.tick;
        let CapturePlan {
            bytes,
            identity_bytes,
            reservation,
        } = Self::capture_plan(source, events, operation, budget)?;
        Self::capture_storage(tick, events, identity_bytes, reservation, output)?;
        let raw = output.as_mut().ok_or(Error::Bound("live raw storage"))?;
        raw.identity = source.reader.identity();
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
            return Err(Error::Bound("live raw retained bytes").into());
        }

        // The removed eager core query also ran before acceptance. Check its bounded integer
        // aggregation now, so a refused derived page cannot first expose an invalid candidate.
        let scratch = match budget {
            Some(budget) => Some(
                budget
                    .reserve(View::native_scratch_bytes(
                        raw,
                        crate::metrics::core_frequency::CORE_LEVEL_STATES,
                    ))
                    .ok_or(CaptureError::Budget)?,
            ),
            None => None,
        };
        let mut view = None;
        raw.view(crate::metrics::core_frequency::CORE_LEVEL_STATES, &mut view)?;
        drop(view);
        drop(scratch);
        operation.check()?;
        source.record_work(persistence::work::ingestion::Kind::RawCapture);

        Ok(())
    }

    fn capture_plan(
        source: &super::live_apps::Publication<'_>,
        events: &super::live_apps::PublicationEvents<'_>,
        operation: &Operation,
        budget: Option<&Arc<ByteBudget>>,
    ) -> std::result::Result<CapturePlan, CaptureError> {
        source.record_work(persistence::work::ingestion::Kind::RawCaptureAttempt);
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
            return Err(Error::Bound("live raw candidate bytes").into());
        }

        let reservation = match budget {
            Some(budget) => Some(
                budget
                    .reserve(bytes + identity_bytes + 2 * size_of::<usize>())
                    .ok_or(CaptureError::Budget)?,
            ),
            None => None,
        };

        Ok(CapturePlan {
            bytes,
            identity_bytes,
            reservation,
        })
    }

    fn capture_storage(
        tick: &Tick,
        events: &super::live_apps::PublicationEvents<'_>,
        identity_bytes: usize,
        reservation: Option<ByteReservation>,
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
            identity: None,
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
            reservation,
        });

        machine_storage(
            tick,
            &mut output
                .as_mut()
                .ok_or(Error::Bound("live raw storage"))?
                .machine,
        )
    }

    /// The original source and clocks travel with the facts, independently of a cache key.
    pub(crate) fn observation(&self) -> Option<crate::live_app_response::Observation> {
        let identity = self.identity?;
        let [tick] = self.machine.ticks.as_slice() else {
            return None;
        };

        Some(crate::live_app_response::Observation {
            recording: identity.recording.0,
            stamp: crate::apps::Stamp {
                boot: identity.boot.0,
                run: identity.run.0,
                publication: tick.publication,
                continuous_ns: tick.continuous_ns,
                awake_ns: tick.awake_ns,
            },
            run_tick: tick.run_tick,
            wall_ns: tick.wall_ns,
        })
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
        let bytes = size_of::<Self>()
            + self.arena.capacity()
            + self.processes.capacity() * size_of::<Process>()
            + self.gpu_ns.capacity() * size_of::<Option<u64>>()
            + self.coalitions.capacity() * size_of::<Coalition>()
            + self
                .identities
                .as_ref()
                .map_or(0, |rows| rows.capacity() * size_of::<Identity>())
            + self.events.capacity() * size_of::<CapturedEvent>()
            + machine_bytes(&self.machine);
        if let Some(reservation) = &self.reservation {
            assert!(reservation.bytes() >= bytes + 2 * size_of::<usize>());
        }

        bytes
    }

    pub fn view<'a>(&'a self, name: &str, output: &mut Option<View<'a>>) -> Result<()> {
        View::native(self, name, output)
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
    let smc = if tick.smc_read {
        [Some(tick.power_watts), Some(tick.soc_celsius)]
    } else {
        [None; 2]
    };

    validate_machine_numbers(&MachineNumbers {
        smc,
        temperatures: [tick.cpu_celsius, tick.gpu_celsius],
        system: tick.system.as_ref(),
        disks: tick.disks.as_ref(),
    })
}

pub(crate) fn validate_machine_tick(tick: &crate::recording::TickRow) -> Result<()> {
    validate_machine_numbers(&MachineNumbers {
        smc: [tick.power_watts, tick.soc_celsius],
        temperatures: [tick.cpu_celsius, tick.gpu_celsius],
        system: tick.system.as_ref(),
        disks: tick.disks.as_ref(),
    })
}

struct MachineNumbers<'tick> {
    smc: [Option<f32>; 2],
    temperatures: [Option<f32>; 2],
    system: Option<&'tick collector::ring::SystemSnapshot>,
    disks: Option<&'tick collector::disks::DiskSnapshot>,
}

fn validate_machine_numbers(numbers: &MachineNumbers<'_>) -> Result<()> {
    for number in numbers.smc.into_iter().flatten() {
        if !number.is_finite() {
            return Err(Error::Bound("unsupported or nonfinite cell"));
        }
    }
    for number in numbers.temperatures.into_iter().flatten() {
        if collector::smc::usable_celsius(number).is_none() {
            return Err(Error::Bound("temperature reading"));
        }
    }
    if let Some(system) = numbers.system {
        validate_system(system)?;
    }
    if let Some(disks) = numbers.disks {
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
