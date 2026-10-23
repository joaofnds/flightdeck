//! The machine's readings of a sealed minute, raw as the collector took them.

use collector::interfaces::{IFNAMSIZ, Interface, Name};
use collector::machine::{CPUTicks, Memory, Pressure};
use persistence::engine::Cells;
use persistence::schema::pressure;
use polars::prelude::{BooleanType, StringType, UInt8Type, UInt16Type, UInt32Type, UInt64Type};

use super::{Refused, SealedMinute};

/// The memory table's counters, in the order `Memory` declares them.
const MEMORY_COUNTERS: [&str; 18] = [
    "total_bytes",
    "free_bytes",
    "active_bytes",
    "inactive_bytes",
    "speculative_bytes",
    "wired_bytes",
    "compressed_bytes",
    "anonymous_bytes",
    "file_backed_bytes",
    "purgeable_bytes",
    "swap_total_bytes",
    "swap_used_bytes",
    "swapped_in_bytes",
    "swapped_out_bytes",
    "pageins",
    "pageouts",
    "compressions",
    "decompressions",
];

/// The interface table's counters, in the order `Interface` declares them.
const INTERFACE_COUNTERS: [&str; 7] = [
    "received_bytes",
    "sent_bytes",
    "received_packets",
    "sent_packets",
    "receive_errors",
    "send_errors",
    "receive_drops",
];

/// One minute's machine readings in publication order, as the collector took them, with no
/// rate computed. The caller owns it and reuses it from minute to minute.
#[derive(Debug, Default, PartialEq)]
pub struct MachineMinute {
    pub ticks: Vec<TickRow>,
    /// Each tick's CPUs, in publication order and then CPU order.
    pub cpus: Vec<CpuRow>,
    pub memory: Vec<MemoryRow>,
    /// Each tick's interfaces, in publication order and then interface order.
    pub interfaces: Vec<InterfaceRow>,
}

