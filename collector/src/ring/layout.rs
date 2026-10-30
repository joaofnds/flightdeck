//! Where each part of a ring lies, in words from its start. Every part is a run of 64-bit
//! words, which the writer stores and readers load as atomics, since they share them.

use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use super::Shape;
use crate::u64_from_usize;

/// Marks the start of a ring, so a reader never takes another object for one.
pub(super) const MAGIC: u64 = u64::from_be_bytes(*b"FLTDRING");
/// The layout this code writes and reads, which a change to the layout raises.
pub(super) const LAYOUT_VERSION_LEGACY: u64 = 2;
pub(super) const LAYOUT_VERSION_OBSERVED: u64 = 3;
pub(super) const LAYOUT_VERSION: u64 = LAYOUT_VERSION_OBSERVED;

/// The words of the header, which starts the ring.
pub(super) const HEADER_MAGIC: usize = 0;
pub(super) const HEADER_LAYOUT_VERSION: usize = 1;
pub(super) const HEADER_GENERATION: usize = 2;
/// The ring's first publication, one past the newest of the ring before it.
pub(super) const HEADER_PUBLICATION_FIRST: usize = 3;
/// The newest publication the writer finished, which before its first is the previous ring's.
pub(super) const HEADER_PUBLICATION_NEWEST: usize = 4;
/// When the writer's run started, in nanoseconds since 1970 by the wall clock, which tells
/// runs apart where a restart of the Mac numbers publications from 1 again.
pub(super) const HEADER_RUN_START_NS: usize = 5;
/// The ring's first event, one past the newest of the ring before it.
pub(super) const HEADER_EVENT_FIRST: usize = 6;
/// The newest event the writer finished, which before its first is the previous ring's.
pub(super) const HEADER_EVENT_NEWEST: usize = 7;
/// The shape, a word for each of its fields.
pub(super) const HEADER_SLOT_COUNT: usize = 8;
pub(super) const HEADER_CPU_CAPACITY: usize = 9;
pub(super) const HEADER_INTERFACE_CAPACITY: usize = 10;
pub(super) const HEADER_CORE_CAPACITY: usize = 11;
pub(super) const HEADER_STATE_CAPACITY: usize = 12;
pub(super) const HEADER_FAN_CAPACITY: usize = 13;
pub(super) const HEADER_PROCESS_CAPACITY: usize = 14;
pub(super) const HEADER_COALITION_CAPACITY: usize = 15;
pub(super) const HEADER_EVENT_CAPACITY: usize = 16;
/// The words of each kind of record, a word for each, so a reader built with records of
/// another width refuses the ring even where the layout version was not raised.
pub(super) const HEADER_SLOT_FIXED_WORDS: usize = 17;
pub(super) const HEADER_CPU_WORDS: usize = 18;
pub(super) const HEADER_INTERFACE_WORDS: usize = 19;
pub(super) const HEADER_PROCESS_WORDS: usize = 20;
pub(super) const HEADER_COALITION_WORDS: usize = 21;
pub(super) const HEADER_EVENT_WORDS: usize = 22;
pub(super) const HEADER_IDENTITY: usize = 23;
pub(super) const IDENTITY_WORDS: usize = 2;
pub(super) const HEADER_COMPLETED: usize = HEADER_IDENTITY + 3 * IDENTITY_WORDS;
pub(super) const HEADER_RUN_START_CONTINUOUS_NS: usize = HEADER_COMPLETED + 1;
pub(super) const HEADER_ORIGIN_STATE: usize = HEADER_RUN_START_CONTINUOUS_NS + 1;
pub(super) const HEADER_WORDS: usize = HEADER_ORIGIN_STATE + 1;
pub(super) const LAYOUT_VERSION_IDENTITY: u64 = 4;
pub(super) const HEADER_WORDS_IDENTITY: usize = 40;
pub(super) const HEADER_DICTIONARY_CAPACITY: usize = 32;
pub(super) const HEADER_ARENA_CAPACITY: usize = 33;
pub(super) const HEADER_DICTIONARY_COUNT: usize = 34;
pub(super) const HEADER_ARENA_USED: usize = 35;
pub(super) const HEADER_PROCESS_IDENTITY_WORDS: usize = 36;
pub(super) const HEADER_DESCRIPTOR_WORDS: usize = 37;
pub(super) const SLOT_IDENTITY_COUNT: usize = 42;
pub(super) const PROCESS_IDENTITY_WORDS: usize = 6;
pub(super) const DESCRIPTOR_WORDS: usize = 4;
pub(super) const EVENT_WORDS_IDENTITY: usize = EVENT_WORDS + 5;
const _: () = assert!(HEADER_WORDS_IDENTITY == 40);
const _: () = assert!(SLOT_IDENTITY_COUNT == SLOT_FIXED_WORDS);
const _: () = assert!(PROCESS_IDENTITY_WORDS == 6);
const _: () = assert!(EVENT_WORDS_IDENTITY == 24);
const _: () = assert!(DESCRIPTOR_WORDS == 4);
pub(super) const ORIGIN_UNAVAILABLE: u64 = 0;
pub(super) const ORIGIN_IDENTIFIED: u64 = 1;
/// The source has an exact elapsed origin but no assigned recording/run/boot identity.
pub(super) const ORIGIN_UNREGISTERED: u64 = 2;
const _: () = assert!(HEADER_EVENT_WORDS < HEADER_WORDS);
const _: () = assert!(HEADER_IDENTITY == HEADER_EVENT_WORDS + 1);
const _: () = assert!(HEADER_WORDS == 32);

