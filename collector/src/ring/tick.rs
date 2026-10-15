//! A tick's readings, as the writer stores them in a slot and a reader loads them back. Each
//! reading becomes whole 64-bit words, two 32-bit values sharing a word where a reading holds
//! them, so the writer stores and a reader loads every word as an atomic.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use super::layout::{
    COALITION_WORDS, CPU_WORDS, INTERFACE_WORDS, Layout, MEMORY_WORDS, PROCESS_WORDS,
    SLOT_COALITION_COUNT, SLOT_CORE_COUNT, SLOT_CPU_COUNT, SLOT_DROPPED_EXECS, SLOT_DROPPED_EXITS,
    SLOT_DROPPED_STARTS, SLOT_EVENTS_END, SLOT_EVENTS_FIRST, SLOT_FAN_COUNT, SLOT_GPU_ENERGY_NJ,
    SLOT_GPU_STATE_COUNT, SLOT_INTERFACE_COUNT, SLOT_MEMORY, SLOT_POWER_WATTS, SLOT_PROCESS_COUNT,
    SLOT_PROCESSES_GONE, SLOT_PROCESSES_REFUSED, SLOT_RUN_TICK, SLOT_SOC_CELSIUS, SLOT_SOURCES,
    SOURCE_IOREPORT, SOURCE_SMC,
};
use crate::coalitions::Coalition;
use crate::interfaces::{IFNAMSIZ, Interface, Name};
use crate::ioreport::States;
use crate::machine::{CPUTicks, Memory, Pressure};
use crate::processes::{Process, Task, Usage};
use crate::tracker::Event;
use crate::{push_without_growing, u64_from_usize};

/// What the collector read in one tick, as the writer publishes it.
pub struct Readings<'a> {
    pub run_tick: u64,
    pub cpus: &'a [CPUTicks],
    pub memory: &'a Memory,
    pub interfaces: &'a [Interface],
    /// `None` where the collector reads no `IOReport`.
    pub ioreport: Option<IOReportReadings<'a>>,
    /// `None` where the collector reads no SMC.
    pub smc: Option<SmcReadings<'a>>,
    pub processes: &'a [Process],
    pub processes_refused: u32,
    pub processes_gone: u32,
    pub coalitions: &'a [Coalition],
    /// The process events the tracker drained in the tick, which go to the event log.
    pub events: &'a [Event],
    pub dropped: Dropped,
}

/// The process events of each kind the tracker dropped in a tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dropped {
    pub starts: u64,
    pub execs: u64,
    pub exits: u64,
}

pub struct IOReportReadings<'a> {
    pub gpu_energy_nj: u64,
    pub gpu: &'a States,
    pub cores: &'a [States],
}

pub struct SmcReadings<'a> {
    pub power_watts: f32,
    pub soc_celsius: f32,
    pub fan_rpm: &'a [f32],
}

/// A tick as a reader loads it, in lists with room taken once for all the ring's shape holds.
#[derive(Debug, PartialEq)]
pub struct Tick {
    pub run_tick: u64,
    pub cpus: Vec<CPUTicks>,
    pub memory: Memory,
    pub interfaces: Vec<Interface>,
    /// Whether the tick read `IOReport`, which the GPU's and the cores' readings come from.
    pub ioreport_read: bool,
    pub gpu_energy_nj: u64,
    pub gpu_active_ns: Vec<u64>,
    /// The active states of each core, at `state_capacity` words apart.
    core_active_ns: Vec<u64>,
    core_state_counts: Vec<usize>,
    state_capacity: usize,
    /// Whether the tick read the SMC, which the power, temperature and fans come from.
    pub smc_read: bool,
    pub power_watts: f32,
    pub soc_celsius: f32,
    pub fan_rpm: Vec<f32>,
    pub processes: Vec<Process>,
    pub processes_refused: u32,
    pub processes_gone: u32,
    pub coalitions: Vec<Coalition>,
    /// The numbers of the tick's events in the event log, from the first up to the end.
    pub events: Range<u64>,
    pub dropped: Dropped,
}

impl Tick {
    pub(super) fn new(layout: &Layout) -> Self {
        let state_capacity = layout.gpu_states.capacity;
        Self {
            run_tick: 0,
            cpus: Vec::with_capacity(layout.cpus.capacity),
            memory: Memory::default(),
            interfaces: Vec::with_capacity(layout.interfaces.capacity),
            ioreport_read: false,
            gpu_energy_nj: 0,
            gpu_active_ns: Vec::with_capacity(state_capacity),
            core_active_ns: vec![0; layout.cores.capacity * state_capacity],
            core_state_counts: Vec::with_capacity(layout.cores.capacity),
            state_capacity,
            smc_read: false,
            power_watts: 0.0,
            soc_celsius: 0.0,
            fan_rpm: Vec::with_capacity(layout.fans.capacity),
            processes: Vec::with_capacity(layout.processes.capacity),
            processes_refused: 0,
            processes_gone: 0,
            coalitions: Vec::with_capacity(layout.coalitions.capacity),
            events: 0..0,
            dropped: Dropped::default(),
        }
    }

