//! The machine's readings of a sealed minute, raw as the collector took them.

use collector::battery::BatteryReading;
use collector::disks::DiskSnapshot;
use collector::interfaces::{IFNAMSIZ, Interface, Name};
use collector::machine::{CPUTicks, Memory, Pressure};
use collector::ring::Tick;
use collector::system::{FactText, PerformanceLevel};
use persistence::engine::Cells;
use persistence::schema::{
    BATTERY_FIELDS, BATTERY_VERSION, CORE_STATE_FREQUENCY_FIELD, SYSTEM_FIELDS, TEMPERATURE_FIELDS,
    TICK_COLUMNS_MAX, Table, pressure,
};
use polars::prelude::{
    BooleanType, DataFrame, Float32Type, Int64Type, PolarsPhysicalType, StringType, UInt8Type,
    UInt16Type, UInt32Type, UInt64Type,
};

use super::{Refused, SealedMinute, ordered, placement};

/// The ticks table's columns every minute holds, of those the store reads.
const TICK_COLUMNS: [&str; 10] = [
    "publication",
    "run_tick",
    "wall_ns",
    "continuous_ns",
    "awake_ns",
    "ioreport_read",
    "smc_read",
    "gpu_energy_nj",
    "power_watts",
    "soc_celsius",
];

/// The system group's columns the store reads, of those `SYSTEM_FIELDS` names.
const SYSTEM_COLUMNS: [&str; 5] = [
    "thermal_pressure",
    "machine_performance_level0_name",
    "machine_performance_level0_logical_cpus",
    "machine_performance_level1_name",
    "machine_performance_level1_logical_cpus",
];