/// Each header word that holds a record's width, with the width this code reads.
const RECORD_WIDTHS: [(usize, usize); 6] = [
    (HEADER_SLOT_FIXED_WORDS, SLOT_FIXED_WORDS),
    (HEADER_CPU_WORDS, CPU_WORDS),
    (HEADER_INTERFACE_WORDS, INTERFACE_WORDS),
    (HEADER_PROCESS_WORDS, PROCESS_WORDS),
    (HEADER_COALITION_WORDS, COALITION_WORDS),
    (HEADER_EVENT_WORDS, EVENT_WORDS),
];

/// The words that start each slot. The sequence word holds twice the publication the slot
/// holds once the writer finished it, and one less while the writer writes it.
pub(super) const SLOT_SEQUENCE: usize = 0;
pub(super) const SLOT_RUN_TICK: usize = 1;
pub(super) const SLOT_PROCESSES_REFUSED: usize = 2;
pub(super) const SLOT_PROCESSES_GONE: usize = 3;
/// The records each section holds, a word for each section.
pub(super) const SLOT_CPU_COUNT: usize = 4;
pub(super) const SLOT_INTERFACE_COUNT: usize = 5;
pub(super) const SLOT_GPU_STATE_COUNT: usize = 6;
pub(super) const SLOT_CORE_COUNT: usize = 7;
pub(super) const SLOT_FAN_COUNT: usize = 8;
pub(super) const SLOT_PROCESS_COUNT: usize = 9;
pub(super) const SLOT_COALITION_COUNT: usize = 10;
/// Which sources the tick read, a bit for each, since a Mac may lack `IOReport` or the SMC.
pub(super) const SLOT_SOURCES: usize = 11;
pub(super) const SLOT_GPU_ENERGY_NJ: usize = 12;
pub(super) const SLOT_POWER_WATTS: usize = 13;
pub(super) const SLOT_SOC_CELSIUS: usize = 14;
/// The tick's events, from the first up to the end, which the event log holds.
pub(super) const SLOT_EVENTS_FIRST: usize = 15;
pub(super) const SLOT_EVENTS_END: usize = 16;
/// The events of each kind the tracker dropped in the tick, which never reach the log.
pub(super) const SLOT_DROPPED_STARTS: usize = 17;
pub(super) const SLOT_DROPPED_EXECS: usize = 18;
pub(super) const SLOT_DROPPED_EXITS: usize = 19;
pub(super) const SLOT_MEMORY: usize = 20;
pub(super) const MEMORY_WORDS: usize = 19;
pub(super) const SLOT_WALL_NS: usize = SLOT_MEMORY + MEMORY_WORDS;
pub(super) const SLOT_CONTINUOUS_NS: usize = SLOT_WALL_NS + 1;
pub(super) const SLOT_AWAKE_NS: usize = SLOT_CONTINUOUS_NS + 1;
pub(super) const SLOT_FIXED_WORDS: usize = SLOT_AWAKE_NS + 1;
pub(super) const SOURCE_OBSERVATION_TIMES: u64 = 4;

pub(super) const SOURCE_IOREPORT: u64 = 1;
pub(super) const SOURCE_SMC: u64 = 2;

/// The words of one record in each section. A core's record is its count of states and then
/// a word for each state the shape has room for.
pub(super) const CPU_WORDS: usize = 2;
pub(super) const INTERFACE_WORDS: usize = 10;
pub(super) const PROCESS_WORDS: usize = 18;
pub(super) const COALITION_WORDS: usize = 10;