    /// The cores whose active states the tick holds.
    #[must_use]
    pub fn core_count(&self) -> usize {
        self.core_state_counts.len()
    }

    /// The time core `core` spent in each of its active states, in nanoseconds.
    #[must_use]
    pub fn core_active_ns(&self, core: usize) -> &[u64] {
        assert!(core < self.core_state_counts.len());
        assert!(self.core_state_counts[core] <= self.state_capacity);

        let start = core * self.state_capacity;
        &self.core_active_ns[start..start + self.core_state_counts[core]]
    }
}

/// Stores `readings` in `slot`, a slot of `layout` whose sequence word the caller keeps, with
/// the numbers `events` the log gave the tick's events.
pub(super) fn store_tick(
    slot: &[AtomicU64],
    layout: &Layout,
    readings: &Readings,
    events: Range<u64>,
) {
    assert_eq!(layout.slot_words, slot.len());
    assert!(events.start <= events.end);
    assert_eq!(
        u64_from_usize(readings.events.len()),
        events.end - events.start
    );

    store_tick_counts(slot, readings);
    let dropped = readings.dropped;
    let words = [
        (SLOT_EVENTS_FIRST, events.start),
        (SLOT_EVENTS_END, events.end),
        (SLOT_DROPPED_STARTS, dropped.starts),
        (SLOT_DROPPED_EXECS, dropped.execs),
        (SLOT_DROPPED_EXITS, dropped.exits),
    ];
    for (index, value) in words {
        store(slot, index, value);
    }
    store_words(
        &slot[SLOT_MEMORY..SLOT_MEMORY + MEMORY_WORDS],
        &memory_words(readings.memory),
    );
    for (index, cpu) in readings.cpus.iter().enumerate() {
        store_words(&slot[layout.cpus.record(index)], &cpu_words(cpu));
    }
    for (index, interface) in readings.interfaces.iter().enumerate() {
        store_words(
            &slot[layout.interfaces.record(index)],
            &interface_words(interface),
        );
    }
    if let Some(ioreport) = &readings.ioreport {
        store_tick_ioreport(slot, layout, ioreport);
    }
    if let Some(smc) = &readings.smc {
        store(slot, SLOT_POWER_WATTS, u64::from(smc.power_watts.to_bits()));
        store(slot, SLOT_SOC_CELSIUS, u64::from(smc.soc_celsius.to_bits()));
        for (index, rpm) in smc.fan_rpm.iter().enumerate() {
            store(
                slot,
                layout.fans.record(index).start,
                u64::from(rpm.to_bits()),
            );
        }
    }
    for (index, process) in readings.processes.iter().enumerate() {
        store_words(
            &slot[layout.processes.record(index)],
            &process_words(process),
        );
    }
    for (index, coalition) in readings.coalitions.iter().enumerate() {
        store_words(
            &slot[layout.coalitions.record(index)],
            &coalition_words(coalition),
        );
    }
}

/// Stores the tick number, the count of each section's records, and which sources it read.
fn store_tick_counts(slot: &[AtomicU64], readings: &Readings) {
    let (gpu_states, cores) = match &readings.ioreport {
        Some(ioreport) => (ioreport.gpu.active_ns().len(), ioreport.cores.len()),
        None => (0, 0),
    };
    let fans = readings.smc.as_ref().map_or(0, |smc| smc.fan_rpm.len());
    let mut sources = 0;
    if readings.ioreport.is_some() {
        sources |= SOURCE_IOREPORT;
    }
    if readings.smc.is_some() {
        sources |= SOURCE_SMC;
    }

    let words = [
        (SLOT_RUN_TICK, readings.run_tick),
        (
            SLOT_PROCESSES_REFUSED,
            u64::from(readings.processes_refused),
        ),
        (SLOT_PROCESSES_GONE, u64::from(readings.processes_gone)),
        (SLOT_CPU_COUNT, u64_from_usize(readings.cpus.len())),
        (
            SLOT_INTERFACE_COUNT,
            u64_from_usize(readings.interfaces.len()),
        ),
        (SLOT_GPU_STATE_COUNT, u64_from_usize(gpu_states)),
        (SLOT_CORE_COUNT, u64_from_usize(cores)),
        (SLOT_FAN_COUNT, u64_from_usize(fans)),
        (SLOT_PROCESS_COUNT, u64_from_usize(readings.processes.len())),
        (
            SLOT_COALITION_COUNT,
            u64_from_usize(readings.coalitions.len()),
        ),
        (SLOT_SOURCES, sources),
    ];
    for (index, value) in words {
        store(slot, index, value);
    }
}

