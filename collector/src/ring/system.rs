//! Fixed system records follow the exact complete disks extent. Each slot's record holds the
//! tick's system readings and the machine's facts, the clock of each core's each active
//! state, and the GPU time of each process the tick's identities name, in their order.

use std::io::ErrorKind;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::clock_states::ClockTables;
use crate::gpu_memory::GpuMemoryReading;
use crate::system::{FactText, MachineFacts, PerformanceLevel, SystemReading};
use crate::u64_from_usize;

pub(super) const MAGIC: u64 = u64::from_be_bytes(*b"FDSYTL01");
pub(super) const HEADER_WORDS: usize = 7;
const VERSION: u64 = 1;

/// What one tick publishes beside its readings, in the order its identities name processes.
pub struct SystemPublication<'a> {
    pub reading: &'a SystemReading,
    pub machine: &'a MachineFacts,
    /// The clock tables each core's state count selects from, or none where unreadable.
    pub clock: Option<&'a ClockTables>,
    /// The GPU time of each process the tick's identities name, in the same order.
    pub process_gpu_ns: &'a [Option<u64>],
}

/// What a reader loads from a slot's system record.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SystemSnapshot {
    pub reading: SystemReading,
    pub machine: MachineFacts,
}

// The words of a record's fixed part, from its start.
const PRESENCE: usize = 0;
const LOAD_AVERAGE: usize = 1;
const OPEN_FILES: usize = 4;
const OPEN_FILES_LIMIT: usize = 5;
const THERMAL_PRESSURE: usize = 6;
const GPU_IN_USE_BYTES: usize = 7;
const GPU_ALLOCATED_BYTES: usize = 8;
const BOOT_WALL_NS: usize = 9;
const CPU_CORES: usize = 10;
const GPU_CORES: usize = 11;
const MEMORY_BYTES: usize = 12;
const LEVEL_LOGICAL_CPUS: usize = 13;
const PROCESS_COUNT: usize = 15;
const TEXTS: usize = 16;
const TEXT_COUNT: usize = 5;
/// A text's length word and its bytes, eight to a word.
const TEXT_WORDS: usize = 1 + crate::system::FACT_BYTES_MAX / 8;
const FIXED_WORDS: usize = TEXTS + TEXT_COUNT * TEXT_WORDS;
const _: () = assert!(crate::system::FACT_BYTES_MAX.is_multiple_of(8));
const _: () = assert!(FIXED_WORDS == 61);

// The bits of the presence word. A record with no bit set holds no publication.
const RECORDED: u64 = 1 << 0;
const HAS_LOAD_AVERAGE: u64 = 1 << 1;
const HAS_OPEN_FILES: u64 = 1 << 2;
const HAS_OPEN_FILES_LIMIT: u64 = 1 << 3;
const HAS_THERMAL_PRESSURE: u64 = 1 << 4;
const HAS_GPU_IN_USE_BYTES: u64 = 1 << 5;
const HAS_GPU_ALLOCATED_BYTES: u64 = 1 << 6;
const HAS_BOOT_WALL_NS: u64 = 1 << 7;
const HAS_CPU_CORES: u64 = 1 << 8;
const HAS_GPU_CORES: u64 = 1 << 9;
const HAS_MEMORY_BYTES: u64 = 1 << 10;
const HAS_LEVELS: u64 = 1 << 11;
const HAS_MODEL: u64 = 1 << 12;
const HAS_CHIP: u64 = 1 << 13;
const HAS_ARCHITECTURE: u64 = 1 << 14;
const PRESENCE_ALL: u64 = (1 << 15) - 1;

/// The texts' places among the record's texts.
const MODEL: usize = 0;
const CHIP: usize = 1;
const ARCHITECTURE: usize = 2;
const LEVEL_NAMES: usize = 3;

/// A process's GPU time word holds this bit beside its nanoseconds, so 0 reads as none.
const GPU_PRESENT: u64 = 1 << 63;
/// The most GPU time a record carries for a process, in nanoseconds, 292 years of it.
pub const GPU_NS_MAX: u64 = GPU_PRESENT - 1;

