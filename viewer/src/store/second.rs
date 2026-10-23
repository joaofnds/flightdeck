//! A second of the machine's readings as the pages draw it. The store derives it once, when it
//! takes the second in from the ring or from a sealed minute, so the main thread reads values
//! and computes no rate.

use collector::disks::{DiskReading, DiskSnapshot, ScanState};
use collector::interfaces::Interface;
use collector::machine::{CPUTicks, Pressure};
use collector::smc::FANS_MAX;
use collector::system::PerformanceLevel;

use crate::metrics::{battery, number};
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
    if previous.publication.checked_add(1) != Some(current.publication) {
        return None;
    }

    if previous.run_tick.checked_add(1) != Some(current.run_tick) {
        return None;
    }

    let (Some(wall_before), Some(wall_now)) = (previous.wall_ns, current.wall_ns) else {
        return None;
    };
    if wall_before >= wall_now {
        return None;
    }

    // Both divisions floor, so the wall clocks fall in the same or the next whole second.
    if wall_now / 1_000_000_000 > wall_before / 1_000_000_000 + 1 {
        return None;
    }

    let (Some(before), Some(now)) = (previous.continuous_ns, current.continuous_ns) else {
        return None;
    };
    now.checked_sub(before).filter(|elapsed| *elapsed > 0)
}

/// The share of every CPU's ticks since `previous` that were busy, in percent. `None` where
/// the CPUs differ or a counter went back.
fn cpu_percent(current: &[CpuRow], previous: &[CpuRow]) -> Option<f64> {
    if current.len() != previous.len() {
        return None;
    }

    let mut busy = 0_u64;
    let mut idle = 0_u64;
    for (index, now) in current.iter().enumerate() {
        let before = paired(previous, index, |row| row.cpu == now.cpu)?;
        let delta = cpu_delta(&before.ticks, &now.ticks)?;
        busy = busy.checked_add(delta.busy)?;
        idle = idle.checked_add(delta.idle)?;
    }

    let total = busy.checked_add(idle)?;
    if total == 0 {
        return None;
    }

    Some(number(busy) * 100.0 / number(total))
}

struct CpuDelta {
    busy: u64,
    idle: u64,
}

fn cpu_delta(before: &CPUTicks, now: &CPUTicks) -> Option<CpuDelta> {
    let user = now.user.checked_sub(before.user)?;
    let system = now.system.checked_sub(before.system)?;
    let nice = now.nice.checked_sub(before.nice)?;
    let idle = now.idle.checked_sub(before.idle)?;

    Some(CpuDelta {
        busy: u64::from(user) + u64::from(system) + u64::from(nice),
        idle: u64::from(idle),
    })
}

/// The mean busy share of each performance level's CPUs, level 0 first, where the levels of
/// `current` cover its CPUs. The slower level's CPUs take the lowest indices, as macOS numbers
/// them.
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

    let (slow_now, fast_now) = current.cpus.split_at(slow);
    let (slow_before, fast_before) = previous.cpus.split_at(slow);
    [
        mean_busy_percent(fast_now, fast_before),
        mean_busy_percent(slow_now, slow_before),
    ]
}

/// Each CPU's busy share since `previous` added up, in percent, so one busy CPU is 100%, as the
/// worker's All CPU adds it up. `None` where the CPUs differ, a counter went back or a CPU
/// counted no ticks.
fn cpu_percent_one_core(current: &[CpuRow], previous: &[CpuRow]) -> Option<f64> {
    if current.len() != previous.len() {
        return None;
    }
    if current.is_empty() {
        return None;
    }

    Some(busy_shares(current, previous)?.percent_sum)
}

/// The mean of each CPU's busy share since `previous`, in percent. `None` where a CPU differs,
/// a counter went back or a CPU counted no ticks.
fn mean_busy_percent(current: &[CpuRow], previous: &[CpuRow]) -> Option<f64> {
    assert_eq!(current.len(), previous.len());
    if current.is_empty() {
        return None;
    }

    let shares = busy_shares(current, previous)?;
    Some(shares.percent_sum / f64::from(shares.count))
}

