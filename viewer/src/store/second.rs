//! A second of the machine's readings as the pages draw it. The store derives it once, when it
//! takes the second in from the ring or from a sealed minute, so the main thread reads values
//! and computes no rate.

use collector::disks::{DiskReading, DiskSnapshot, ScanState};
use collector::machine::Pressure;
use collector::smc::FANS_MAX;
use collector::system::PerformanceLevel;

use crate::cpu_history_sample;
use crate::metrics::{battery, reduction};
use crate::recording::{
    CpuRow, FanRow, GpuStateRow, InterfaceRow, MachineMinute, MemoryRow, TickRow,
};

#[cfg(test)]
#[path = "second_tests.rs"]
mod tests;

/// One second of the machine's readings. A value the collector did not read, or a rate whose
/// interval the readings do not show, is `None` and never zero.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MachineSecond {
    pub publication: u64,
    pub run_tick: u64,
    pub wall_ns: u64,
    pub continuous_ns: Option<u64>,
    pub awake_ns: Option<u64>,
    /// Whether the rates are over the interval since the run's previous reading, which holds
    /// only where the two readings are consecutive (doc-30).
    pub follows: bool,
    /// The breaks in the store's readings before this second, so two seconds share it where
    /// every reading between them follows the one before it in one run. The store sets it as
    /// it takes the second in.
    pub segment: u64,
    pub cpu_percent: Option<f64>,
    /// Each CPU's busy share added up, so one busy CPU is 100%, as All CPU draws it beside the
    /// CPU each App used.
    pub cpu_percent_one_core: Option<f64>,
    /// Each performance level's busy share, level 0, the fastest, first.
    pub cpu_level_percent: [Option<f64>; 2],
    pub gpu_active_percent: Option<f64>,
    pub memory: Option<MemorySecond>,
    pub network: Option<Traffic>,
    pub disk_read_bps: Option<f64>,
    pub disk_written_bps: Option<f64>,
    pub power_watts: Option<f32>,
    pub soc_celsius: Option<f32>,
    pub cpu_celsius: Option<f32>,
    pub gpu_celsius: Option<f32>,
    pub fan_mean_rpm: Option<f32>,
    /// The speeds of fans 0 and 1, the two the hardware figure draws.
    pub fan_rpm: [Option<f32>; 2],
    /// `None` where the tick holds no battery reading, or one older than the interval the
    /// collector reads the battery at.
    pub battery: Option<BatterySecond>,
    pub thermal_pressure: Option<u8>,
    /// The chip's two performance levels, level 0 the fastest, as the tick named them.
    pub performance_levels: Option<[PerformanceLevel; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySecond {
    pub total_bytes: u64,
    /// The memory apps asked for themselves, `None` where macOS reports more purgeable memory
    /// than anonymous memory.
    pub app_bytes: Option<u64>,
    pub wired_bytes: u64,
    pub compressed_bytes: u64,
    pub pressure: Pressure,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
}

/// External traffic over a second, in bytes per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Traffic {
    pub received_bps: f64,
    pub sent_bps: f64,
}

/// The battery at a second, as the collector last read it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatterySecond {
    /// The charge the battery holds, as a share of what it holds when full.
    pub charge_percent: Option<f64>,
    /// The battery's power in watts, negative while it discharges.
    pub power_watts: Option<f64>,
}

/// The rows of one publication of a `MachineMinute`.
#[derive(Clone, Copy, Debug)]
pub struct Reading<'a> {
    pub tick: &'a TickRow,
    pub cpus: &'a [CpuRow],
    /// `None` where the minute holds no memory row for the publication.
    pub memory: Option<&'a MemoryRow>,
    pub interfaces: &'a [InterfaceRow],
    pub gpu_states: &'a [GpuStateRow],
    pub fans: &'a [FanRow],
}