/// The words of an event's record in the log. The sequence word holds twice the event's
/// number once the writer finished it, and one less while the writer writes it.
pub(super) const EVENT_SEQUENCE: usize = 0;
pub(super) const EVENT_WORDS: usize = 19;

/// The most a shape's slot count and each capacity may be, which keeps a reader's sums of
/// them far from overflow whatever another program wrote in the header.
const SHAPE_FIELD_MAX: u64 = 1 << 24;

/// Where each part of a ring lies.
#[derive(Debug, Clone)]
pub(super) struct Layout {
    pub(super) battery: Option<super::battery::BatteryLayout>,
    pub(super) disks: Option<super::disks::DisksLayout>,
    pub(super) system: Option<super::system::SystemLayout>,
    pub(super) version: u64,
    pub(super) shape: Shape,
    pub(super) slot_words: usize,
    pub(super) cpus: Section,
    pub(super) interfaces: Section,
    pub(super) gpu_states: Section,
    pub(super) cores: Section,
    pub(super) fans: Section,
    pub(super) processes: Section,
    pub(super) coalitions: Section,
    /// The event log's first word, from the ring's start.
    pub(super) events_start: usize,
    pub(super) event_words: usize,
    pub(super) process_identities: Section,
    pub(super) dictionary: Section,
    pub(super) arena_start: usize,
    pub(super) arena_bytes: usize,
}

/// A run of records of one size within a slot.
#[derive(Debug, Clone)]
pub(super) struct Section {
    /// The section's first word, from the slot's start.
    pub(super) start: usize,
    pub(super) capacity: usize,
    pub(super) record_words: usize,
}

impl Section {
    /// The words of the record at `index`, from the slot's start.
    pub(super) fn record(&self, index: usize) -> Range<usize> {
        assert!(index < self.capacity);

        let start = self.start + index * self.record_words;
        start..start + self.record_words
    }

    fn end(&self) -> usize {
        self.start + self.capacity * self.record_words
    }
}

impl Layout {
    /// The layout of `shape`, or `None` where a field of it is 0 or past `SHAPE_FIELD_MAX`.
    pub(super) fn new(shape: Shape) -> Option<Self> {
        Self::versioned(shape, LAYOUT_VERSION)
    }

    pub(super) fn versioned(shape: Shape, version: u64) -> Option<Self> {
        if version == LAYOUT_VERSION_IDENTITY {
            return Self::identity(shape, super::DictionaryCapacity::default());
        }

        Self::planned(
            shape,
            version,
            super::DictionaryCapacity {
                entries: 0,
                arena_bytes: 0,
            },
        )
    }

    pub(super) fn identity(shape: Shape, capacity: super::DictionaryCapacity) -> Option<Self> {
        if capacity.entries == 0 {
            return None;
        }

        if capacity.entries > 65_536 {
            return None;
        }

        if capacity.arena_bytes == 0 {
            return None;
        }

        if capacity.arena_bytes > 64 * 1024 * 1024 {
            return None;
        }

        if !capacity.arena_bytes.is_multiple_of(8) {
            return None;
        }

        Self::planned(shape, LAYOUT_VERSION_IDENTITY, capacity)
    }

    pub(super) fn identity_battery(
        shape: Shape,
        capacity: super::DictionaryCapacity,
    ) -> Option<Self> {
        let mut layout = Self::identity(shape, capacity)?;
        layout.battery = Some(super::battery::BatteryLayout::plan(
            layout.base_word_count(),
            shape.slot_count,
        )?);
        Some(layout)
    }

    pub(super) fn identity_disks(
        shape: Shape,
        capacity: super::DictionaryCapacity,
    ) -> Option<Self> {
        let mut layout = Self::identity_battery(shape, capacity)?;
        layout.disks = Some(super::disks::DisksLayout::plan(
            layout.word_count(),
            shape.slot_count,
        )?);

        Some(layout)
    }

    pub(super) fn identity_system(
        shape: Shape,
        capacity: super::DictionaryCapacity,
    ) -> Option<Self> {
        let mut layout = Self::identity_disks(shape, capacity)?;
        layout.system = Some(super::system::SystemLayout::plan(
            layout.word_count(),
            &layout.system_capacities(),
        )?);

        Some(layout)
    }