/// Why a reading is refused that names a publication none of the minute's ticks has.
const TICKLESS: &str = "reading of no tick";
/// Why a tick's readings are refused that name an entity twice or out of the order the
/// collector read them in.
const UNORDERED: &str = "reading order";

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
    /// The GPU's states at each tick that read `IOReport`, in publication order and then state
    /// order.
    pub gpu_states: Vec<GpuStateRow>,
    /// Each core's states at each tick that read `IOReport`, in publication order, then core
    /// order, then state order.
    pub core_states: Vec<CoreStateRow>,
    /// Each fan at each tick that read the SMC, in publication order and then fan order.
    pub fans: Vec<FanRow>,
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
    /// `None` where the tick did not read `IOReport`.
    pub gpu_energy_nj: Option<u64>,
    /// Each `None` where the tick did not read the SMC.
    pub power_watts: Option<f32>,
    pub soc_celsius: Option<f32>,
    /// Each `None` where the tick did not read the SMC or the SMC lists no such sensor.
    pub cpu_celsius: Option<f32>,
    pub gpu_celsius: Option<f32>,
    /// `None` where the tick published no system readings or macOS gave no thermal pressure.
    pub thermal_pressure: Option<u8>,
    /// `None` where the tick published no system readings or macOS named no levels.
    pub performance_levels: Option<[PerformanceLevel; 2]>,
    /// `None` where the tick published no battery reading.
    pub battery: Option<BatteryReading>,
    /// `None` where the tick published no disks.
    pub disks: Option<DiskSnapshot>,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuStateRow {
    pub publication: u64,
    pub state: u32,
    pub active_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoreStateRow {
    pub publication: u64,
    pub core: u32,
    pub state: u32,
    pub active_ns: u64,
    /// The state's clock, `None` where the tick published none for it.
    pub frequency_khz: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FanRow {
    pub publication: u64,
    pub fan: u32,
    pub rpm: f32,
}

impl MachineMinute {
    pub fn clear(&mut self) {
        self.ticks.clear();
        self.cpus.clear();
        self.memory.clear();
        self.interfaces.clear();
        self.gpu_states.clear();
        self.core_states.clear();
        self.fans.clear();
    }

    /// Appends the machine readings of `tick`, the ring's publication `publication`, as the
    /// recording holds them once sealed.
    pub fn push_tick(&mut self, publication: u64, tick: &Tick) {
        self.ticks.push(TickRow {
            publication,
            run_tick: tick.run_tick,
            wall_ns: tick.times.map(|times| times.wall_ns),
            continuous_ns: tick.times.map(|times| times.continuous_ns),
            awake_ns: tick.times.map(|times| times.awake_ns),
            ioreport_read: tick.ioreport_read,
            smc_read: tick.smc_read,
            gpu_energy_nj: tick.ioreport_read.then_some(tick.gpu_energy_nj),
            power_watts: tick.smc_read.then_some(tick.power_watts),
            soc_celsius: tick.smc_read.then_some(tick.soc_celsius),
            cpu_celsius: tick.cpu_celsius,
            gpu_celsius: tick.gpu_celsius,
            thermal_pressure: tick
                .system
                .as_ref()
                .and_then(|system| system.reading.thermal_pressure),
            performance_levels: tick
                .system
                .as_ref()
                .and_then(|system| system.machine.levels),
            battery: tick.battery,
            disks: tick.disks,
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
        if tick.ioreport_read {
            self.push_tick_states(publication, tick);
        }
        if tick.smc_read {
            for (fan, rpm) in (0_u32..).zip(&tick.fan_rpm) {
                self.fans.push(FanRow {
                    publication,
                    fan,
                    rpm: *rpm,
                });
            }
        }
    }

    /// Appends the GPU's and the cores' states of `tick`, which read `IOReport`.
    fn push_tick_states(&mut self, publication: u64, tick: &Tick) {
        for (state, active_ns) in (0_u32..).zip(&tick.gpu_active_ns) {
            self.gpu_states.push(GpuStateRow {
                publication,
                state,
                active_ns: *active_ns,
            });
        }
        for (core, index) in (0_u32..).zip(0..tick.core_count()) {
            let states = tick
                .core_active_ns(index)
                .iter()
                .zip(tick.core_state_khz(index));
            for (state, (active_ns, khz)) in (0_u32..).zip(states) {
                self.core_states.push(CoreStateRow {
                    publication,
                    core,
                    state,
                    active_ns: *active_ns,
                    frequency_khz: push_tick_frequency_khz(tick, *khz),
                });
            }
        }
    }
}

/// The clock of a core's state as a sealed minute holds it: none where the tick published no
/// system readings, which carry the clocks, or the clock is 0, which the ring writes for one
/// unknown.
fn push_tick_frequency_khz(tick: &Tick, khz: u32) -> Option<u32> {
    tick.system.as_ref()?;

    if khz == 0 { None } else { Some(khz) }
}

/// Reads the machine's readings of `minute` into `into`, replacing what it held.
pub fn read_machine(minute: &SealedMinute, into: &mut MachineMinute) -> Result<(), Refused> {
    into.clear();
    read_ticks(minute, &mut into.ticks)?;

    // Every other table's rows are readings of the minute's ticks.
    let ticks: Vec<u64> = into.ticks.iter().map(|tick| tick.publication).collect();
    read_cpus(minute, &ticks, &mut into.cpus)?;
    read_memory(minute, &ticks, &mut into.memory)?;
    read_interfaces(minute, &ticks, &mut into.interfaces)?;
    read_gpu_states(minute, &ticks, &mut into.gpu_states)?;
    read_core_states(minute, &ticks, &mut into.core_states)?;
    read_fans(minute, &ticks, &mut into.fans)
}

fn read_ticks(minute: &SealedMinute, into: &mut Vec<TickRow>) -> Result<(), Refused> {
    assert!(into.is_empty());
    let temperatures = TEMPERATURE_FIELDS.map(|(name, _)| name);
    let system = SYSTEM_FIELDS.map(|(name, _)| name);
    let battery = BATTERY_FIELDS.map(|(name, _)| name);
    let disks = [persistence::disks::FIELD];
    let table = minute.table(Table::Ticks)?;
    let temperatures_held = table.holds(&temperatures)?;
    let system_held = table.holds(&system)?;
    let battery_held = table.holds(&battery)?;
    let disks_held = table.holds(&disks)?;

    let mut columns = Vec::with_capacity(TICK_COLUMNS_MAX);
    columns.extend(TICK_COLUMNS);
    if temperatures_held {
        columns.extend(temperatures);
    }
    if system_held {
        columns.extend(SYSTEM_COLUMNS);
    }
    if battery_held {
        columns.extend(battery);
    }
    if disks_held {
        columns.extend(disks);
    }
    let frame = table.collect(&columns)?;

    read_ticks_rows(&frame, into)?;
    ordered(into, "tick order", |tick| tick.publication)?;
    if temperatures_held {
        read_ticks_temperatures(&frame, into)?;
    }
    if system_held {
        read_ticks_system(&frame, into)?;
    }
    if battery_held {
        read_ticks_battery(&frame, into)?;
    }
    if disks_held {
        read_ticks_disks(&frame, into)?;
    }
    Ok(())
}

/// Appends a row for each of `frame`'s ticks from the columns every minute holds, with the
/// readings of the groups a minute may lack left out.
fn read_ticks_rows(frame: &DataFrame, into: &mut Vec<TickRow>) -> Result<(), Refused> {
    let publication = Cells::<UInt64Type>::named(frame, "publication")?;
    let run_tick = Cells::<UInt64Type>::named(frame, "run_tick")?;
    let wall_ns = Cells::<UInt64Type>::named(frame, "wall_ns")?;
    let continuous_ns = Cells::<UInt64Type>::named(frame, "continuous_ns")?;
    let awake_ns = Cells::<UInt64Type>::named(frame, "awake_ns")?;
    let ioreport_read = Cells::<BooleanType>::named(frame, "ioreport_read")?;
    let smc_read = Cells::<BooleanType>::named(frame, "smc_read")?;
    let gpu_energy_nj = Cells::<UInt64Type>::named(frame, "gpu_energy_nj")?;
    let power_watts = Cells::<Float32Type>::named(frame, "power_watts")?;
    let soc_celsius = Cells::<Float32Type>::named(frame, "soc_celsius")?;
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
            gpu_energy_nj: gpu_energy_nj.optional(row)?,
            power_watts: power_watts.optional(row)?,
            soc_celsius: soc_celsius.optional(row)?,
            cpu_celsius: None,
            gpu_celsius: None,
            thermal_pressure: None,
            performance_levels: None,
            battery: None,
            disks: None,
        });
    }
    Ok(())
}

/// Fills each of `into`'s temperatures from `frame`, which holds the temperature group.
fn read_ticks_temperatures(frame: &DataFrame, into: &mut [TickRow]) -> Result<(), Refused> {
    assert_eq!(into.len(), frame.height());
    let cpu_celsius = Cells::<Float32Type>::named(frame, "cpu_celsius")?;
    let gpu_celsius = Cells::<Float32Type>::named(frame, "gpu_celsius")?;
    for (row, tick) in into.iter_mut().enumerate() {
        tick.cpu_celsius = cpu_celsius.optional(row)?;
        tick.gpu_celsius = gpu_celsius.optional(row)?;
    }
    Ok(())
}

/// Fills each of `into`'s thermal pressure and performance levels from `frame`, which holds
/// the system group.
fn read_ticks_system(frame: &DataFrame, into: &mut [TickRow]) -> Result<(), Refused> {
    assert_eq!(into.len(), frame.height());
    let [pressure, level0_name, level0_cpus, level1_name, level1_cpus] = SYSTEM_COLUMNS;
    let thermal_pressure = Cells::<UInt8Type>::named(frame, pressure)?;
    let level0_name = Cells::<StringType>::named(frame, level0_name)?;
    let level0_cpus = Cells::<UInt32Type>::named(frame, level0_cpus)?;
    let level1_name = Cells::<StringType>::named(frame, level1_name)?;
    let level1_cpus = Cells::<UInt32Type>::named(frame, level1_cpus)?;
    for (row, tick) in into.iter_mut().enumerate() {
        tick.thermal_pressure = thermal_pressure.optional(row)?;
        tick.performance_levels = performance_levels([
            (level0_name.optional(row)?, level0_cpus.optional(row)?),
            (level1_name.optional(row)?, level1_cpus.optional(row)?),
        ])?;
    }
    Ok(())
}

/// The performance levels of a tick's level `columns`, each a name and a count of logical
/// CPUs, which hold both levels whole or nothing.
fn performance_levels(
    columns: [(Option<&str>, Option<u32>); 2],
) -> Result<Option<[PerformanceLevel; 2]>, Refused> {
    let mut levels = [PerformanceLevel::default(); 2];
    let mut held = 0_usize;
    for (level, (name, logical_cpus)) in levels.iter_mut().zip(columns) {
        if let Some(name) = name {
            level.name = FactText::new(name.as_bytes())
                .ok_or(persistence::Error::Bound("performance level name"))?;
            held += 1;
        }
        if let Some(logical_cpus) = logical_cpus {
            level.logical_cpus = logical_cpus;
            held += 1;
        }
    }
    if held == 0 {
        return Ok(None);
    }

    if held < 2 * levels.len() {
        return Err(persistence::Error::Bound("incomplete performance levels").into());
    }

    Ok(Some(levels))
}

/// Fills each of `into`'s battery reading from `frame`, which holds the battery group,
/// refusing a row of another version, or one with readings and no source time.
fn read_ticks_battery(frame: &DataFrame, into: &mut [TickRow]) -> Result<(), Refused> {
    assert_eq!(into.len(), frame.height());
    let version = Cells::<UInt8Type>::named(frame, "battery_version")?;
    let source_ns = Cells::<UInt64Type>::named(frame, "battery_source_continuous_ns")?;
    let charge_current = Cells::<UInt64Type>::named(frame, "battery_charge_current")?;
    let charge_maximum = Cells::<UInt64Type>::named(frame, "battery_charge_maximum")?;
    let full_capacity = Cells::<UInt64Type>::named(frame, "battery_full_charge_capacity")?;
    let design_capacity = Cells::<UInt64Type>::named(frame, "battery_design_capacity")?;
    let cycle_count = Cells::<UInt64Type>::named(frame, "battery_cycle_count")?;
    let charging = Cells::<BooleanType>::named(frame, "battery_charging")?;
    let external_power = Cells::<BooleanType>::named(frame, "battery_external_power")?;
    let voltage_mv = Cells::<UInt64Type>::named(frame, "battery_voltage_mv")?;
    let current_ma = Cells::<Int64Type>::named(frame, "battery_current_ma")?;
    let adapter_watts = Cells::<UInt64Type>::named(frame, "battery_adapter_watts")?;
    for (row, tick) in into.iter_mut().enumerate() {
        if version.get(row)? != BATTERY_VERSION {
            return Err(persistence::Error::Bound("battery version").into());
        }

        let reading = BatteryReading {
            source_continuous_ns: 0,
            charge_current: charge_current.optional(row)?,
            charge_maximum: charge_maximum.optional(row)?,
            full_charge_capacity: full_capacity.optional(row)?,
            design_capacity: design_capacity.optional(row)?,
            cycle_count: cycle_count.optional(row)?,
            charging: charging.optional(row)?,
            external_power: external_power.optional(row)?,
            voltage_mv: voltage_mv.optional(row)?,
            current_ma: current_ma.optional(row)?,
            adapter_watts: adapter_watts.optional(row)?,
        };
        tick.battery = battery(source_ns.optional(row)?, reading)?;
    }
    Ok(())
}

/// The battery reading of a row whose source time is `source_continuous_ns` and whose other
/// columns hold `reading`, which a row without a source time leaves empty.
fn battery(
    source_continuous_ns: Option<u64>,
    reading: BatteryReading,
) -> Result<Option<BatteryReading>, Refused> {
    let Some(source_continuous_ns) = source_continuous_ns else {
        if reading != BatteryReading::default() {
            return Err(persistence::Error::Bound("battery payload without source time").into());
        }

        return Ok(None);
    };

    Ok(Some(BatteryReading {
        source_continuous_ns,
        ..reading
    }))
}

/// Fills each of `into`'s disks from `frame`, which holds the disks column, in the
/// collector's own encoding.
fn read_ticks_disks(frame: &DataFrame, into: &mut [TickRow]) -> Result<(), Refused> {
    assert_eq!(into.len(), frame.height());
    let disks = Cells::<StringType>::named(frame, persistence::disks::FIELD)?;
    for (row, tick) in into.iter_mut().enumerate() {
        if let Some(text) = disks.optional(row)? {
            let snapshot = tick.disks.insert(DiskSnapshot::default());
            persistence::disks::decode(text, snapshot)?;
        }
    }
    Ok(())
}

/// The cell of row `row` of `cells`, an optional column, `None` where the minute lacks the
/// column or the cell holds no value.
fn optional_cell<'a, T: PolarsPhysicalType>(
    cells: Option<&Cells<'a, T>>,
    row: usize,
) -> Result<Option<T::Physical<'a>>, Refused> {
    let Some(cells) = cells else {
        return Ok(None);
    };

    Ok(cells.optional(row)?)
}