impl MachineSecond {
    /// The second `reading` shows, its rates over the interval since `previous`, the run's
    /// reading before it, where the two are consecutive. `None` when the tick has no wall
    /// clock, since a second without one has no place on a chart.
    pub fn of(reading: &Reading<'_>, previous: Option<&Reading<'_>>) -> Option<Self> {
        let wall_ns = reading.tick.wall_ns?;

        let mut second = Self {
            publication: reading.tick.publication,
            run_tick: reading.tick.run_tick,
            wall_ns,
            continuous_ns: reading.tick.continuous_ns,
            awake_ns: reading.tick.awake_ns,
            follows: false,
            segment: 0,
            cpu_percent: None,
            cpu_percent_one_core: None,
            cpu_level_percent: [None; 2],
            gpu_active_percent: None,
            memory: reading.memory.map(MemorySecond::of),
            network: None,
            disk_read_bps: None,
            disk_written_bps: None,
            power_watts: measured(reading.tick.power_watts),
            soc_celsius: measured(reading.tick.soc_celsius),
            cpu_celsius: measured(reading.tick.cpu_celsius),
            gpu_celsius: measured(reading.tick.gpu_celsius),
            fan_mean_rpm: fan_mean_rpm(reading.fans),
            fan_rpm: fan_rpm(reading.fans),
            battery: BatterySecond::of(reading.tick),
            thermal_pressure: reading.tick.thermal_pressure,
            performance_levels: reading.tick.performance_levels,
        };
        if let Some(previous) = previous {
            if let Some(elapsed_ns) = interval_ns(previous.tick, reading.tick) {
                second.follows = true;
                second.cpu_percent = cpu_percent(reading.cpus, previous.cpus);
                second.cpu_percent_one_core = cpu_percent_one_core(reading.cpus, previous.cpus);
                second.cpu_level_percent = cpu_level_percent(reading, previous);
                second.gpu_active_percent =
                    gpu_active_percent(reading.gpu_states, previous.gpu_states, elapsed_ns);
                second.network = traffic(reading.interfaces, previous.interfaces, elapsed_ns);
                let [read, written] = disk_bytes(reading.tick, previous.tick);
                second.disk_read_bps = read.map(|bytes| per_second(bytes, elapsed_ns));
                second.disk_written_bps = written.map(|bytes| per_second(bytes, elapsed_ns));
            }
        }

        Some(second)
    }
}

impl MemorySecond {
    fn of(row: &MemoryRow) -> Self {
        let memory = &row.memory;
        Self {
            total_bytes: memory.total_bytes,
            app_bytes: memory.anonymous_bytes.checked_sub(memory.purgeable_bytes),
            wired_bytes: memory.wired_bytes,
            compressed_bytes: memory.compressed_bytes,
            pressure: memory.pressure,
            swap_used_bytes: memory.swap_used_bytes,
            swap_total_bytes: memory.swap_total_bytes,
        }
    }
}

impl BatterySecond {
    /// The battery as `tick` shows it, `None` where the tick holds no reading or one older than
    /// the interval the collector reads the battery at, as the worker's summary leaves it out.
    fn of(tick: &TickRow) -> Option<Self> {
        let reading = tick.battery.as_ref()?;
        let now = tick.continuous_ns?;
        if !reading.is_fresh_at(now) {
            return None;
        }

        Some(Self {
            charge_percent: battery::charge_percent(reading),
            power_watts: battery::power_watts(reading),
        })
    }
}

/// The nanoseconds of the continuous clock between `previous` and `current`, where the two are
/// consecutive readings of one run: consecutive publications and run ticks, wall clocks rising
/// by at most one second, and continuous clocks rising (doc-30, `metrics::compatible`).
fn interval_ns(previous: &TickRow, current: &TickRow) -> Option<u64> {
    let observation = |tick: &TickRow| {
        Some(crate::session::Observation {
            window: 0,
            publication: tick.publication,
            run_tick: tick.run_tick,
            wall: tick.wall_ns?,
            continuous: tick.continuous_ns,
        })
    };
    if !crate::metrics::compatible(&observation(previous)?, &observation(current)?) {
        return None;
    }
    current.continuous_ns?.checked_sub(previous.continuous_ns?)
}

/// The share of every CPU's ticks since `previous` that were busy, in percent. `None` where
/// the CPUs differ or a counter went back.
fn cpu_percent(current: &[CpuRow], previous: &[CpuRow]) -> Option<f64> {
    if current.len() > cpu_history_sample::CPUS_MAX {
        return None;
    }
    if current.len() != previous.len() {
        return None;
    }
    let mut totals = reduction::CpuTotals::default();
    for (index, now) in current.iter().enumerate() {
        let old = paired(previous, index, |old| old.cpu == now.cpu)?;
        let Ok(Some(delta)) = cpu_delta(now, old) else {
            return None;
        };
        if totals.add(&delta).is_err() {
            return None;
        }
    }
    totals.machine_percent()
}

fn cpu_delta(
    current: &CpuRow,
    previous: &CpuRow,
) -> persistence::Result<Option<reduction::CpuTotals>> {
    reduction::CpuTotals::between(reduction::CounterPair {
        current: &cpu_history_sample::counters_values(&current.ticks),
        previous: &cpu_history_sample::counters_values(&previous.ticks),
    })
}

