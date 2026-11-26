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

    /// The reading's `rate` over the interval before it, `None` where the readings give none.
    #[must_use]
    pub fn memory_rate(&self, rate: super::MemoryRate) -> Option<f64> {
        self.point.rates[rate.point_index()]
    }

    /// The reading's `rate` over the interval before it, `None` where the readings give none.
    #[must_use]
    pub fn network_rate(&self, rate: super::NetworkRate) -> Option<f64> {
        self.point.rates[rate.point_index()]
    }

    /// The reading's `rate` over the interval before it, `None` where the readings give none,
    /// as where no disk scan completed.
    #[must_use]
    pub fn storage_rate(&self, rate: super::StorageRate) -> Option<f64> {
        self.point.storage.as_ref()?.rate(rate.point_index())
    }

    /// The GPU's power over the interval before the reading, in watts, `None` where the readings
    /// give none, as where either reading lacks `IOReport`.
    #[must_use]
    pub fn gpu_watts(&self) -> Option<f64> {
        self.point.gpu_watts
    }

    /// Each fan's speed, one for each channel the reading counts, `None` where the reading holds
    /// no fans. A channel the SMC read no speed of holds `None`.
    #[must_use]
    pub fn fan_rpm(&self) -> Option<&[Option<f64>]> {
        self.point.fan_rpm.as_deref()
    }

    /// The unified memory the GPU driver reported in use, `None` where the reading holds no
    /// system group.
    #[must_use]
    pub fn gpu_memory_in_use_bytes(&self) -> Option<u64> {
        self.point.system.gpu_memory_in_use_bytes
    }

    /// The unified memory the GPU driver reported allocated, `None` where the reading holds no
    /// system group.
    #[must_use]
    pub fn gpu_memory_allocated_bytes(&self) -> Option<u64> {
        self.point.system.gpu_memory_allocated_bytes
    }

    /// The mean clock of each performance level while its cores were active, in gigahertz, level
    /// 0, the fastest, first, `None` where the readings give no clock of any level.
    #[must_use]
    pub fn cpu_level_ghz(&self) -> Option<[Option<f64>; 2]> {
        self.point.cpu_level_ghz
    }

    /// The load averages over 1, 5 and 15 minutes, `None` where the reading holds no system
    /// group.
    #[must_use]
    pub fn load_average(&self) -> Option<[f64; 3]> {
        self.point.system.load_average
    }

    /// The file entries open across the system, `None` where the reading holds no system group.
    #[must_use]
    pub fn open_files(&self) -> Option<u64> {
        self.point.system.open_files
    }

    /// The share of the kernel's limit the open files take, `None` where the reading holds no
    /// system group or macOS reported no limit.
    #[must_use]
    pub fn open_files_percent(&self) -> Option<f64> {
        self.point.system.open_files_percent
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

    /// The per-CPU readings of `second` that the store keeps where it refused the second's
    /// complete reading, a counter of each of as many logical CPUs as its performance levels
    /// name, whose interval is refused, charged to `budget`, so a test outside the viewer can
    /// give a refused second the logical CPUs it holds.
    #[must_use]
    pub fn of_levels(second: &MachineSecond, budget: &Arc<ByteBudget>) -> Arc<Self> {
        let Some(reservation) = budget.reserve(size_of::<Self>() + ARC_BYTES) else {
            panic!("the budget has no room for the per-CPU readings");
        };

        Arc::new(Self {
            cpus: counters_of_levels(second),
            interval: CpuIntervalState::Refused("fixture"),
            observation: MachineIdentity {
                publication: second.publication,
                run_tick: second.run_tick,
                wall: second.wall_ns,
                continuous: second.continuous_ns,
            },
            reservation,
        })
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

#[cfg(any(test, feature = "fixtures"))]
impl CompleteMachine {
    /// A complete reading of `second` whose one reading is a CPU share of `cpu_percent`, charged
    /// to `budget`, so a test can give a second a complete reading of the content it chooses.
    pub(crate) fn of_cpu_percent(
        second: &MachineSecond,
        cpu_percent: f64,
        budget: &Arc<ByteBudget>,
    ) -> Arc<Self> {
        let point = Point {
            cpu_percent: Some(cpu_percent),
            ..Point::default()
        };

        Self::of_point(second, point, budget)
    }

    /// A complete reading of `second` whose readings are the memory `second` holds, with no
    /// purgeable memory, and memory's rates `rates` in the order of `MemoryRate::ALL`, charged to
    /// `budget`, so a test outside the viewer can give a second the complete reading the Memory
    /// page draws.
    #[must_use]
    pub fn of_memory(
        second: &MachineSecond,
        rates: [Option<f64>; super::MemoryRate::ALL.len()],
        budget: &Arc<ByteBudget>,
    ) -> Arc<Self> {
        let mut point = Point::default();
        if let Some(memory) = second.memory.as_ref() {
            let held = [
                ("memory_total_bytes", Some(memory.total_bytes)),
                ("memory_wired_bytes", Some(memory.wired_bytes)),
                ("memory_compressed_bytes", Some(memory.compressed_bytes)),
                ("memory_anonymous_bytes", memory.app_bytes),
                ("memory_purgeable_bytes", Some(0)),
                ("swap_total_bytes", Some(memory.swap_total_bytes)),
                ("swap_used_bytes", Some(memory.swap_used_bytes)),
                (
                    "pressure",
                    Some(u64::from(crate::store::pressure_level(memory.pressure))),
                ),
            ];
            for (name, amount) in held {
                let Some(index) = crate::metrics::point::MEMORY_FIELDS
                    .iter()
                    .position(|(field, _column)| *field == name)
                else {
                    unreachable!("a trend point holds memory's {name}");
                };
                point.memory[index] = amount;
            }
        }
        for (rate, value) in super::MemoryRate::ALL.into_iter().zip(rates) {
            point.rates[rate.point_index()] = value;
        }

        Self::of_point(second, point, budget)
    }

    /// A complete reading of `second` whose readings are the traffic `second` holds and the
    /// network's packet and error rates `rates` in the order of `NetworkRate::ALL`, charged to
    /// `budget`, so a test outside the viewer can give a second the complete reading the Network
    /// page draws.
    #[must_use]
    pub fn of_network(
        second: &MachineSecond,
        rates: [Option<f64>; super::NetworkRate::ALL.len()],
        budget: &Arc<ByteBudget>,
    ) -> Arc<Self> {
        let mut point = Point::default();
        let received_index = crate::metrics::MEMORY_RATES.len();
        point.rates[received_index] = second.network.map(|traffic| traffic.received_bps);
        point.rates[received_index + 1] = second.network.map(|traffic| traffic.sent_bps);
        for (rate, value) in super::NetworkRate::ALL.into_iter().zip(rates) {
            point.rates[rate.point_index()] = value;
        }

        Self::of_point(second, point, budget)
    }

    /// The complete reading of `second` whose storage reads the bytes the second's record holds
    /// and `rates`, in the order of `StorageRate::ALL`, charged to `budget`, so a test outside
    /// the viewer can give a second the complete reading the Storage page draws.
    #[must_use]
    pub fn of_storage(
        second: &MachineSecond,
        rates: [Option<f64>; super::StorageRate::ALL.len()],
        budget: &Arc<ByteBudget>,
    ) -> Arc<Self> {
        let mut storage = [None; crate::metrics::storage::RATE_FIELDS.len()];
        storage[0] = second.disk_read_bps;
        storage[1] = second.disk_written_bps;
        for (rate, value) in super::StorageRate::ALL.into_iter().zip(rates) {
            storage[rate.point_index()] = value;
        }
        let point = Point {
            storage: Some(crate::metrics::storage::StoragePoint::of_rates(storage)),
            ..Point::default()
        };

        Self::of_point(second, point, budget)
    }

    /// The complete reading of `second` whose GPU reads the activity and sensor the second's
    /// record holds, the power `watts`, and the memory `memory_bytes`, in use and then
    /// allocated, charged to `budget`, so a test outside the viewer can give a second the
    /// complete reading the GPU page draws.
    #[must_use]
    pub fn of_gpu(
        second: &MachineSecond,
        watts: Option<f64>,
        memory_bytes: [Option<u64>; 2],
        budget: &Arc<ByteBudget>,
    ) -> Arc<Self> {
        let [in_use_bytes, allocated_bytes] = memory_bytes;
        let mut point = Point {
            gpu_active_percent: second.gpu_active_percent,
            gpu_watts: watts,
            gpu_celsius: second.gpu_celsius.map(f64::from),
            ..Point::default()
        };
        point.system.gpu_memory_in_use_bytes = in_use_bytes;
        point.system.gpu_memory_allocated_bytes = allocated_bytes;

        Self::of_point(second, point, budget)
    }

    /// The complete reading of `second` whose power, `soc_celsius` and thermal state read what the
    /// second's record holds, whose GPU power is `gpu_watts` and whose fans read `fan_rpm`, one
    /// speed for each channel, charged to `budget`, so a test outside the viewer can give a second
    /// the complete reading the Energy page draws.
    #[must_use]
    pub fn of_energy(
        second: &MachineSecond,
        gpu_watts: Option<f64>,
        fan_rpm: Vec<Option<f64>>,
        budget: &Arc<ByteBudget>,
    ) -> Arc<Self> {
        assert!(fan_rpm.len() <= usize::from(collector::smc::FANS_MAX));
        let mut point = Point {
            power_watts: second.power_watts.map(f64::from),
            soc_celsius: second.soc_celsius.map(f64::from),
            gpu_watts,
            fan_rpm: Some(fan_rpm),
            ..Point::default()
        };
        point.system.thermal_pressure = second.thermal_pressure.map(u64::from);

        Self::of_point(second, point, budget)
    }

    /// The complete reading of `second` whose CPU reads the activity and sensor the second's
    /// record holds, a counter of each of as many logical CPUs as its performance levels name,
    /// which the CPU counts, each level's clock
    /// `level_ghz`, the load averages `load_average`, and the open files `open_files` with their
    /// share of the limit `open_files_percent`, charged to `budget`, so a test outside the viewer
    /// can give a second the complete reading the CPU page's machine charts draw.
    #[must_use]
    pub fn of_cpu(
        second: &MachineSecond,
        level_ghz: [Option<f64>; 2],
        load_average: Option<[f64; 3]>,
        open_files: Option<u64>,
        open_files_percent: Option<f64>,
        budget: &Arc<ByteBudget>,
    ) -> Arc<Self> {
        let logical_cpu_count = second
            .performance_levels
            .map(|[fastest, slower]| fastest.logical_cpus + slower.logical_cpus);
        let mut point = Point {
            cpu_percent: second.cpu_percent,
            cpu_level_percent: Some(second.cpu_level_percent),
            cpu_level_ghz: Some(level_ghz),
            logical_cpu_count,
            cpu_celsius: second.cpu_celsius.map(f64::from),
            ..Point::default()
        };
        point.system.load_average = load_average;
        point.system.open_files = open_files;
        point.system.open_files_percent = open_files_percent;
        if let Some(levels) = &second.performance_levels {
            point.system.machine = point.system.machine.with_levels(levels);
        }
        let mut reading = Self::of_point(second, point, budget);

        let Some(complete) = Arc::get_mut(&mut reading) else {
            unreachable!("a complete reading just built has no other owner");
        };
        complete.cpus = counters_of_levels(second);
        reading
    }

    /// `reading`, a complete reading no other owner holds yet, with each logical CPU busy for the
    /// share `busy_percent` gives it over the interval since `previous`, so a test outside the
    /// viewer can give a second the logical CPUs the CPU page's bars and heatmap draw.
    #[must_use]
    pub fn with_cpu_interval(
        mut reading: Arc<Self>,
        busy_percent: Vec<Option<f64>>,
        previous: &MachineSecond,
    ) -> Arc<Self> {
        let Some(complete) = Arc::get_mut(&mut reading) else {
            panic!("another owner holds the complete reading");
        };
        complete.interval = CpuIntervalState::Available(CapturedInterval {
            values: busy_percent,
            unavailable_reason: None,
            previous: Some(CpuPrevious {
                publication: previous.publication,
                run_tick: previous.run_tick,
                wall_ns: previous.wall_ns,
                continuous_ns: previous.continuous_ns,
            }),
        });

        reading
    }

    /// The complete reading of `second` whose readings are `point`, charged to `budget`.
    fn of_point(second: &MachineSecond, point: Point, budget: &Arc<ByteBudget>) -> Arc<Self> {
        let Some(reservation) = budget.reserve(size_of::<Self>() + ARC_BYTES) else {
            panic!("the budget has no room for a complete reading");
        };

        Arc::new(Self {
            point,
            cpus: Vec::new(),
            interval: CpuIntervalState::Refused("fixture"),
            observation: MachineIdentity {
                publication: second.publication,
                run_tick: second.run_tick,
                wall: second.wall_ns,
                continuous: second.continuous_ns,
            },
            reservation,
        })
    }
}

/// A counter of each logical CPU the performance levels of `second` name, for a fixture.
fn counters_of_levels(second: &MachineSecond) -> Vec<CpuCounter> {
    let count = second.performance_levels.map_or(0, |[fastest, slower]| {
        fastest.logical_cpus + slower.logical_cpus
    });

    (0..count)
        .map(|cpu| CpuCounter {
            cpu,
            ticks: CPUTicks::default(),
        })
        .collect()
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