    /// The capacities of this base layout that size a system record.
    pub(super) fn system_capacities(&self) -> super::system::SystemCapacities {
        super::system::SystemCapacities {
            slot_count: self.shape.slot_count,
            cores: self.cores.capacity,
            states: self.gpu_states.capacity,
            processes: self.process_identities.capacity,
        }
    }

    fn planned(shape: Shape, version: u64, capacity: super::DictionaryCapacity) -> Option<Self> {
        if !(LAYOUT_VERSION_LEGACY..=LAYOUT_VERSION_IDENTITY).contains(&version) {
            return None;
        }

        if !planned_shape(shape) {
            return None;
        }

        let mut plan = SlotPlan {
            end: if version == LAYOUT_VERSION_LEGACY {
                SLOT_WALL_NS
            } else {
                SLOT_FIXED_WORDS + usize::from(version == LAYOUT_VERSION_IDENTITY)
            },
        };
        let cpus = plan.section(shape.cpu_capacity, CPU_WORDS);
        let interfaces = plan.section(shape.interface_capacity, INTERFACE_WORDS);
        let state_capacity = usize_from(shape.state_capacity);
        let gpu_states = plan.section(shape.state_capacity, 1);
        let cores = plan.section(shape.core_capacity, 1 + state_capacity);
        let fans = plan.section(shape.fan_capacity, 1);
        let processes = plan.section(shape.process_capacity, PROCESS_WORDS);
        let coalitions = plan.section(shape.coalition_capacity, COALITION_WORDS);
        let process_identities = plan.section(
            if version == LAYOUT_VERSION_IDENTITY {
                shape.process_capacity
            } else {
                0
            },
            PROCESS_IDENTITY_WORDS,
        );
        let mut widths = PlannedWords {
            slot: plan.end,
            header: HEADER_WORDS,
            event: EVENT_WORDS,
        };
        planned_widths(version, &mut widths);
        planned_size(shape, capacity, &widths)?;

        let events_start = widths.header + usize_from(shape.slot_count) * widths.slot;
        let dictionary = Section {
            start: events_start + usize_from(shape.event_capacity) * widths.event,
            capacity: usize_from(capacity.entries),
            record_words: DESCRIPTOR_WORDS,
        };

        Some(Self {
            battery: None,
            disks: None,
            system: None,
            version,
            shape,
            slot_words: widths.slot,
            cpus,
            interfaces,
            gpu_states,
            cores,
            fans,
            processes,
            coalitions,
            events_start,
            event_words: widths.event,
            process_identities,
            arena_start: dictionary.end(),
            dictionary,
            arena_bytes: usize::try_from(capacity.arena_bytes).ok()?,
        })
    }

    pub(super) fn word_count(&self) -> usize {
        if let Some(system) = &self.system {
            return system.word_count();
        }

        if let Some(disks) = &self.disks {
            return disks.word_count();
        }

        match &self.battery {
            Some(battery) => battery.word_count(),
            None => self.base_word_count(),
        }
    }

    pub(super) fn base_word_count(&self) -> usize {
        self.arena_start + self.arena_bytes / 8
    }

    pub(super) fn size_bytes(&self) -> usize {
        self.word_count() * size_of::<u64>()
    }

    /// The publications the slots hold once `newest` is published, where every publication
    /// numbered from 1 went into them.
    pub(super) fn publications_held(&self, newest: u64) -> Held {
        Held {
            oldest: newest.saturating_sub(self.shape.slot_count) + 1,
            newest,
        }
    }

    /// The events the log holds once `newest` is written, where every event numbered from 1
    /// went into it.
    pub(super) fn events_held(&self, newest: u64) -> Held {
        Held {
            oldest: newest.saturating_sub(self.shape.event_capacity) + 1,
            newest,
        }
    }

    /// The words of the record that holds the event numbered `number`, from the ring's start.
    pub(super) fn event(&self, number: u64) -> Range<usize> {
        assert!(number > 0);

        let index = usize_from((number - 1) % self.shape.event_capacity);
        let start = self.events_start + index * self.event_words;
        start..start + self.event_words
    }

    /// The words of the slot that holds `publication`, from the ring's start.
    pub(super) fn slot(&self, publication: u64) -> Range<usize> {
        let index = self.slot_index(publication);
        let header_words = if self.version == LAYOUT_VERSION_IDENTITY {
            HEADER_WORDS_IDENTITY
        } else {
            HEADER_WORDS
        };
        let start = header_words + index * self.slot_words;
        start..start + self.slot_words
    }