fn store_tick_ioreport(slot: &[AtomicU64], layout: &Layout, ioreport: &IOReportReadings) {
    store(slot, SLOT_GPU_ENERGY_NJ, ioreport.gpu_energy_nj);
    let gpu = ioreport.gpu.active_ns();
    let gpu_start = layout.gpu_states.start;
    assert!(gpu.len() <= layout.gpu_states.capacity);
    store_words(&slot[gpu_start..gpu_start + gpu.len()], gpu);
    for (index, core) in ioreport.cores.iter().enumerate() {
        let record = layout.cores.record(index);
        let active_ns = core.active_ns();
        // A record holds the count and then a word for each state the shape has room for.
        assert!(active_ns.len() < record.len());
        store(slot, record.start, u64_from_usize(active_ns.len()));
        store_words(
            &slot[record.start + 1..record.start + 1 + active_ns.len()],
            active_ns,
        );
    }
}

/// Loads the tick `slot` holds into `tick`, and returns whether every word made a reading,
/// which a slot the writer overwrote during the load may not.
pub(super) fn load_tick(slot: &[AtomicU64], layout: &Layout, tick: &mut Tick) -> bool {
    assert_eq!(layout.slot_words, slot.len());

    let count = |index: usize, capacity: usize| {
        let count = slot[index].load(Ordering::Relaxed);
        usize::try_from(count)
            .ok()
            .filter(|&count| count <= capacity)
    };
    let (Some(cpus), Some(interfaces), Some(processes), Some(coalitions)) = (
        count(SLOT_CPU_COUNT, layout.cpus.capacity),
        count(SLOT_INTERFACE_COUNT, layout.interfaces.capacity),
        count(SLOT_PROCESS_COUNT, layout.processes.capacity),
        count(SLOT_COALITION_COUNT, layout.coalitions.capacity),
    ) else {
        return false;
    };

    tick.run_tick = slot[SLOT_RUN_TICK].load(Ordering::Relaxed);
    tick.processes_refused = load_half(slot, SLOT_PROCESSES_REFUSED);
    tick.processes_gone = load_half(slot, SLOT_PROCESSES_GONE);
    let events_first = slot[SLOT_EVENTS_FIRST].load(Ordering::Relaxed);
    let events_end = slot[SLOT_EVENTS_END].load(Ordering::Relaxed);
    if events_first > events_end {
        return false;
    }

    tick.events = events_first..events_end;
    tick.dropped = Dropped {
        starts: slot[SLOT_DROPPED_STARTS].load(Ordering::Relaxed),
        execs: slot[SLOT_DROPPED_EXECS].load(Ordering::Relaxed),
        exits: slot[SLOT_DROPPED_EXITS].load(Ordering::Relaxed),
    };
    let Some(memory) = memory_from(&load_words(&slot[SLOT_MEMORY..SLOT_MEMORY + MEMORY_WORDS]))
    else {
        return false;
    };

    tick.memory = memory;

    load_tick_cpus(slot, layout, cpus, tick);
    if !load_tick_interfaces(slot, layout, interfaces, tick) {
        return false;
    }

    let sources = slot[SLOT_SOURCES].load(Ordering::Relaxed);
    tick.ioreport_read = sources & SOURCE_IOREPORT != 0;
    tick.smc_read = sources & SOURCE_SMC != 0;
    load_tick_clear_sources(tick);
    if tick.ioreport_read {
        if !load_tick_ioreport(slot, layout, tick) {
            return false;
        }
    }

    if tick.smc_read {
        if !load_tick_smc(slot, layout, tick) {
            return false;
        }
    }

    load_tick_processes(slot, layout, processes, tick);
    load_tick_coalitions(slot, layout, coalitions, tick);
    true
}

/// Loads the first `cpus` CPU records of `slot` into `tick`.
fn load_tick_cpus(slot: &[AtomicU64], layout: &Layout, cpus: usize, tick: &mut Tick) {
    tick.cpus.clear();
    for index in 0..cpus {
        push_without_growing(
            &mut tick.cpus,
            cpu_from(&load_words(&slot[layout.cpus.record(index)])),
        );
    }
}

/// Loads the first `interfaces` interface records of `slot` into `tick`, and returns whether
/// each made an interface.
fn load_tick_interfaces(
    slot: &[AtomicU64],
    layout: &Layout,
    interfaces: usize,
    tick: &mut Tick,
) -> bool {
    tick.interfaces.clear();
    for index in 0..interfaces {
        match interface_from(&load_words(&slot[layout.interfaces.record(index)])) {
            Some(interface) => push_without_growing(&mut tick.interfaces, interface),
            None => return false,
        }
    }
    true
}