#[derive(Debug, Clone)]
pub(super) struct SystemLayout {
    start: usize,
    slot_count: usize,
    pub(super) core_capacity: usize,
    pub(super) state_capacity: usize,
    pub(super) process_capacity: usize,
    record_words: usize,
    end: usize,
}

/// The capacities of the base layout that size each record.
pub(super) struct SystemCapacities {
    pub(super) slot_count: u64,
    pub(super) cores: usize,
    pub(super) states: usize,
    pub(super) processes: usize,
}

pub(super) struct SystemExtents {
    pub(super) start_words: usize,
    pub(super) size_bytes: usize,
}

impl SystemLayout {
    pub(super) fn plan(start: usize, capacities: &SystemCapacities) -> Option<Self> {
        if capacities.slot_count == 0 {
            return None;
        }

        let slot_count = usize::try_from(capacities.slot_count).ok()?;
        let record_words = FIXED_WORDS
            .checked_add(capacities.cores.checked_mul(capacities.states)?)?
            .checked_add(capacities.processes)?;
        let end = start
            .checked_add(HEADER_WORDS)?
            .checked_add(slot_count.checked_mul(record_words)?)?;
        i64::try_from(end.checked_mul(size_of::<u64>())?).ok()?;

        Some(Self {
            start,
            slot_count,
            core_capacity: capacities.cores,
            state_capacity: capacities.states,
            process_capacity: capacities.processes,
            record_words,
            end,
        })
    }

    pub(super) fn word_count(&self) -> usize {
        self.end
    }

    pub(super) fn record(&self, publication: u64) -> Range<usize> {
        assert!(publication > 0);

        let index = (publication - 1) % u64_from_usize(self.slot_count);
        let index = match usize::try_from(index) {
            Ok(index) => index,
            Err(error) => unreachable!("{error}"),
        };
        let start = self.start + HEADER_WORDS + index * self.record_words;

        start..start + self.record_words
    }

    fn header(&self) -> [u64; HEADER_WORDS] {
        [
            MAGIC,
            VERSION,
            u64_from_usize(self.slot_count),
            u64_from_usize(self.record_words),
            u64_from_usize(self.core_capacity),
            u64_from_usize(self.state_capacity),
            u64_from_usize(self.process_capacity),
        ]
    }

    pub(super) fn store_header(&self, words: &[AtomicU64]) {
        for (index, value) in self.header().iter().enumerate() {
            words[self.start + index].store(*value, Ordering::Relaxed);
        }
    }

    pub(super) fn admit(
        header: &[AtomicU64],
        capacities: &SystemCapacities,
        extents: &SystemExtents,
    ) -> Option<Self> {
        if header.len() < HEADER_WORDS {
            return None;
        }

        let layout = Self::plan(extents.start_words, capacities)?;
        for (word, expected) in header.iter().zip(layout.header()) {
            if word.load(Ordering::Relaxed) != expected {
                return None;
            }
        }
        if layout.end.checked_mul(size_of::<u64>())? > extents.size_bytes {
            return None;
        }

        Some(layout)
    }

    fn clocks(&self) -> Range<usize> {
        FIXED_WORDS..FIXED_WORDS + self.core_capacity * self.state_capacity
    }

    fn processes(&self) -> Range<usize> {
        let start = self.clocks().end;
        start..start + self.process_capacity
    }
}

/// Stores `publication`'s record, or a record of none, for a tick whose cores report the
/// state counts `core_states` gives.
pub(super) fn store(
    words: &[AtomicU64],
    layout: &SystemLayout,
    publication: Option<&SystemPublication>,
    core_state_counts: impl Iterator<Item = usize>,
) {
    assert_eq!(words.len(), layout.record_words);

    let mut fixed = [0; FIXED_WORDS];
    if let Some(publication) = publication {
        assert!(publication.process_gpu_ns.len() <= layout.process_capacity);
        store_fixed(&mut fixed, publication);
    }
    for (word, value) in words[..FIXED_WORDS].iter().zip(fixed) {
        word.store(value, Ordering::Relaxed);
    }

    let clocks = &words[layout.clocks()];
    for word in clocks {
        word.store(0, Ordering::Relaxed);
    }
    if let Some(clock) = publication.and_then(|publication| publication.clock) {
        store_clocks(clocks, layout.state_capacity, clock, core_state_counts);
    }

    let processes = &words[layout.processes()];
    let gpu_ns = publication.map_or(&[][..], |publication| publication.process_gpu_ns);
    for (index, word) in processes.iter().enumerate() {
        let value = match gpu_ns.get(index).copied().flatten() {
            Some(gpu_ns) => gpu_word(gpu_ns),
            None => 0,
        };
        word.store(value, Ordering::Relaxed);
    }
}

