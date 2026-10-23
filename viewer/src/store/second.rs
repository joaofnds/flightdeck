//! A second of the machine's readings as the pages draw it. The store derives it once, when it
//! takes the second in from the ring or from a sealed minute, so the main thread reads values
//! and computes no rate.

use collector::interfaces::Interface;
use collector::machine::{CPUTicks, Pressure};

use crate::metrics::number;
use crate::recording::{CpuRow, InterfaceRow, MachineMinute, MemoryRow, TickRow};

#[cfg(test)]
#[path = "second_tests.rs"]
mod tests;

/// One second of the machine's readings. A value the collector did not read, or a rate whose
/// interval the readings do not show, is `None` and never zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MachineSecond {
    pub publication: u64,
    pub run_tick: u64,
    pub wall_ns: u64,
    pub continuous_ns: Option<u64>,
    /// Whether the rates are over the interval since the run's previous reading, which holds
    /// only where the two readings are consecutive (doc-30).
    pub follows: bool,
    pub cpu_percent: Option<f64>,
    pub memory: Option<MemorySecond>,
    pub network: Option<Traffic>,
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

/// The rows of one publication of a `MachineMinute`.
#[derive(Clone, Copy, Debug)]
pub struct Reading<'a> {
    pub tick: &'a TickRow,
    pub cpus: &'a [CpuRow],
    /// `None` where the minute holds no memory row for the publication.
    pub memory: Option<&'a MemoryRow>,
    pub interfaces: &'a [InterfaceRow],
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
            follows: false,
            cpu_percent: None,
            memory: reading.memory.map(MemorySecond::of),
            network: None,
        };
        if let Some(previous) = previous {
            if let Some(elapsed_ns) = interval_ns(previous.tick, reading.tick) {
                second.follows = true;
                second.cpu_percent = cpu_percent(reading.cpus, previous.cpus);
                second.network = traffic(reading.interfaces, previous.interfaces, elapsed_ns);
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
    minute.ticks.iter().map(move |tick| {
        let publication = tick.publication;
        let (own_cpus, rest) = split_publication(cpus, publication, |row| row.publication);
        cpus = rest;
        let (own_memory, rest) = split_publication(memory, publication, |row| row.publication);
        memory = rest;
        let (own_interfaces, rest) =
            split_publication(interfaces, publication, |row| row.publication);
        interfaces = rest;

        Reading {
            tick,
            cpus: own_cpus,
            memory: own_memory.first(),
            interfaces: own_interfaces,
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