/// Clears the readings of `IOReport` and the SMC, which a tick that did not read them holds
/// as zero and empty.
fn load_tick_clear_sources(tick: &mut Tick) {
    tick.gpu_energy_nj = 0;
    tick.gpu_active_ns.clear();
    tick.core_state_counts.clear();
    tick.power_watts = 0.0;
    tick.soc_celsius = 0.0;
    tick.fan_rpm.clear();
}

fn load_tick_ioreport(slot: &[AtomicU64], layout: &Layout, tick: &mut Tick) -> bool {
    tick.gpu_energy_nj = slot[SLOT_GPU_ENERGY_NJ].load(Ordering::Relaxed);
    let gpu_states = slot[SLOT_GPU_STATE_COUNT].load(Ordering::Relaxed);
    let cores = slot[SLOT_CORE_COUNT].load(Ordering::Relaxed);
    let (Some(gpu_states), Some(cores)) = (
        usize::try_from(gpu_states)
            .ok()
            .filter(|&count| count <= layout.gpu_states.capacity),
        usize::try_from(cores)
            .ok()
            .filter(|&count| count <= layout.cores.capacity),
    ) else {
        return false;
    };

    let gpu_start = layout.gpu_states.start;
    for word in &slot[gpu_start..gpu_start + gpu_states] {
        push_without_growing(&mut tick.gpu_active_ns, word.load(Ordering::Relaxed));
    }
    for index in 0..cores {
        let record = layout.cores.record(index);
        let states = slot[record.start].load(Ordering::Relaxed);
        let Some(states) = usize::try_from(states)
            .ok()
            .filter(|&count| count < record.len())
        else {
            return false;
        };

        let target = index * tick.state_capacity;
        for (offset, word) in slot[record.start + 1..record.start + 1 + states]
            .iter()
            .enumerate()
        {
            tick.core_active_ns[target + offset] = word.load(Ordering::Relaxed);
        }
        push_without_growing(&mut tick.core_state_counts, states);
    }
    true
}

fn load_tick_smc(slot: &[AtomicU64], layout: &Layout, tick: &mut Tick) -> bool {
    let fans = slot[SLOT_FAN_COUNT].load(Ordering::Relaxed);
    let Some(fans) = usize::try_from(fans)
        .ok()
        .filter(|&count| count <= layout.fans.capacity)
    else {
        return false;
    };

    tick.power_watts = f32::from_bits(load_half(slot, SLOT_POWER_WATTS));
    tick.soc_celsius = f32::from_bits(load_half(slot, SLOT_SOC_CELSIUS));
    for index in 0..fans {
        let word = load_half(slot, layout.fans.record(index).start);
        push_without_growing(&mut tick.fan_rpm, f32::from_bits(word));
    }
    true
}

fn load_tick_processes(slot: &[AtomicU64], layout: &Layout, count: usize, tick: &mut Tick) {
    assert!(count <= tick.processes.capacity());

    tick.processes.clear();
    for index in 0..count {
        let words = load_words(&slot[layout.processes.record(index)]);
        push_without_growing(&mut tick.processes, process_from(&words));
    }
}

fn load_tick_coalitions(slot: &[AtomicU64], layout: &Layout, count: usize, tick: &mut Tick) {
    assert!(count <= tick.coalitions.capacity());

    tick.coalitions.clear();
    for index in 0..count {
        let words = load_words(&slot[layout.coalitions.record(index)]);
        push_without_growing(&mut tick.coalitions, coalition_from(&words));
    }
}

fn store(slot: &[AtomicU64], index: usize, value: u64) {
    slot[index].store(value, Ordering::Relaxed);
}

/// Stores `words` in `target`, which is as long.
pub(super) fn store_words(target: &[AtomicU64], words: &[u64]) {
    assert_eq!(target.len(), words.len());

    for (target, &word) in target.iter().zip(words) {
        target.store(word, Ordering::Relaxed);
    }
}

/// Loads the `N` words of `source`.
pub(super) fn load_words<const N: usize>(source: &[AtomicU64]) -> [u64; N] {
    assert_eq!(N, source.len());

    let mut words = [0; N];
    for (word, source) in words.iter_mut().zip(source) {
        *word = source.load(Ordering::Relaxed);
    }
    words
}

/// The low half of the word at `index`, which holds a 32-bit value.
fn load_half(slot: &[AtomicU64], index: usize) -> u32 {
    low_half(slot[index].load(Ordering::Relaxed))
}

pub(super) fn low_half(word: u64) -> u32 {
    match u32::try_from(word & u64::from(u32::MAX)) {
        Ok(half) => half,
        Err(error) => unreachable!("{error}"),
    }
}

pub(super) fn high_half(word: u64) -> u32 {
    match u32::try_from(word >> 32) {
        Ok(half) => half,
        Err(error) => unreachable!("{error}"),
    }
}