fn cpu_percent_one_core(current: &[CpuRow], previous: &[CpuRow]) -> Option<f64> {
    let Ok(percent) = cpu_busy_sum(current, previous) else {
        return None;
    };
    percent
}

fn cpu_busy_sum(current: &[CpuRow], previous: &[CpuRow]) -> persistence::Result<Option<f64>> {
    if current.len() > cpu_history_sample::CPUS_MAX {
        return Ok(None);
    }
    if current.len() != previous.len() {
        return Ok(None);
    }
    reduction::busy_sum(current.iter().zip(previous).map(|(now, old)| {
        if now.cpu != old.cpu {
            return Ok(None);
        }
        cpu_delta(now, old)
    }))
}

fn cpu_level_percent(current: &Reading<'_>, previous: &Reading<'_>) -> [Option<f64>; 2] {
    let Some(levels) = current.tick.performance_levels else {
        return [None; 2];
    };
    let [Ok(fast), Ok(slow)] = levels.map(|level| usize::try_from(level.logical_cpus)) else {
        return [None; 2];
    };
    if fast.checked_add(slow) != Some(current.cpus.len()) {
        return [None; 2];
    }
    if previous.cpus.len() != current.cpus.len() {
        return [None; 2];
    }
    [
        cpu_level_mean(&current.cpus[slow..], &previous.cpus[slow..]),
        cpu_level_mean(&current.cpus[..slow], &previous.cpus[..slow]),
    ]
}

fn cpu_level_mean(current: &[CpuRow], previous: &[CpuRow]) -> Option<f64> {
    let Ok(Some(sum)) = cpu_busy_sum(current, previous) else {
        return None;
    };
    let count = u32::try_from(current.len()).unwrap_or_else(|_| unreachable!("bounded CPU count"));
    assert!(count > 0);
    Some(sum / f64::from(count))
}

/// The share of `elapsed_ns` the GPU spent in its states since `previous`, in percent. `None`
/// where either tick read no states, the states differ or a counter went back.
fn gpu_active_percent(
    current: &[GpuStateRow],
    previous: &[GpuStateRow],
    elapsed_ns: u64,
) -> Option<f64> {
    assert!(elapsed_ns > 0);
    if current.is_empty() {
        return None;
    }
    if current.len() != previous.len() {
        return None;
    }
    let mut totals = reduction::CounterTotals::new();
    for (index, now) in current.iter().enumerate() {
        let old = paired(previous, index, |old| old.state == now.state)?;
        if totals
            .add(reduction::CounterPair {
                current: &[Some(now.active_ns)],
                previous: &[Some(old.active_ns)],
            })
            .is_err()
        {
            return None;
        }
    }
    reduction::machine_percent(&reduction::Fraction {
        numerator: totals.finish()[0]?,
        denominator: elapsed_ns,
    })
}

/// The bytes read from and written to the disks since `previous`, summed over the disks, where
/// both ticks scanned every disk at the tick and found the same ones. Each is `None` where a
/// disk failed or its counter is missing or went back.
fn disk_bytes(current: &TickRow, previous: &TickRow) -> [Option<u64>; 2] {
    let Some(now) = disk_scan(current) else {
        return [None; 2];
    };
    let Some(before) = disk_scan(previous) else {
        return [None; 2];
    };
    crate::metrics::storage::byte_deltas(&crate::metrics::storage::DiskInterval {
        current: now,
        previous: before,
    })
}

/// The disks of `tick`'s scan, where the scan completed at the tick.
fn disk_scan(tick: &TickRow) -> Option<&[DiskReading]> {
    let snapshot: &DiskSnapshot = tick.disks.as_ref()?;
    if Some(snapshot.source_continuous_ns) != tick.continuous_ns {
        return None;
    }
    if snapshot.state != ScanState::Complete {
        return None;
    }

    snapshot.records.get(..snapshot.count)
}

fn per_second(count: u64, elapsed_ns: u64) -> f64 {
    assert!(elapsed_ns > 0);

    reduction::per_second(&reduction::Rate {
        delta: count,
        elapsed_ns,
    })
}

/// The mean speed of the fans `fans` measured, or `None` where it measured none. A reading that
/// is no speed is left out, and so are fans past the most the SMC lists.
fn fan_mean_rpm(fans: &[FanRow]) -> Option<f32> {
    let mut sum = 0.0_f32;
    let mut count = 0_u8;
    for fan in fans.iter().take(usize::from(FANS_MAX)) {
        let Some(rpm) = fan_speed(fan) else {
            continue;
        };

        sum += rpm;
        count += 1;
    }
    if count == 0 {
        return None;
    }

    Some(sum / f32::from(count))
}