    /// The index of the slot that holds `publication`, counting slots from 0.
    pub(super) fn slot_index(&self, publication: u64) -> usize {
        assert!(publication > 0);

        usize_from((publication - 1) % self.shape.slot_count)
    }
}

struct PlannedWords {
    slot: usize,
    header: usize,
    event: usize,
}

fn planned_widths(version: u64, widths: &mut PlannedWords) {
    if version == LAYOUT_VERSION_IDENTITY {
        widths.header = HEADER_WORDS_IDENTITY;
        widths.event = EVENT_WORDS_IDENTITY;
    }
}

fn planned_size(
    shape: Shape,
    capacity: super::DictionaryCapacity,
    widths: &PlannedWords,
) -> Option<()> {
    // Core capacity times state capacity times slot count can exceed the object bound.
    let size_bytes = u64_from_usize(widths.slot)
        .checked_mul(shape.slot_count)?
        .checked_add(u64_from_usize(widths.header))?
        .checked_add(u64_from_usize(widths.event).checked_mul(shape.event_capacity)?)?
        .checked_add(
            capacity
                .entries
                .checked_mul(u64_from_usize(DESCRIPTOR_WORDS))?,
        )?
        .checked_add(capacity.arena_bytes / 8)?
        .checked_mul(u64_from_usize(size_of::<u64>()))?;

    i64::try_from(size_bytes).ok()?;

    Some(())
}

fn planned_shape(shape: Shape) -> bool {
    let fields = [
        shape.slot_count,
        shape.cpu_capacity,
        shape.interface_capacity,
        shape.core_capacity,
        shape.state_capacity,
        shape.fan_capacity,
        shape.process_capacity,
        shape.coalition_capacity,
        shape.event_capacity,
    ];
    for field in fields {
        if field == 0 {
            return false;
        }

        if field > SHAPE_FIELD_MAX {
            return false;
        }
    }

    true
}

/// The words of a slot laid out so far, which each section it lays out extends.
struct SlotPlan {
    end: usize,
}

impl SlotPlan {
    /// Lays out a section of `capacity` records of `record_words` each after the last.
    fn section(&mut self, capacity: u64, record_words: usize) -> Section {
        assert!(capacity <= SHAPE_FIELD_MAX);
        assert!(record_words > 0);

        let section = Section {
            start: self.end,
            capacity: usize_from(capacity),
            record_words,
        };
        self.end = section.end();
        section
    }
}

/// `value`, which every shape field and index here holds within `SHAPE_FIELD_MAX`.
pub(super) fn usize_from(value: u64) -> usize {
    assert!(value <= SHAPE_FIELD_MAX);

    match usize::try_from(value) {
        Ok(value) => value,
        Err(error) => unreachable!("{error}"),
    }
}

/// Writes the header of a ring of `layout`.
pub(super) fn store_header(words: &[AtomicU64], layout: &Layout, header: &Header) {
    let shape = layout.shape;
    let fields = [
        (HEADER_MAGIC, MAGIC),
        (HEADER_LAYOUT_VERSION, layout.version),
        (HEADER_GENERATION, header.generation),
        (HEADER_PUBLICATION_FIRST, header.publication_first),
        (HEADER_PUBLICATION_NEWEST, header.publication_first - 1),
        (HEADER_RUN_START_NS, header.run_start_ns),
        (HEADER_EVENT_FIRST, header.event_first),
        (HEADER_EVENT_NEWEST, header.event_first - 1),
        (HEADER_SLOT_COUNT, shape.slot_count),
        (HEADER_CPU_CAPACITY, shape.cpu_capacity),
        (HEADER_INTERFACE_CAPACITY, shape.interface_capacity),
        (HEADER_CORE_CAPACITY, shape.core_capacity),
        (HEADER_STATE_CAPACITY, shape.state_capacity),
        (HEADER_FAN_CAPACITY, shape.fan_capacity),
        (HEADER_PROCESS_CAPACITY, shape.process_capacity),
        (HEADER_COALITION_CAPACITY, shape.coalition_capacity),
        (HEADER_EVENT_CAPACITY, shape.event_capacity),
    ];
    for (index, value) in fields {
        words[index].store(value, Ordering::Relaxed);
    }
    for (index, width) in RECORD_WIDTHS {
        let width = match index {
            HEADER_SLOT_FIXED_WORDS if layout.version == LAYOUT_VERSION_IDENTITY => 43,
            HEADER_EVENT_WORDS => layout.event_words,
            _ => width,
        };
        words[index].store(u64_from_usize(width), Ordering::Relaxed);
    }
    if layout.version == LAYOUT_VERSION_IDENTITY {
        let fields = [
            (HEADER_DICTIONARY_CAPACITY, layout.dictionary.capacity),
            (HEADER_ARENA_CAPACITY, layout.arena_bytes),
            (HEADER_PROCESS_IDENTITY_WORDS, PROCESS_IDENTITY_WORDS),
            (HEADER_DESCRIPTOR_WORDS, DESCRIPTOR_WORDS),
        ];
        for (index, value) in fields {
            words[index].store(u64_from_usize(value), Ordering::Relaxed);
        }
    }
}