/// Two 32-bit values one word holds, `low` in its low half.
pub(super) struct Halves {
    pub(super) low: u32,
    pub(super) high: u32,
}

impl Halves {
    pub(super) fn word(self) -> u64 {
        u64::from(self.low) | (u64::from(self.high) << 32)
    }
}

fn cpu_words(cpu: &CPUTicks) -> [u64; CPU_WORDS] {
    [
        Halves {
            low: cpu.user,
            high: cpu.system,
        }
        .word(),
        Halves {
            low: cpu.nice,
            high: cpu.idle,
        }
        .word(),
    ]
}

fn cpu_from(words: &[u64; CPU_WORDS]) -> CPUTicks {
    CPUTicks {
        user: low_half(words[0]),
        system: high_half(words[0]),
        nice: low_half(words[1]),
        idle: high_half(words[1]),
    }
}

/// The word of a memory record that holds the pressure, its last.
const MEMORY_PRESSURE: usize = MEMORY_WORDS - 1;
/// The memory pressure, as the pressure word holds it.
const PRESSURE_NORMAL: u64 = 0;
const PRESSURE_WARNING: u64 = 1;
const PRESSURE_CRITICAL: u64 = 2;

fn memory_words(memory: &Memory) -> [u64; MEMORY_WORDS] {
    let pressure = match memory.pressure {
        Pressure::Normal => PRESSURE_NORMAL,
        Pressure::Warning => PRESSURE_WARNING,
        Pressure::Critical => PRESSURE_CRITICAL,
    };
    [
        memory.total_bytes,
        memory.free_bytes,
        memory.active_bytes,
        memory.inactive_bytes,
        memory.speculative_bytes,
        memory.wired_bytes,
        memory.compressed_bytes,
        memory.anonymous_bytes,
        memory.file_backed_bytes,
        memory.purgeable_bytes,
        memory.swap_total_bytes,
        memory.swap_used_bytes,
        memory.swapped_in_bytes,
        memory.swapped_out_bytes,
        memory.pageins,
        memory.pageouts,
        memory.compressions,
        memory.decompressions,
        pressure,
    ]
}

fn memory_from(words: &[u64; MEMORY_WORDS]) -> Option<Memory> {
    let pressure = match words[MEMORY_PRESSURE] {
        PRESSURE_NORMAL => Pressure::Normal,
        PRESSURE_WARNING => Pressure::Warning,
        PRESSURE_CRITICAL => Pressure::Critical,
        _ => return None,
    };
    Some(Memory {
        total_bytes: words[0],
        free_bytes: words[1],
        active_bytes: words[2],
        inactive_bytes: words[3],
        speculative_bytes: words[4],
        wired_bytes: words[5],
        compressed_bytes: words[6],
        anonymous_bytes: words[7],
        file_backed_bytes: words[8],
        purgeable_bytes: words[9],
        swap_total_bytes: words[10],
        swap_used_bytes: words[11],
        swapped_in_bytes: words[12],
        swapped_out_bytes: words[13],
        pageins: words[14],
        pageouts: words[15],
        compressions: words[16],
        decompressions: words[17],
        pressure,
    })
}

/// Where an interface's first word holds the length of its name, above its 16-bit index.
const INTERFACE_NAME_LENGTH_SHIFT: u32 = 16;

fn interface_words(interface: &Interface) -> [u64; INTERFACE_WORDS] {
    const { assert!(IFNAMSIZ == 2 * size_of::<u64>()) };
    let name = interface.name.bytes();
    let mut low = [0; 8];
    low.copy_from_slice(&name[..8]);
    let mut high = [0; 8];
    high.copy_from_slice(&name[8..]);
    let length = u64_from_usize(interface.name.as_str().len());
    [
        u64::from(interface.index) | (length << INTERFACE_NAME_LENGTH_SHIFT),
        u64::from_le_bytes(low),
        u64::from_le_bytes(high),
        interface.received_bytes,
        interface.sent_bytes,
        interface.received_packets,
        interface.sent_packets,
        interface.receive_errors,
        interface.send_errors,
        interface.receive_drops,
    ]
}

fn interface_from(words: &[u64; INTERFACE_WORDS]) -> Option<Interface> {
    let mut name = [0; IFNAMSIZ];
    name[..8].copy_from_slice(&words[1].to_le_bytes());
    name[8..].copy_from_slice(&words[2].to_le_bytes());
    let length = usize::try_from(words[0] >> INTERFACE_NAME_LENGTH_SHIFT).ok()?;
    let index = u16::try_from(words[0] & u64::from(u16::MAX)).ok()?;
    Some(Interface {
        index,
        name: Name::from_bytes(name, length)?,
        received_bytes: words[3],
        sent_bytes: words[4],
        received_packets: words[5],
        sent_packets: words[6],
        receive_errors: words[7],
        send_errors: words[8],
        receive_drops: words[9],
    })
}