fn store_fixed(fixed: &mut [u64; FIXED_WORDS], publication: &SystemPublication) {
    let reading = publication.reading;
    let machine = publication.machine;
    let mut presence = RECORDED;
    let mut put = |bit: u64, index: usize, value: Option<u64>| {
        if let Some(value) = value {
            presence |= bit;
            fixed[index] = value;
        }
    };
    put(HAS_OPEN_FILES, OPEN_FILES, reading.open_files);
    put(
        HAS_OPEN_FILES_LIMIT,
        OPEN_FILES_LIMIT,
        reading.open_files_limit,
    );
    put(
        HAS_THERMAL_PRESSURE,
        THERMAL_PRESSURE,
        reading.thermal_pressure.map(u64::from),
    );
    let gpu_memory = reading.gpu_memory.unwrap_or_default();
    put(
        HAS_GPU_IN_USE_BYTES,
        GPU_IN_USE_BYTES,
        gpu_memory.in_use_bytes,
    );
    put(
        HAS_GPU_ALLOCATED_BYTES,
        GPU_ALLOCATED_BYTES,
        gpu_memory.allocated_bytes,
    );
    put(HAS_BOOT_WALL_NS, BOOT_WALL_NS, reading.boot_wall_ns);
    put(HAS_CPU_CORES, CPU_CORES, machine.cpu_cores.map(u64::from));
    put(HAS_GPU_CORES, GPU_CORES, machine.gpu_cores.map(u64::from));
    put(HAS_MEMORY_BYTES, MEMORY_BYTES, machine.memory_bytes);

    if let Some(load_average) = reading.load_average {
        presence |= HAS_LOAD_AVERAGE;
        for (index, average) in load_average.iter().enumerate() {
            fixed[LOAD_AVERAGE + index] = average.to_bits();
        }
    }
    if let Some(levels) = machine.levels {
        presence |= HAS_LEVELS;
        for (index, level) in levels.iter().enumerate() {
            fixed[LEVEL_LOGICAL_CPUS + index] = u64::from(level.logical_cpus);
            store_text(fixed, LEVEL_NAMES + index, &level.name);
        }
    }
    for (bit, index, text) in [
        (HAS_MODEL, MODEL, machine.model),
        (HAS_CHIP, CHIP, machine.chip),
        (HAS_ARCHITECTURE, ARCHITECTURE, machine.architecture),
    ] {
        if let Some(text) = text {
            presence |= bit;
            store_text(fixed, index, &text);
        }
    }
    fixed[PROCESS_COUNT] = u64_from_usize(publication.process_gpu_ns.len());
    fixed[PRESENCE] = presence;
}