/// The speeds of fans 0 and 1 as `fans` read them, each `None` where the tick read no speed of
/// that fan.
fn fan_rpm(fans: &[FanRow]) -> [Option<f32>; 2] {
    let mut speeds = [None; 2];
    for (fan_number, speed) in (0_u32..).zip(&mut speeds) {
        let mut measured = fans.iter().take(usize::from(FANS_MAX));
        *speed = measured
            .find(|fan| fan.fan == fan_number)
            .and_then(fan_speed);
    }

    speeds
}

/// The sensor's `reading`, `None` where it read no number, as a chart draws a gap there.
fn measured(reading: Option<f32>) -> Option<f32> {
    reading.filter(|reading| reading.is_finite())
}

/// The speed `fan` read, `None` where its reading is no speed.
fn fan_speed(fan: &FanRow) -> Option<f32> {
    if !fan.rpm.is_finite() {
        return None;
    }
    if fan.rpm < 0.0 {
        return None;
    }

    Some(fan.rpm)
}

/// The external traffic since `previous`, over `elapsed_ns` of the continuous clock. A Mac with
/// no interface on either side moves no traffic. `None` where the interfaces differ or a
/// counter went back.
fn traffic(
    current: &[InterfaceRow],
    previous: &[InterfaceRow],
    elapsed_ns: u64,
) -> Option<Traffic> {
    assert!(elapsed_ns > 0);
    if current.len() != previous.len() {
        return None;
    }
    let mut totals = reduction::CounterTotals::new();
    for (index, now) in current.iter().enumerate() {
        let old = paired(previous, index, |old| {
            if old.interface.index != now.interface.index {
                return false;
            }
            old.interface.name == now.interface.name
        })?;
        if totals
            .add(reduction::CounterPair {
                current: &[
                    Some(now.interface.received_bytes),
                    Some(now.interface.sent_bytes),
                ],
                previous: &[
                    Some(old.interface.received_bytes),
                    Some(old.interface.sent_bytes),
                ],
            })
            .is_err()
        {
            return None;
        }
    }
    let [received, sent] = totals.finish();
    Some(Traffic {
        received_bps: reduction::per_second(&reduction::Rate {
            delta: received?,
            elapsed_ns,
        }),
        sent_bps: reduction::per_second(&reduction::Rate {
            delta: sent?,
            elapsed_ns,
        }),
    })
}

/// The adapter retains each domain's original pairing policy; the reducer owns arithmetic.
fn paired<T>(previous: &[T], index: usize, matches: impl Fn(&T) -> bool) -> Option<&T> {
    if let Some(row) = previous.get(index) {
        if matches(row) {
            return Some(row);
        }
    }
    previous.iter().find(|row| matches(row))
}

/// The readings of `minute`, one for each tick, in publication order.
pub fn readings(minute: &MachineMinute) -> impl Iterator<Item = Reading<'_>> {
    let mut cpus = &minute.cpus[..];
    let mut memory = &minute.memory[..];
    let mut interfaces = &minute.interfaces[..];
    let mut gpu_states = &minute.gpu_states[..];
    let mut fans = &minute.fans[..];
    minute.ticks.iter().map(move |tick| {
        let publication = tick.publication;
        let (own_cpus, rest) = split_publication(cpus, publication, |row| row.publication);
        cpus = rest;
        let (own_memory, rest) = split_publication(memory, publication, |row| row.publication);
        memory = rest;
        let (own_interfaces, rest) =
            split_publication(interfaces, publication, |row| row.publication);
        interfaces = rest;
        let (own_gpu_states, rest) =
            split_publication(gpu_states, publication, |row| row.publication);
        gpu_states = rest;
        let (own_fans, rest) = split_publication(fans, publication, |row| row.publication);
        fans = rest;

        Reading {
            tick,
            cpus: own_cpus,
            memory: own_memory.first(),
            interfaces: own_interfaces,
            gpu_states: own_gpu_states,
            fans: own_fans,
        }
    })
}

/// The leading rows of `rows` that belong to `publication`, and the rest. Rows of an earlier
/// publication, whose tick the minute does not hold, are passed over.
fn split_publication<T>(
    rows: &[T],
    publication: u64,
    publication_of: impl Fn(&T) -> u64,
) -> (&[T], &[T]) {
    let first = rows
        .iter()
        .position(|row| publication_of(row) >= publication)
        .unwrap_or(rows.len());
    let rows = &rows[first..];
    let count = rows
        .iter()
        .position(|row| publication_of(row) != publication)
        .unwrap_or(rows.len());

    rows.split_at(count)
}