/// The words of a `Usage`, which a process record and an exit event both hold.
pub(super) const USAGE_WORDS: usize = 14;

pub(super) fn usage_words(usage: &Usage) -> [u64; USAGE_WORDS] {
    [
        usage.awake_ns_at_start,
        usage.user_ns,
        usage.system_ns,
        usage.energy_nj,
        usage.instructions,
        usage.super_instructions,
        usage.cycles,
        usage.idle_wakeups,
        usage.interrupt_wakeups,
        usage.pageins,
        usage.resident_bytes,
        usage.footprint_bytes,
        usage.disk_read_bytes,
        usage.disk_written_bytes,
    ]
}

pub(super) fn usage_from(words: &[u64; USAGE_WORDS]) -> Usage {
    Usage {
        awake_ns_at_start: words[0],
        user_ns: words[1],
        system_ns: words[2],
        energy_nj: words[3],
        instructions: words[4],
        super_instructions: words[5],
        cycles: words[6],
        idle_wakeups: words[7],
        interrupt_wakeups: words[8],
        pageins: words[9],
        resident_bytes: words[10],
        footprint_bytes: words[11],
        disk_read_bytes: words[12],
        disk_written_bytes: words[13],
    }
}

fn process_words(process: &Process) -> [u64; PROCESS_WORDS] {
    const { assert!(PROCESS_WORDS == 1 + USAGE_WORDS + 3) };
    let usage = usage_words(&process.usage);
    let task = &process.task;
    let mut words = [0; PROCESS_WORDS];
    words[0] = u64::from(process.pid.cast_unsigned());
    words[1..=USAGE_WORDS].copy_from_slice(&usage);
    words[USAGE_WORDS + 1] = Halves {
        low: task.threads.cast_unsigned(),
        high: task.faults.cast_unsigned(),
    }
    .word();
    words[USAGE_WORDS + 2] = Halves {
        low: task.context_switches.cast_unsigned(),
        high: task.mach_syscalls.cast_unsigned(),
    }
    .word();
    words[USAGE_WORDS + 3] = u64::from(task.unix_syscalls.cast_unsigned());
    words
}

fn process_from(words: &[u64; PROCESS_WORDS]) -> Process {
    let mut usage = [0; USAGE_WORDS];
    usage.copy_from_slice(&words[1..=USAGE_WORDS]);
    Process {
        pid: low_half(words[0]).cast_signed(),
        usage: usage_from(&usage),
        task: Task {
            threads: low_half(words[USAGE_WORDS + 1]).cast_signed(),
            faults: high_half(words[USAGE_WORDS + 1]).cast_signed(),
            context_switches: low_half(words[USAGE_WORDS + 2]).cast_signed(),
            mach_syscalls: high_half(words[USAGE_WORDS + 2]).cast_signed(),
            unix_syscalls: low_half(words[USAGE_WORDS + 3]).cast_signed(),
        },
    }
}

fn coalition_words(coalition: &Coalition) -> [u64; COALITION_WORDS] {
    [
        coalition.id,
        coalition.tasks_started,
        coalition.tasks_exited,
        coalition.cpu_ns,
        coalition.interrupt_wakeups,
        coalition.idle_wakeups,
        coalition.disk_read_bytes,
        coalition.disk_written_bytes,
        coalition.gpu_ns,
        coalition.energy_nj,
    ]
}