fn store_text(fixed: &mut [u64; FIXED_WORDS], text: usize, value: &FactText) {
    assert!(text < TEXT_COUNT);

    let start = TEXTS + text * TEXT_WORDS;
    let bytes = value.as_bytes();
    fixed[start] = u64_from_usize(bytes.len());
    for (index, chunk) in bytes.chunks(8).enumerate() {
        let mut word = [0; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        fixed[start + 1 + index] = u64::from_le_bytes(word);
    }
}

fn store_clocks(
    clocks: &[AtomicU64],
    state_capacity: usize,
    clock: &ClockTables,
    core_state_counts: impl Iterator<Item = usize>,
) {
    for (core, count) in core_state_counts.enumerate() {
        assert!(count <= state_capacity);
        let Some(table) = clock.for_states(count) else {
            continue;
        };
        assert_eq!(table.len(), count);

        let start = core * state_capacity;
        for (word, khz) in clocks[start..start + count].iter().zip(table) {
            word.store(u64::from(*khz), Ordering::Relaxed);
        }
    }
}

fn gpu_word(gpu_ns: u64) -> u64 {
    // 2^63 nanoseconds is 292 years of GPU time, which no client counts.
    assert!(gpu_ns < GPU_PRESENT);
    gpu_ns | GPU_PRESENT
}

/// Where a load puts what a record holds.
pub(super) struct SystemLoad<'a> {
    pub(super) snapshot: &'a mut Option<SystemSnapshot>,
    /// Each core's state clocks in kHz, `stride` words apart, 0 where unknown.
    pub(super) core_state_khz: &'a mut [u32],
    pub(super) stride: usize,
    pub(super) process_gpu_ns: &'a mut Vec<Option<u64>>,
}

/// Loads a record, refusing one whose words no writer of this format stores.
pub(super) fn load(
    words: &[AtomicU64],
    layout: &SystemLayout,
    target: &mut SystemLoad,
) -> Result<(), ErrorKind> {
    assert_eq!(words.len(), layout.record_words);
    assert!(target.stride >= layout.state_capacity);
    assert!(target.process_gpu_ns.capacity() >= layout.process_capacity);

    *target.snapshot = None;
    target.core_state_khz.fill(0);
    target.process_gpu_ns.clear();
    let fixed: [u64; FIXED_WORDS] =
        std::array::from_fn(|index| words[index].load(Ordering::Relaxed));
    if fixed[PRESENCE] == 0 {
        return Ok(());
    }
    if fixed[PRESENCE] & !PRESENCE_ALL != 0 {
        return Err(ErrorKind::InvalidData);
    }
    if fixed[PRESENCE] & RECORDED == 0 {
        return Err(ErrorKind::InvalidData);
    }

    *target.snapshot = Some(load_fixed(&fixed)?);
    load_clocks(&words[layout.clocks()], layout, target)?;
    load_processes(&words[layout.processes()], fixed[PROCESS_COUNT], target)
}

fn load_fixed(fixed: &[u64; FIXED_WORDS]) -> Result<SystemSnapshot, ErrorKind> {
    let presence = fixed[PRESENCE];
    let has = |bit: u64| presence & bit != 0;
    let word = |bit: u64, index: usize| has(bit).then_some(fixed[index]);
    let narrow = |bit: u64, index: usize| -> Result<Option<u32>, ErrorKind> {
        word(bit, index)
            .map(|value| u32::try_from(value).map_err(|_| ErrorKind::InvalidData))
            .transpose()
    };

    let load_average = if has(HAS_LOAD_AVERAGE) {
        let averages = std::array::from_fn(|index| f64::from_bits(fixed[LOAD_AVERAGE + index]));
        for average in averages {
            if !average.is_finite() {
                return Err(ErrorKind::InvalidData);
            }
        }
        Some(averages)
    } else {
        None
    };
    let thermal_pressure = word(HAS_THERMAL_PRESSURE, THERMAL_PRESSURE)
        .map(|value| u8::try_from(value).map_err(|_| ErrorKind::InvalidData))
        .transpose()?;
    let in_use_bytes = word(HAS_GPU_IN_USE_BYTES, GPU_IN_USE_BYTES);
    let allocated_bytes = word(HAS_GPU_ALLOCATED_BYTES, GPU_ALLOCATED_BYTES);
    let gpu_memory = match (in_use_bytes, allocated_bytes) {
        (None, None) => None,
        _ => Some(GpuMemoryReading {
            in_use_bytes,
            allocated_bytes,
        }),
    };

    let reading = SystemReading {
        load_average,
        open_files: word(HAS_OPEN_FILES, OPEN_FILES),
        open_files_limit: word(HAS_OPEN_FILES_LIMIT, OPEN_FILES_LIMIT),
        thermal_pressure,
        gpu_memory,
        boot_wall_ns: word(HAS_BOOT_WALL_NS, BOOT_WALL_NS),
    };
    let machine = MachineFacts {
        model: load_text_if(has(HAS_MODEL), fixed, MODEL)?,
        chip: load_text_if(has(HAS_CHIP), fixed, CHIP)?,
        architecture: load_text_if(has(HAS_ARCHITECTURE), fixed, ARCHITECTURE)?,
        cpu_cores: narrow(HAS_CPU_CORES, CPU_CORES)?,
        gpu_cores: narrow(HAS_GPU_CORES, GPU_CORES)?,
        memory_bytes: word(HAS_MEMORY_BYTES, MEMORY_BYTES),
        levels: load_levels(has(HAS_LEVELS), fixed)?,
    };

    Ok(SystemSnapshot { reading, machine })
}