/// The numbers a ring holds, from the oldest it has not overwritten to the newest written.
#[derive(Debug, Clone, Copy)]
pub(super) struct Held {
    pub(super) oldest: u64,
    pub(super) newest: u64,
}

/// What a ring's header holds besides its shape.
#[derive(Debug)]
pub(super) struct Header {
    pub(super) generation: u64,
    pub(super) publication_first: u64,
    pub(super) run_start_ns: u64,
    pub(super) event_first: u64,
}

/// The layout the header `header` describes, or an error where it is no ring of this layout
/// or the object, of `size_bytes`, is too short for it.
pub(super) fn open_layout(header: &[AtomicU64], size_bytes: usize) -> io::Result<Layout> {
    if header.len() < HEADER_WORDS {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let load = |index: usize| header[index].load(Ordering::Relaxed);
    if load(HEADER_MAGIC) != MAGIC {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }

    let version = load(HEADER_LAYOUT_VERSION);
    if version != LAYOUT_VERSION_IDENTITY {
        if version != LAYOUT_VERSION {
            if version != LAYOUT_VERSION_LEGACY {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
        }
    }
    open_layout_widths(header, version)?;

    let shape = Shape {
        slot_count: load(HEADER_SLOT_COUNT),
        cpu_capacity: load(HEADER_CPU_CAPACITY),
        interface_capacity: load(HEADER_INTERFACE_CAPACITY),
        core_capacity: load(HEADER_CORE_CAPACITY),
        state_capacity: load(HEADER_STATE_CAPACITY),
        fan_capacity: load(HEADER_FAN_CAPACITY),
        process_capacity: load(HEADER_PROCESS_CAPACITY),
        coalition_capacity: load(HEADER_COALITION_CAPACITY),
        event_capacity: load(HEADER_EVENT_CAPACITY),
    };
    let planned = if version == LAYOUT_VERSION_IDENTITY {
        Layout::identity(
            shape,
            super::DictionaryCapacity {
                entries: load(HEADER_DICTIONARY_CAPACITY),
                arena_bytes: load(HEADER_ARENA_CAPACITY),
            },
        )
    } else {
        Layout::versioned(shape, version)
    };
    let Some(layout) = planned else {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    };

    if layout.size_bytes() > size_bytes {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }

    Ok(layout)
}

fn open_layout_widths(header: &[AtomicU64], version: u64) -> io::Result<()> {
    let load = |index: usize| header[index].load(Ordering::Relaxed);
    if version == LAYOUT_VERSION_IDENTITY {
        if header.len() < HEADER_WORDS_IDENTITY {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let count = header[HEADER_DICTIONARY_COUNT].load(Ordering::Acquire);
        if count > load(HEADER_DICTIONARY_CAPACITY) {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let used = load(HEADER_ARENA_USED);
        if used > load(HEADER_ARENA_CAPACITY) {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        if !used.is_multiple_of(8) {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let exact = [
            (HEADER_PROCESS_IDENTITY_WORDS, 6),
            (HEADER_DESCRIPTOR_WORDS, 4),
            (38, 0),
            (39, 0),
        ];
        for (index, value) in exact {
            if load(index) != value {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
        }
    }
    for (index, width) in RECORD_WIDTHS {
        let mut width = width;
        if version == LAYOUT_VERSION_IDENTITY {
            match index {
                HEADER_SLOT_FIXED_WORDS => width = 43,
                HEADER_EVENT_WORDS => width = EVENT_WORDS_IDENTITY,
                _ => {}
            }
        }
        if index == HEADER_SLOT_FIXED_WORDS {
            if version == LAYOUT_VERSION_LEGACY {
                width = SLOT_WALL_NS;
            }
        }
        if load(index) != u64_from_usize(width) {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layout of one slot with room for one record of each kind.
    fn layout() -> Layout {
        Layout::new(Shape {
            slot_count: 1,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        })
        .unwrap()
    }

    /// The header of a ring of `layout()`.
    fn header() -> Vec<AtomicU64> {
        let words: Vec<AtomicU64> = (0..HEADER_WORDS).map(|_| AtomicU64::new(0)).collect();
        let header = Header {
            generation: 1,
            publication_first: 1,
            run_start_ns: 1,
            event_first: 1,
        };
        store_header(&words, &layout(), &header);
        words
    }

    fn identity_header() -> (Layout, Vec<AtomicU64>) {
        let layout = Layout::identity(
            layout().shape,
            super::super::DictionaryCapacity {
                entries: 2,
                arena_bytes: 32,
            },
        )
        .unwrap();
        let words = (0..HEADER_WORDS_IDENTITY)
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>();
        store_header(
            &words,
            &layout,
            &Header {
                generation: 1,
                publication_first: 1,
                run_start_ns: 1,
                event_first: 1,
            },
        );
        (layout, words)
    }

    #[test]
    fn refuses_identity_counts_past_declared_dictionary_storage() {
        // Goal: corrupt dictionary accounting is refused before exposing the mapping.
        // Method: commit more entries than the fixed dictionary can contain.
        let (layout, words) = identity_header();
        words[HEADER_DICTIONARY_COUNT].store(3, Ordering::Relaxed);

        let result = open_layout(&words, layout.size_bytes());

        assert_eq!(io::ErrorKind::InvalidData, result.unwrap_err().kind());
    }

    struct HeaderCorruption {
        index: usize,
        value: u64,
    }

    fn refuses_corrupt_identity_header(corruption: &HeaderCorruption) {
        let (layout, words) = identity_header();
        words[corruption.index].store(corruption.value, Ordering::Relaxed);

        let result = open_layout(&words, layout.size_bytes());

        assert_eq!(io::ErrorKind::InvalidData, result.unwrap_err().kind());
    }

    macro_rules! corrupt_identity_headers {
        ($($name:ident, $index:expr, $value:expr;)*) => {$(
            #[test]
            fn $name() {
                // Goal: v4 refuses a wrong width, capacity or reserved header word.
                // Method: corrupt one discriminator in an otherwise valid header.
                refuses_corrupt_identity_header(&HeaderCorruption { index: $index, value: $value });
            }
        )*};
    }

    corrupt_identity_headers! {
        refuses_wrong_identity_slot_width, HEADER_SLOT_FIXED_WORDS, 42;
        refuses_wrong_identity_event_width, HEADER_EVENT_WORDS, 19;
        refuses_wrong_identity_process_width, HEADER_PROCESS_WORDS, 19;
        refuses_wrong_process_identity_width, HEADER_PROCESS_IDENTITY_WORDS, 7;
        refuses_wrong_descriptor_width, HEADER_DESCRIPTOR_WORDS, 5;
        refuses_empty_dictionary_capacity, HEADER_DICTIONARY_CAPACITY, 0;
        refuses_oversized_dictionary_capacity, HEADER_DICTIONARY_CAPACITY, 65_537;
        refuses_empty_arena_capacity, HEADER_ARENA_CAPACITY, 0;
        refuses_unaligned_arena_capacity, HEADER_ARENA_CAPACITY, 33;
        refuses_used_arena_past_capacity, HEADER_ARENA_USED, 40;
        refuses_unaligned_used_arena, HEADER_ARENA_USED, 1;
        refuses_first_identity_reserved_word, 38, 1;
        refuses_second_identity_reserved_word, 39, 1;
    }

    #[test]
    fn refuses_identity_header_and_arena_truncation() {
        // Goal: v4 cannot inherit legacy header size or map a truncated arena.
        // Method: open its valid header with a short header and a one-word-short object.
        let (layout, words) = identity_header();

        let short_header = open_layout(&words[..HEADER_WORDS], layout.size_bytes());
        let short_object = open_layout(&words, layout.size_bytes() - 8);

        assert_eq!(io::ErrorKind::InvalidData, short_header.unwrap_err().kind());
        assert_eq!(io::ErrorKind::InvalidData, short_object.unwrap_err().kind());
    }

    #[test]
    fn reserves_exact_identity_bytes_for_the_documented_full_profile() {
        // Goal: the documented full ring remains below the existing one-GiB admission bound.
        // Method: compare exact v3/v4 arithmetic without allocating either full ring.
        let shape = Shape {
            slot_count: 120,
            cpu_capacity: 18,
            interface_capacity: 1638,
            core_capacity: 18,
            state_capacity: 64,
            fan_capacity: 10,
            process_capacity: 12000,
            coalition_capacity: 28384,
            event_capacity: 1_966_080,
        };

        let legacy = Layout::new(shape).unwrap();
        let identity =
            Layout::identity(shape, super::super::DictionaryCapacity::default()).unwrap();

        assert_eq!(795_684_736, legacy.size_bytes());
        assert_eq!(1_012_654_976, identity.size_bytes());
        println!(
            "ring={} index={} observation={} reference={} reader_identity_scratch={}",
            identity.size_bytes(),
            65_536 * 2 * size_of::<u64>(),
            size_of::<super::super::IdentityObservation>(),
            size_of::<super::super::ProgramReference>(),
            12_000 * size_of::<super::super::IdentityObservation>()
        );

        assert!(identity.size_bytes() < 1024 * 1024 * 1024);
    }

    #[test]
    fn plans_identity_layout_without_changing_default_writes() {
        // Goal: identity fixtures use v4 while the production constructor stays v3.
        // Method: compare explicit identity planning with the existing constructor.
        let identity = Layout::identity(
            layout().shape,
            super::super::DictionaryCapacity {
                entries: 2,
                arena_bytes: 32,
            },
        )
        .unwrap();

        assert_eq!(4, identity.version);
        assert_eq!(3, layout().version);
        assert_eq!(43, identity.cpus.start);
        assert_eq!(24, identity.event_words);
    }

    #[test]
    fn opens_the_layout_a_header_of_this_code_describes() {
        // Goal: a ring this code wrote opens with the layout it was written with.
        // Method: open a header of a known layout in an object of its size.
        let opened = open_layout(&header(), layout().size_bytes()).unwrap();

        assert_eq!(layout().shape, opened.shape);
    }

    #[test]
    fn opens_layout_two_with_its_original_section_offsets() {
        // Goal: old headers retain their original narrower slots and data offsets.
        // Method: open a legacy header and compare its section offsets to the old widths.
        let words = header();
        words[HEADER_LAYOUT_VERSION].store(LAYOUT_VERSION_LEGACY, Ordering::Relaxed);
        words[HEADER_SLOT_FIXED_WORDS].store(39, Ordering::Relaxed);
        let legacy = Layout::versioned(layout().shape, LAYOUT_VERSION_LEGACY).unwrap();

        let opened = open_layout(&words, legacy.size_bytes()).unwrap();

        assert_eq!(LAYOUT_VERSION_LEGACY, opened.version);
        assert_eq!(39, opened.cpus.start);
        // Header32 + fixed39 + CPU2 + interface10 + GPU1 + core2 + fan1 + process18 + coalition10.
        assert_eq!(115, opened.events_start);
    }

    #[test]
    fn cannot_construct_a_plan_for_an_unknown_layout_version() {
        // Goal: a speculative layout version cannot silently acquire today's offsets.
        // Method: ask the internal planner directly for a version neither reader supports.
        assert!(Layout::versioned(layout().shape, LAYOUT_VERSION_IDENTITY + 1).is_none());
    }

    #[test]
    fn refuses_a_header_of_another_program_or_layout() {
        // Goal: a reader never reads an object as a ring where its header is another
        // program's, another layout's, or a shape no ring has.
        // Method: open headers that hold, one at a time, another magic, the next layout
        // version, a slot count of zero, and a process record one word wider.
        let breaks = [
            (HEADER_MAGIC, MAGIC + 1),
            (HEADER_LAYOUT_VERSION, LAYOUT_VERSION_IDENTITY + 1),
            (HEADER_SLOT_COUNT, 0),
            (HEADER_PROCESS_WORDS, u64_from_usize(PROCESS_WORDS + 1)),
        ];

        for (index, word) in breaks {
            let header = header();
            header[index].store(word, Ordering::Relaxed);

            let opened = open_layout(&header, layout().size_bytes());

            assert_eq!(
                io::ErrorKind::InvalidData,
                opened.unwrap_err().kind(),
                "word {index}"
            );
        }
    }

    #[test]
    fn refuses_an_object_shorter_than_its_header_says() {
        // Goal: a reader never maps a ring past the end of its object.
        // Method: open a header in an object one word short of the layout it describes.
        let short_bytes = layout().size_bytes() - size_of::<u64>();

        let opened = open_layout(&header(), short_bytes);

        assert_eq!(io::ErrorKind::InvalidData, opened.unwrap_err().kind());
    }
}
