//! The complete, immutable machine result. Its reservation follows every shared retainer.
use std::sync::Arc;

use collector::machine::CPUTicks;
use persistence::Error;
use serde_json::Value;

use super::{MachineSecond, Reading};
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::cpu_history_sample::{CapturedInterval, ObservedCounters, ObservedInterval};
pub use crate::cpu_history_sample::{CpuInterval, CpuPrevious};
use crate::metrics::point::Point;

/// Every existing machine summary group and the compact counters CPU detail/history read.
/// Raw tables, a previous machine minute, and presentation JSON are never retained here.
#[derive(Debug)]
pub struct CompleteMachine {
    point: Point,
    cpus: Vec<CpuCounter>,
    interval: CpuIntervalState,
    observation: MachineIdentity,
    reservation: ByteReservation,
}

/// A failed non-CPU sensor cannot erase the independently valid native CPU counters.
#[derive(Debug)]
pub struct CpuReading {
    cpus: Vec<CpuCounter>,
    interval: CpuIntervalState,
    observation: MachineIdentity,
    reservation: ByteReservation,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum CompleteMachineState {
    #[default]
    Unread,
    Available(Arc<CompleteMachine>),
    Refused(&'static str),
}

#[derive(Debug)]
pub enum MachineCaptureError {
    Admission(MachineAdmission),
    Projection(Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineAdmission {
    RetainedBytes,
    ScratchBytes,
}

impl From<Error> for MachineCaptureError {
    fn from(error: Error) -> Self {
        Self::Projection(error)
    }
}

impl std::fmt::Display for MachineCaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission(reason) => write!(formatter, "machine admission: {reason:?}"),
            Self::Projection(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for MachineCaptureError {}

/// Component pairing, CPU counter conversion, and two 512-state core views share this bound.
/// This is the existing native machine reducer's scratch allowance, released after capture.
const SCRATCH_BYTES_MAX: usize = 8 * 1024 * 1024;
const ARC_BYTES: usize = 2 * size_of::<usize>();

#[derive(Debug, PartialEq)]
pub struct CpuCounter {
    pub cpu: u32,
    pub ticks: CPUTicks,
}

#[derive(Debug, PartialEq)]
struct MachineIdentity {
    publication: u64,
    run_tick: u64,
    wall: u64,
    continuous: Option<u64>,
}

#[derive(Debug, PartialEq)]
enum CpuIntervalState {
    Available(CapturedInterval),
    Refused(&'static str),
}

impl CpuIntervalState {
    fn borrowed(&self) -> persistence::Result<CpuInterval<'_>> {
        match self {
            Self::Available(interval) => Ok(interval.borrowed()),
            Self::Refused(reason) => Err(Error::Bound(reason)),
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Available(interval) => interval.bytes(),
            Self::Refused(_) => 0,
        }
    }
}

impl CompleteMachine {
    pub(super) fn capture(
        reading: &Reading<'_>,
        previous: Option<&Reading<'_>>,
        budget: &Arc<ByteBudget>,
    ) -> Result<Arc<Self>, MachineCaptureError> {
        let wall = reading
            .tick
            .wall_ns
            .ok_or(Error::Bound("machine wall clock"))?;
        if reading.cpus.len() > crate::cpu_history_sample::CPUS_MAX {
            return Err(Error::Bound("logical CPU count").into());
        }
        if reading.fans.len() > usize::from(collector::smc::FANS_MAX) {
            return Err(Error::Bound("fan channels").into());
        }
        let device_count = reading
            .tick
            .disks
            .as_ref()
            .filter(|disks| Some(disks.source_continuous_ns) == reading.tick.continuous_ns)
            .map_or(0, |disks| disks.count);
        if device_count > collector::disks::DISKS_MAX {
            return Err(Error::Bound("physical disk count").into());
        }
        let bytes = size_of::<Self>()
            + ARC_BYTES
            + computer_bytes(reading)
            + reading.cpus.len() * size_of::<CpuCounter>()
            + reading.cpus.len() * size_of::<Option<f64>>()
            + reading.fans.len() * size_of::<Option<f64>>()
            + crate::metrics::storage::StoragePoint::retained_bytes(device_count);
        let mut reservation = budget.reserve(bytes).ok_or(MachineCaptureError::Admission(
            MachineAdmission::RetainedBytes,
        ))?;
        let scratch = budget
            .reserve(SCRATCH_BYTES_MAX)
            .ok_or(MachineCaptureError::Admission(
                MachineAdmission::ScratchBytes,
            ))?;
        let mut interval = None;
        let point = crate::metrics::point::capture(reading, previous, &mut interval)?;
        let interval = match interval {
            Some(interval) => CpuIntervalState::Available(interval),
            None => capture_interval(reading, previous)?,
        };
        let cpus = capture_counters(reading.cpus);
        let actual = size_of::<Self>()
            + ARC_BYTES
            + point.bytes()
            + interval.bytes()
            + cpus.capacity() * size_of::<CpuCounter>();
        assert!(actual <= reservation.bytes());
        reservation.shrink_to(actual);
        let retained = Arc::new(Self {
            point,
            cpus,
            interval,
            reservation,
            observation: MachineIdentity {
                publication: reading.tick.publication,
                run_tick: reading.tick.run_tick,
                wall,
                continuous: reading.tick.continuous_ns,
            },
        });
        drop(scratch);
        Ok(retained)
    }

    /// Serializes held values only. The second supplies Store's current segment number.
    #[must_use]
    pub fn summary(&self, second: &MachineSecond) -> Value {
        assert!(self.matches(second));
        self.point.json(
            &crate::session::Observation {
                window: 0,
                publication: second.publication,
                run_tick: second.run_tick,
                wall: second.wall_ns,
                continuous: second.continuous_ns,
            },
            second.segment,
        )
    }

    /// Each compact counter keeps its original logical CPU identity.
    #[must_use]
    pub fn cpu_counters(&self) -> &[CpuCounter] {
        &self.cpus
    }

    pub(super) fn cpu_interval(&self) -> persistence::Result<CpuInterval<'_>> {
        self.interval.borrowed()
    }

    pub(super) fn matches(&self, second: &MachineSecond) -> bool {
        self.observation.publication == second.publication
            && self.observation.run_tick == second.run_tick
            && self.observation.wall == second.wall_ns
            && self.observation.continuous == second.continuous_ns
    }

    pub(super) fn point(&self) -> &Point {
        &self.point
    }

    /// The reservation charges one physical owner even when Views and sampled tails share it.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation.bytes()
    }
}

impl CpuReading {
    pub(super) fn capture(
        reading: &Reading<'_>,
        previous: Option<&Reading<'_>>,
        budget: &Arc<ByteBudget>,
    ) -> Result<Option<Arc<Self>>, MachineCaptureError> {
        if reading.cpus.is_empty() {
            return Ok(None);
        }
        if reading.cpus.len() > crate::cpu_history_sample::CPUS_MAX {
            return Ok(None);
        }
        let bytes = size_of::<Self>()
            + ARC_BYTES
            + reading.cpus.len() * (size_of::<CpuCounter>() + size_of::<Option<f64>>());
        let mut reservation = budget.reserve(bytes).ok_or(MachineCaptureError::Admission(
            MachineAdmission::RetainedBytes,
        ))?;
        let scratch_bytes = (reading.cpus.len()
            + previous.map_or(0, |old| {
                old.cpus.len().min(crate::cpu_history_sample::CPUS_MAX)
            }))
            * size_of::<[Option<u64>; 4]>();
        let scratch = budget
            .reserve(scratch_bytes)
            .ok_or(MachineCaptureError::Admission(
                MachineAdmission::ScratchBytes,
            ))?;
        let interval = capture_interval(reading, previous)?;
        let cpus = capture_counters(reading.cpus);
        let actual = size_of::<Self>()
            + ARC_BYTES
            + cpus.capacity() * size_of::<CpuCounter>()
            + interval.bytes();
        reservation.shrink_to(actual);
        drop(scratch);
        Ok(Some(Arc::new(Self {
            cpus,
            interval,
            reservation,
            observation: MachineIdentity {
                publication: reading.tick.publication,
                run_tick: reading.tick.run_tick,
                wall: reading
                    .tick
                    .wall_ns
                    .ok_or(Error::Bound("machine wall clock"))?,
                continuous: reading.tick.continuous_ns,
            },
        })))
    }

    pub(super) fn counters(&self, second: &MachineSecond) -> Option<&[CpuCounter]> {
        if self.observation.publication != second.publication {
            return None;
        }
        if self.observation.run_tick != second.run_tick {
            return None;
        }
        if self.observation.wall != second.wall_ns {
            return None;
        }
        if self.observation.continuous != second.continuous_ns {
            return None;
        }
        assert!(self.reservation.bytes() >= size_of::<Self>() + ARC_BYTES);
        Some(&self.cpus)
    }

    pub(super) fn cpu_interval(
        &self,
        second: &MachineSecond,
    ) -> persistence::Result<Option<CpuInterval<'_>>> {
        if self.counters(second).is_none() {
            return Ok(None);
        }
        self.interval.borrowed().map(Some)
    }
}

impl PartialEq for CpuReading {
    fn eq(&self, other: &Self) -> bool {
        self.observation == other.observation
            && self.cpus == other.cpus
            && self.interval == other.interval
    }
}

impl PartialEq for CompleteMachine {
    fn eq(&self, other: &Self) -> bool {
        self.observation == other.observation
            && self.point == other.point
            && self.cpus == other.cpus
            && self.interval == other.interval
    }
}

fn capture_interval(
    reading: &Reading<'_>,
    previous: Option<&Reading<'_>>,
) -> persistence::Result<CpuIntervalState> {
    match capture_interval_rows(reading, previous) {
        Ok(interval) => Ok(CpuIntervalState::Available(interval)),
        Err(Error::Bound(reason)) => Ok(CpuIntervalState::Refused(reason)),
        Err(error) => Err(error),
    }
}

fn capture_counters(rows: &[crate::recording::CpuRow]) -> Vec<CpuCounter> {
    let counters = rows
        .iter()
        .map(|row| CpuCounter {
            cpu: row.cpu,
            ticks: row.ticks,
        })
        .collect::<Vec<_>>();
    assert_eq!(counters.capacity(), rows.len());
    counters
}

fn capture_interval_rows(
    reading: &Reading<'_>,
    previous: Option<&Reading<'_>>,
) -> persistence::Result<CapturedInterval> {
    let current = crate::metrics::point::observation(reading);
    let mut counters = Vec::new();
    crate::cpu_history_sample::counters_rows(reading.cpus, &mut counters)?;
    let previous = previous.filter(|old| old.tick.wall_ns.is_some());
    let before = previous.map(crate::metrics::point::observation);
    let mut old_counters = Vec::new();
    if let Some(previous) = previous {
        crate::cpu_history_sample::counters_rows(previous.cpus, &mut old_counters)?;
    }
    CapturedInterval::capture(&ObservedInterval {
        current: ObservedCounters {
            observation: &current,
            counters: &counters,
        },
        previous: before.as_ref().map(|observation| ObservedCounters {
            observation,
            counters: &old_counters,
        }),
    })
}

fn computer_bytes(reading: &Reading<'_>) -> usize {
    reading
        .tick
        .computer_text()
        .iter()
        .flatten()
        .map(|text| text.len())
        .sum::<usize>()
        + reading.tick.system.as_ref().map_or(0, |system| {
            system.machine.levels.as_ref().map_or(0, |levels| {
                levels
                    .iter()
                    .map(|level| level.name.as_bytes().len())
                    .sum::<usize>()
            })
        })
}