fn load_levels(
    present: bool,
    fixed: &[u64; FIXED_WORDS],
) -> Result<Option<[PerformanceLevel; 2]>, ErrorKind> {
    if !present {
        return Ok(None);
    }

    let mut levels = [PerformanceLevel::default(); 2];
    for (index, level) in levels.iter_mut().enumerate() {
        let Some(name) = load_text_if(true, fixed, LEVEL_NAMES + index)? else {
            unreachable!("a present text loads as some");
        };
        level.name = name;
        level.logical_cpus =
            u32::try_from(fixed[LEVEL_LOGICAL_CPUS + index]).map_err(|_| ErrorKind::InvalidData)?;
    }
    Ok(Some(levels))
}

fn load_text_if(
    present: bool,
    fixed: &[u64; FIXED_WORDS],
    text: usize,
) -> Result<Option<FactText>, ErrorKind> {
    assert!(text < TEXT_COUNT);
    if !present {
        return Ok(None);
    }

    let start = TEXTS + text * TEXT_WORDS;
    let count = usize::try_from(fixed[start]).map_err(|_| ErrorKind::InvalidData)?;
    if count > crate::system::FACT_BYTES_MAX {
        return Err(ErrorKind::InvalidData);
    }
    let mut bytes = [0_u8; crate::system::FACT_BYTES_MAX];
    for (index, chunk) in bytes.chunks_mut(8).enumerate() {
        chunk.copy_from_slice(&fixed[start + 1 + index].to_le_bytes());
    }
    if bytes[count..].iter().any(|byte| *byte != 0) {
        return Err(ErrorKind::InvalidData);
    }

    FactText::new(&bytes[..count])
        .map(Some)
        .ok_or(ErrorKind::InvalidData)
}

fn load_clocks(
    clocks: &[AtomicU64],
    layout: &SystemLayout,
    target: &mut SystemLoad,
) -> Result<(), ErrorKind> {
    for core in 0..layout.core_capacity {
        let source = &clocks[core * layout.state_capacity..][..layout.state_capacity];
        let start = core * target.stride;
        let Some(destination) = target
            .core_state_khz
            .get_mut(start..start + layout.state_capacity)
        else {
            return Err(ErrorKind::InvalidData);
        };
        for (khz, word) in destination.iter_mut().zip(source) {
            *khz =
                u32::try_from(word.load(Ordering::Relaxed)).map_err(|_| ErrorKind::InvalidData)?;
        }
    }
    Ok(())
}

fn load_processes(
    processes: &[AtomicU64],
    count: u64,
    target: &mut SystemLoad,
) -> Result<(), ErrorKind> {
    let count = usize::try_from(count).map_err(|_| ErrorKind::InvalidData)?;
    if count > processes.len() {
        return Err(ErrorKind::InvalidData);
    }

    for (index, word) in processes.iter().enumerate() {
        let value = word.load(Ordering::Relaxed);
        if index >= count {
            if value != 0 {
                return Err(ErrorKind::InvalidData);
            }
            continue;
        }
        let gpu_ns = if value & GPU_PRESENT != 0 {
            Some(value & !GPU_PRESENT)
        } else {
            if value != 0 {
                return Err(ErrorKind::InvalidData);
            }
            None
        };
        target.process_gpu_ns.push(gpu_ns);
    }
    Ok(())
}