fn coalition_from(words: &[u64; COALITION_WORDS]) -> Coalition {
    Coalition {
        id: words[0],
        tasks_started: words[1],
        tasks_exited: words[2],
        cpu_ns: words[3],
        interrupt_wakeups: words[4],
        idle_wakeups: words[5],
        disk_read_bytes: words[6],
        disk_written_bytes: words[7],
        gpu_ns: words[8],
        energy_nj: words[9],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::Name;
    use crate::ring::Shape;

    /// A shape with room for `readings` and nothing more, in `slot_count` slots.
    fn shape_for(readings: &Readings, slot_count: u64) -> Shape {
        let at_least_one = |count: usize| u64_from_usize(count.max(1));
        let (cores, states) = match &readings.ioreport {
            Some(ioreport) => {
                let mut states = ioreport.gpu.active_ns().len();
                for core in ioreport.cores {
                    states = states.max(core.active_ns().len());
                }
                (ioreport.cores.len(), states)
            }
            None => (0, 0),
        };
        Shape {
            slot_count,
            cpu_capacity: at_least_one(readings.cpus.len()),
            interface_capacity: at_least_one(readings.interfaces.len()),
            core_capacity: at_least_one(cores),
            state_capacity: at_least_one(states),
            fan_capacity: at_least_one(readings.smc.as_ref().map_or(0, |smc| smc.fan_rpm.len())),
            process_capacity: at_least_one(readings.processes.len()),
            coalition_capacity: at_least_one(readings.coalitions.len()),
            event_capacity: at_least_one(readings.events.len()),
        }
    }

    /// A process whose every field holds its own value, so a field read from the wrong word
    /// shows.
    fn process(pid: i32) -> Process {
        let base = u64::from(pid.cast_unsigned()) * 100;
        Process {
            pid,
            usage: Usage {
                awake_ns_at_start: base + 1,
                user_ns: base + 2,
                system_ns: base + 3,
                energy_nj: base + 4,
                instructions: base + 5,
                super_instructions: base + 6,
                cycles: base + 7,
                idle_wakeups: base + 8,
                interrupt_wakeups: base + 9,
                pageins: base + 10,
                resident_bytes: base + 11,
                footprint_bytes: base + 12,
                disk_read_bytes: base + 13,
                disk_written_bytes: base + 14,
            },
            task: Task {
                threads: -1,
                faults: 2,
                context_switches: i32::MAX,
                mach_syscalls: i32::MIN,
                unix_syscalls: 5,
            },
        }
    }

    fn coalition(id: u64) -> Coalition {
        Coalition {
            id,
            tasks_started: id + 1,
            tasks_exited: id + 2,
            cpu_ns: id + 3,
            interrupt_wakeups: id + 4,
            idle_wakeups: id + 5,
            disk_read_bytes: id + 6,
            disk_written_bytes: id + 7,
            gpu_ns: id + 8,
            energy_nj: id + 9,
        }
    }

    fn interface(index: u16, name: &str) -> Interface {
        let mut bytes = [0; IFNAMSIZ];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Interface {
            index,
            name: Name::from_bytes(bytes, name.len()).unwrap(),
            received_bytes: 1,
            sent_bytes: 2,
            received_packets: 3,
            sent_packets: 4,
            receive_errors: 5,
            send_errors: 6,
            receive_drops: 7,
        }
    }

    fn memory() -> Memory {
        Memory {
            total_bytes: 1,
            free_bytes: 2,
            active_bytes: 3,
            inactive_bytes: 4,
            speculative_bytes: 5,
            wired_bytes: 6,
            compressed_bytes: 7,
            anonymous_bytes: 8,
            file_backed_bytes: 9,
            purgeable_bytes: 10,
            swap_total_bytes: 11,
            swap_used_bytes: 12,
            swapped_in_bytes: 13,
            swapped_out_bytes: 14,
            pageins: 15,
            pageouts: 16,
            compressions: 17,
            decompressions: 18,
            pressure: Pressure::Warning,
        }
    }

    fn slot_of(layout: &Layout) -> Vec<AtomicU64> {
        (0..layout.slot_words).map(|_| AtomicU64::new(0)).collect()
    }

    /// The readings of a tick that read nothing, with `memory`.
    fn empty_readings(memory: &Memory) -> Readings<'_> {
        Readings {
            run_tick: 1,
            cpus: &[],
            memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        }
    }

    /// Stores `readings` in a slot with room for them, and loads it into a tick.
    fn store_and_load(readings: &Readings) -> Tick {
        let layout = Layout::new(shape_for(readings, 1)).unwrap();
        let slot = slot_of(&layout);
        let mut tick = Tick::new(&layout);

        store_tick(&slot, &layout, readings, 1..1);

        assert!(load_tick(&slot, &layout, &mut tick));
        tick
    }

    #[test]
    fn a_ticks_machine_readings_load_back_as_they_were_stored() {
        // Goal: every reading of the whole Mac crosses the slot unchanged.
        // Method: store a tick with two CPUs, two interfaces, IOReport and the SMC, load it,
        // and compare each reading.
        let cpus = [
            CPUTicks {
                user: 1,
                system: 2,
                nice: 3,
                idle: u32::MAX,
            },
            CPUTicks {
                user: 5,
                system: 6,
                nice: 7,
                idle: 8,
            },
        ];
        let interfaces = [interface(1, "lo0"), interface(9, "utun1234567890ab")];
        let gpu = States::with_active_ns(&[11, 12, 13]);
        let cores = [
            States::with_active_ns(&[21, 22]),
            States::with_active_ns(&[31]),
        ];
        let memory = memory();
        let readings = Readings {
            run_tick: 77,
            cpus: &cpus,
            interfaces: &interfaces,
            ioreport: Some(IOReportReadings {
                gpu_energy_nj: 99,
                gpu: &gpu,
                cores: &cores,
            }),
            smc: Some(SmcReadings {
                power_watts: 12.5,
                soc_celsius: 48.25,
                fan_rpm: &[1200.0],
            }),
            ..empty_readings(&memory)
        };

        let tick = store_and_load(&readings);

        assert_eq!(77, tick.run_tick);
        assert_eq!(&cpus[..], &tick.cpus[..]);
        assert_eq!(memory, tick.memory);
        assert_eq!(&interfaces[..], &tick.interfaces[..]);
        assert!(tick.ioreport_read);
        assert_eq!(99, tick.gpu_energy_nj);
        assert_eq!(vec![11, 12, 13], tick.gpu_active_ns);
        assert_eq!(2, tick.core_count());
        assert_eq!(&[21, 22], tick.core_active_ns(0));
        assert_eq!(&[31], tick.core_active_ns(1));
        assert!(tick.smc_read);
        assert_eq!(12.5_f32.to_bits(), tick.power_watts.to_bits());
        assert_eq!(48.25_f32.to_bits(), tick.soc_celsius.to_bits());
        assert_eq!(vec![1200.0], tick.fan_rpm);
    }

    #[test]
    fn a_ticks_processes_and_coalitions_load_back_as_they_were_stored() {
        // Goal: every process and coalition record crosses the slot unchanged, with the
        // processes the sweep could not read.
        // Method: store a tick with two processes, one of a negative pid, and two coalitions,
        // load it, and compare each record.
        let processes = [process(1), process(-2)];
        let coalitions = [coalition(4), coalition(5)];
        let memory = Memory::default();
        let readings = Readings {
            processes: &processes,
            processes_refused: 3,
            processes_gone: 4,
            coalitions: &coalitions,
            ..empty_readings(&memory)
        };

        let tick = store_and_load(&readings);

        assert_eq!(&processes[..], &tick.processes[..]);
        assert_eq!(3, tick.processes_refused);
        assert_eq!(4, tick.processes_gone);
        assert_eq!(&coalitions[..], &tick.coalitions[..]);
    }

    #[test]
    fn a_tick_without_ioreport_or_an_smc_loads_back_without_them() {
        // Goal: a tick of a Mac without IOReport or an SMC reads back as one, never with
        // readings left from an earlier tick.
        // Method: load a tick with both sources into a tick that holds both, from a slot whose
        // tick read neither.
        let memory = Memory::default();
        let gpu = States::with_active_ns(&[1]);
        let cores = [States::with_active_ns(&[2])];
        let with_sources = Readings {
            ioreport: Some(IOReportReadings {
                gpu_energy_nj: 5,
                gpu: &gpu,
                cores: &cores,
            }),
            smc: Some(SmcReadings {
                power_watts: 1.0,
                soc_celsius: 2.0,
                fan_rpm: &[3.0],
            }),
            ..empty_readings(&memory)
        };
        let without_sources = Readings {
            run_tick: 2,
            ioreport: None,
            smc: None,
            ..with_sources
        };
        let layout = Layout::new(shape_for(&with_sources, 1)).unwrap();
        let mut tick = Tick::new(&layout);
        let first = slot_of(&layout);
        store_tick(&first, &layout, &with_sources, 1..1);
        assert!(load_tick(&first, &layout, &mut tick));
        let second = slot_of(&layout);

        store_tick(&second, &layout, &without_sources, 1..1);
        let loaded = load_tick(&second, &layout, &mut tick);

        assert!(loaded);
        assert!(!tick.ioreport_read);
        assert_eq!(0, tick.gpu_energy_nj);
        assert!(tick.gpu_active_ns.is_empty());
        assert_eq!(0, tick.core_count());
        assert!(!tick.smc_read);
        assert_eq!(0.0_f32.to_bits(), tick.power_watts.to_bits());
        assert_eq!(0.0_f32.to_bits(), tick.soc_celsius.to_bits());
        assert!(tick.fan_rpm.is_empty());
    }

    #[test]
    fn a_slot_with_words_that_make_no_tick_does_not_load() {
        // Goal: words a torn or broken slot holds never become readings.
        // Method: load slots holding, one at a time, a count past its section's room, a
        // pressure past the three macOS has, and an interface name longer than its bytes.
        let interfaces = [interface(1, "en0")];
        let memory = Memory::default();
        let readings = Readings {
            interfaces: &interfaces,
            ..empty_readings(&memory)
        };
        let layout = Layout::new(shape_for(&readings, 1)).unwrap();
        let name_word = layout.interfaces.record(0).start;
        let breaks = [
            (SLOT_PROCESS_COUNT, 2),
            (SLOT_MEMORY + MEMORY_PRESSURE, 3),
            (name_word, 1 | (17 << INTERFACE_NAME_LENGTH_SHIFT)),
        ];

        for (index, word) in breaks {
            let slot = slot_of(&layout);
            store_tick(&slot, &layout, &readings, 1..1);
            slot[index].store(word, Ordering::Relaxed);

            assert!(
                !load_tick(&slot, &layout, &mut Tick::new(&layout)),
                "word {index}"
            );
        }
    }
}