fn read_cpus(minute: &SealedMinute, ticks: &[u64], into: &mut Vec<CpuRow>) -> Result<(), Refused> {
    let columns = [
        "publication",
        "cpu",
        "user_ticks",
        "system_ticks",
        "nice_ticks",
        "idle_ticks",
    ];
    let frame = minute.table(Table::Cpus)?.collect(&columns)?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let cpu = Cells::<UInt32Type>::named(&frame, "cpu")?;
    let user = Cells::<UInt32Type>::named(&frame, "user_ticks")?;
    let system = Cells::<UInt32Type>::named(&frame, "system_ticks")?;
    let nice = Cells::<UInt32Type>::named(&frame, "nice_ticks")?;
    let idle = Cells::<UInt32Type>::named(&frame, "idle_ticks")?;
    let order = placement(ticks, &publication, frame.height(), TICKLESS)?;
    into.reserve(order.len());
    for row in order {
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
    ordered(into, UNORDERED, |row| (row.publication, row.cpu))
}

fn read_memory(
    minute: &SealedMinute,
    ticks: &[u64],
    into: &mut Vec<MemoryRow>,
) -> Result<(), Refused> {
    let mut columns = Vec::with_capacity(MEMORY_COUNTERS.len() + 2);
    columns.push("publication");
    columns.extend(MEMORY_COUNTERS);
    columns.push("pressure");
    let frame = minute.table(Table::Memory)?.collect(&columns)?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let counters = MEMORY_COUNTERS
        .iter()
        .map(|name| Cells::<UInt64Type>::named(&frame, name))
        .collect::<persistence::Result<Vec<_>>>()?;
    let pressures = Cells::<UInt8Type>::named(&frame, "pressure")?;
    let order = placement(ticks, &publication, frame.height(), TICKLESS)?;
    into.reserve(order.len());
    for row in order {
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
    ordered(into, UNORDERED, |row| row.publication)
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

fn read_interfaces(
    minute: &SealedMinute,
    ticks: &[u64],
    into: &mut Vec<InterfaceRow>,
) -> Result<(), Refused> {
    let mut columns = Vec::with_capacity(INTERFACE_COUNTERS.len() + 3);
    columns.extend(["publication", "interface", "name"]);
    columns.extend(INTERFACE_COUNTERS);
    let frame = minute.table(Table::Interfaces)?.collect(&columns)?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let index = Cells::<UInt16Type>::named(&frame, "interface")?;
    let names = Cells::<StringType>::named(&frame, "name")?;
    let counters = INTERFACE_COUNTERS
        .iter()
        .map(|name| Cells::<UInt64Type>::named(&frame, name))
        .collect::<persistence::Result<Vec<_>>>()?;
    let order = placement(ticks, &publication, frame.height(), TICKLESS)?;
    into.reserve(order.len());
    for row in order {
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
    ordered(into, UNORDERED, |row| {
        (row.publication, row.interface.index)
    })
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

fn read_gpu_states(
    minute: &SealedMinute,
    ticks: &[u64],
    into: &mut Vec<GpuStateRow>,
) -> Result<(), Refused> {
    let columns = ["publication", "state", "active_ns"];
    let frame = minute.table(Table::GpuStates)?.collect(&columns)?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let state = Cells::<UInt32Type>::named(&frame, "state")?;
    let active_ns = Cells::<UInt64Type>::named(&frame, "active_ns")?;
    let order = placement(ticks, &publication, frame.height(), TICKLESS)?;
    into.reserve(order.len());
    for row in order {
        into.push(GpuStateRow {
            publication: publication.get(row)?,
            state: state.get(row)?,
            active_ns: active_ns.get(row)?,
        });
    }
    ordered(into, UNORDERED, |row| (row.publication, row.state))
}

fn read_core_states(
    minute: &SealedMinute,
    ticks: &[u64],
    into: &mut Vec<CoreStateRow>,
) -> Result<(), Refused> {
    let (clock, _) = CORE_STATE_FREQUENCY_FIELD;
    let table = minute.table(Table::CoreStates)?;
    let clocked = table.holds(&[clock])?;
    let mut columns = Vec::with_capacity(5);
    columns.extend(["publication", "core", "state", "active_ns"]);
    if clocked {
        columns.push(clock);
    }
    let frame = table.collect(&columns)?;

    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let core = Cells::<UInt32Type>::named(&frame, "core")?;
    let state = Cells::<UInt32Type>::named(&frame, "state")?;
    let active_ns = Cells::<UInt64Type>::named(&frame, "active_ns")?;
    let frequency_khz = clocked
        .then(|| Cells::<UInt32Type>::named(&frame, clock))
        .transpose()?;
    let order = placement(ticks, &publication, frame.height(), TICKLESS)?;
    into.reserve(order.len());
    for row in order {
        into.push(CoreStateRow {
            publication: publication.get(row)?,
            core: core.get(row)?,
            state: state.get(row)?,
            active_ns: active_ns.get(row)?,
            frequency_khz: optional_cell(frequency_khz.as_ref(), row)?,
        });
    }
    ordered(into, UNORDERED, |row| {
        (row.publication, row.core, row.state)
    })
}

fn read_fans(minute: &SealedMinute, ticks: &[u64], into: &mut Vec<FanRow>) -> Result<(), Refused> {
    let columns = ["publication", "fan", "rpm"];
    let frame = minute.table(Table::Fans)?.collect(&columns)?;
    let publication = Cells::<UInt64Type>::named(&frame, "publication")?;
    let fan = Cells::<UInt32Type>::named(&frame, "fan")?;
    let rpm = Cells::<Float32Type>::named(&frame, "rpm")?;
    let order = placement(ticks, &publication, frame.height(), TICKLESS)?;
    into.reserve(order.len());
    for row in order {
        into.push(FanRow {
            publication: publication.get(row)?,
            fan: fan.get(row)?,
            rpm: rpm.get(row)?,
        });
    }
    ordered(into, UNORDERED, |row| (row.publication, row.fan))
}