/// Each CPU's busy share since a previous reading, added up in percent, and the CPUs added.
struct BusyShares {
    percent_sum: f64,
    count: u32,
}

/// The busy shares of `current`'s CPUs since `previous`, CPU by CPU in order. `None` where a CPU
/// differs, a counter went back or a CPU counted no ticks.
fn busy_shares(current: &[CpuRow], previous: &[CpuRow]) -> Option<BusyShares> {
    assert_eq!(current.len(), previous.len());

    let mut shares = BusyShares {
        percent_sum: 0.0,
        count: 0,
    };
    for (now, before) in current.iter().zip(previous) {
        if now.cpu != before.cpu {
            return None;
        }
        let delta = cpu_delta(&before.ticks, &now.ticks)?;
        let total = delta.busy.checked_add(delta.idle)?;
        if total == 0 {
            return None;
        }

        shares.percent_sum += number(delta.busy) / number(total) * 100.0;
        shares.count += 1;
    }

    Some(shares)
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

    let mut active_ns = 0_u64;
    for (index, now) in current.iter().enumerate() {
        let before = paired(previous, index, |row| row.state == now.state)?;
        active_ns = active_ns.checked_add(now.active_ns.checked_sub(before.active_ns)?)?;
    }

    Some(number(active_ns) * 100.0 / number(elapsed_ns))
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
    if now.len() != before.len() {
        return [None; 2];
    }

    let add = |total: Option<u64>, delta: Option<u64>| {
        total
            .zip(delta)
            .and_then(|(total, delta)| total.checked_add(delta))
    };
    let mut read = Some(0_u64);
    let mut written = Some(0_u64);
    for disk in now {
        let Some(old) = before.iter().find(|old| same_disk(old, disk)) else {
            return [None; 2];
        };
        let [read_delta, written_delta] = disk_delta(disk, old);
        read = add(read, read_delta);
        written = add(written, written_delta);
    }

    [read, written]
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

fn same_disk(one: &DiskReading, other: &DiskReading) -> bool {
    if one.driver_id != other.driver_id {
        return false;
    }

    one.media_id == other.media_id
}

/// The bytes `current` read and wrote since `previous`.
fn disk_delta(current: &DiskReading, previous: &DiskReading) -> [Option<u64>; 2] {
    if current.failed {
        return [None; 2];
    }
    if previous.failed {
        return [None; 2];
    }

    let delta = |now: Option<u64>, before: Option<u64>| {
        now.zip(before)
            .and_then(|(now, before)| now.checked_sub(before))
    };
    [
        delta(current.read_bytes, previous.read_bytes),
        delta(current.write_bytes, previous.write_bytes),
    ]
}

fn per_second(count: u64, elapsed_ns: u64) -> f64 {
    assert!(elapsed_ns > 0);

    number(count) * 1e9 / number(elapsed_ns)
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

    let mut received = 0_u64;
    let mut sent = 0_u64;
    for (index, now) in current.iter().enumerate() {
        let before = paired(previous, index, |row| {
            same_interface(&row.interface, &now.interface)
        })?;
        let now = &now.interface;
        let before = &before.interface;
        received = received.checked_add(now.received_bytes.checked_sub(before.received_bytes)?)?;
        sent = sent.checked_add(now.sent_bytes.checked_sub(before.sent_bytes)?)?;
    }

    Some(Traffic {
        received_bps: number(received) * 1e9 / number(elapsed_ns),
        sent_bps: number(sent) * 1e9 / number(elapsed_ns),
    })
}

fn same_interface(one: &Interface, other: &Interface) -> bool {
    if one.index != other.index {
        return false;
    }

    one.name == other.name
}

/// The row of `previous` that `matches`, looked for first at `index`, since both readings
/// list their rows in one order unless what the collector found changed.
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