/// When a tick was taken and which sources it read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TickRow {
    pub publication: u64,
    pub run_tick: u64,
    /// Each clock is `None` where the collector did not read it at the tick.
    pub wall_ns: Option<u64>,
    pub continuous_ns: Option<u64>,
    pub awake_ns: Option<u64>,
    pub ioreport_read: bool,
    pub smc_read: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuRow {
    pub publication: u64,
    pub cpu: u32,
    pub ticks: CPUTicks,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryRow {
    pub publication: u64,
    pub memory: Memory,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InterfaceRow {
    pub publication: u64,
    pub interface: Interface,
}

impl MachineMinute {
    pub fn clear(&mut self) {
        self.ticks.clear();
        self.cpus.clear();
        self.memory.clear();
        self.interfaces.clear();
    }

    /// Appends the machine readings of `tick`, the ring's publication `publication`, as the
    /// recording holds them once sealed.
    pub fn push_tick(&mut self, publication: u64, tick: &collector::ring::Tick) {
        self.ticks.push(TickRow {
            publication,
            run_tick: tick.run_tick,
            wall_ns: tick.times.map(|times| times.wall_ns),
            continuous_ns: tick.times.map(|times| times.continuous_ns),
            awake_ns: tick.times.map(|times| times.awake_ns),
            ioreport_read: tick.ioreport_read,
            smc_read: tick.smc_read,
        });
        for (cpu, ticks) in (0_u32..).zip(&tick.cpus) {
            self.cpus.push(CpuRow {
                publication,
                cpu,
                ticks: *ticks,
            });
        }
        self.memory.push(MemoryRow {
            publication,
            memory: tick.memory,
        });
        for interface in &tick.interfaces {
            self.interfaces.push(InterfaceRow {
                publication,
                interface: *interface,
            });
        }
    }
}

/// Reads the machine's readings of `minute` into `into`, replacing what it held.
pub fn read_machine(minute: &SealedMinute, into: &mut MachineMinute) -> Result<(), Refused> {
    into.clear();
    read_ticks(minute, &mut into.ticks)?;
    read_cpus(minute, &mut into.cpus)?;
    read_memory(minute, &mut into.memory)?;
    read_interfaces(minute, &mut into.interfaces)
}

fn read_ticks(minute: &SealedMinute, into: &mut Vec<TickRow>) -> Result<(), Refused> {
    let columns = [
        "publication",
        "run_tick",
        "wall_ns",
        "continuous_ns",
        "awake_ns",
        "ioreport_read",
        "smc_read",
    ];
    let frame = minute.collect("ticks", &columns, &["publication"])?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let run_tick = Cells::<UInt64Type>::named(&frame, "run_tick")?;
    let wall_ns = Cells::<UInt64Type>::named(&frame, "wall_ns")?;
    let continuous_ns = Cells::<UInt64Type>::named(&frame, "continuous_ns")?;
    let awake_ns = Cells::<UInt64Type>::named(&frame, "awake_ns")?;
    let ioreport_read = Cells::<BooleanType>::named(&frame, "ioreport_read")?;
    let smc_read = Cells::<BooleanType>::named(&frame, "smc_read")?;
    into.reserve(frame.height());
    for row in 0..frame.height() {
        into.push(TickRow {
            publication: publication.get(row)?,
            run_tick: run_tick.get(row)?,
            wall_ns: wall_ns.optional(row)?,
            continuous_ns: continuous_ns.optional(row)?,
            awake_ns: awake_ns.optional(row)?,
            ioreport_read: ioreport_read.get(row)?,
            smc_read: smc_read.get(row)?,
        });
    }
    Ok(())
}

fn read_cpus(minute: &SealedMinute, into: &mut Vec<CpuRow>) -> Result<(), Refused> {
    let columns = [
        "publication",
        "cpu",
        "user_ticks",
        "system_ticks",
        "nice_ticks",
        "idle_ticks",
    ];
    let frame = minute.collect("cpus", &columns, &["publication", "cpu"])?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let cpu = Cells::<UInt32Type>::named(&frame, "cpu")?;
    let user = Cells::<UInt32Type>::named(&frame, "user_ticks")?;
    let system = Cells::<UInt32Type>::named(&frame, "system_ticks")?;
    let nice = Cells::<UInt32Type>::named(&frame, "nice_ticks")?;
    let idle = Cells::<UInt32Type>::named(&frame, "idle_ticks")?;
    into.reserve(frame.height());
    for row in 0..frame.height() {
        into.push(CpuRow {
            publication: publication.get(row)?,
            cpu: cpu.get(row)?,
            ticks: CPUTicks {
                user: user.get(row)?,
                system: system.get(row)?,
                nice: nice.get(row)?,
                idle: idle.get(row)?,
            },
        });
    }
    Ok(())
}

fn read_memory(minute: &SealedMinute, into: &mut Vec<MemoryRow>) -> Result<(), Refused> {
    let mut columns = Vec::with_capacity(MEMORY_COUNTERS.len() + 2);
    columns.push("publication");
    columns.extend(MEMORY_COUNTERS);
    columns.push("pressure");
    let frame = minute.collect("memory", &columns, &["publication"])?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let counters = MEMORY_COUNTERS
        .iter()
        .map(|name| Cells::<UInt64Type>::named(&frame, name))
        .collect::<persistence::Result<Vec<_>>>()?;
    let pressures = Cells::<UInt8Type>::named(&frame, "pressure")?;
    into.reserve(frame.height());
    for row in 0..frame.height() {
        let mut values = [0_u64; MEMORY_COUNTERS.len()];
        for (value, cells) in values.iter_mut().zip(&counters) {
            *value = cells.get(row)?;
        }
        let memory = memory(values, memory_pressure(pressures.get(row)?)?);
        into.push(MemoryRow {
            publication: publication.get(row)?,
            memory,
        });
    }
    Ok(())
}

/// The memory reading of the counters `values`, in `MEMORY_COUNTERS` order, and `pressure`.
fn memory(values: [u64; MEMORY_COUNTERS.len()], pressure: Pressure) -> Memory {
    let [
        total_bytes,
        free_bytes,
        active_bytes,
        inactive_bytes,
        speculative_bytes,
        wired_bytes,
        compressed_bytes,
        anonymous_bytes,
        file_backed_bytes,
        purgeable_bytes,
        swap_total_bytes,
        swap_used_bytes,
        swapped_in_bytes,
        swapped_out_bytes,
        pageins,
        pageouts,
        compressions,
        decompressions,
    ] = values;
    Memory {
        total_bytes,
        free_bytes,
        active_bytes,
        inactive_bytes,
        speculative_bytes,
        wired_bytes,
        compressed_bytes,
        anonymous_bytes,
        file_backed_bytes,
        purgeable_bytes,
        swap_total_bytes,
        swap_used_bytes,
        swapped_in_bytes,
        swapped_out_bytes,
        pageins,
        pageouts,
        compressions,
        decompressions,
        pressure,
    }
}

fn memory_pressure(code: u8) -> Result<Pressure, Refused> {
    match code {
        pressure::NORMAL => Ok(Pressure::Normal),
        pressure::WARNING => Ok(Pressure::Warning),
        pressure::CRITICAL => Ok(Pressure::Critical),
        _ => Err(persistence::Error::Bound("memory pressure code").into()),
    }
}

fn read_interfaces(minute: &SealedMinute, into: &mut Vec<InterfaceRow>) -> Result<(), Refused> {
    let mut columns = Vec::with_capacity(INTERFACE_COUNTERS.len() + 3);
    columns.extend(["publication", "interface", "name"]);
    columns.extend(INTERFACE_COUNTERS);
    let frame = minute.collect("interfaces", &columns, &["publication", "interface"])?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let index = Cells::<UInt16Type>::named(&frame, "interface")?;
    let names = Cells::<StringType>::named(&frame, "name")?;
    let counters = INTERFACE_COUNTERS
        .iter()
        .map(|name| Cells::<UInt64Type>::named(&frame, name))
        .collect::<persistence::Result<Vec<_>>>()?;
    into.reserve(frame.height());
    for row in 0..frame.height() {
        let mut values = [0_u64; INTERFACE_COUNTERS.len()];
        for (value, cells) in values.iter_mut().zip(&counters) {
            *value = cells.get(row)?;
        }
        let [
            received_bytes,
            sent_bytes,
            received_packets,
            sent_packets,
            receive_errors,
            send_errors,
            receive_drops,
        ] = values;
        let interface = Interface {
            index: index.get(row)?,
            name: interface_name(names.get(row)?)?,
            received_bytes,
            sent_bytes,
            received_packets,
            sent_packets,
            receive_errors,
            send_errors,
            receive_drops,
        };
        into.push(InterfaceRow {
            publication: publication.get(row)?,
            interface,
        });
    }
    Ok(())
}

fn interface_name(name: &str) -> Result<Name, Refused> {
    let refused = || Refused::from(persistence::Error::Bound("interface name"));
    let mut bytes = [0_u8; IFNAMSIZ];
    bytes
        .get_mut(..name.len())
        .ok_or_else(refused)?
        .copy_from_slice(name.as_bytes());
    Name::from_bytes(bytes, name.len()).ok_or_else(refused)
}
