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
pub(super) const LAYOUT_VERSION: u64 = 2;

/// The words of the header, which starts the ring.
pub(super) const HEADER_MAGIC: usize = 0;
pub(super) const HEADER_LAYOUT_VERSION: usize = 1;
pub(super) const HEADER_GENERATION: usize = 2;
/// The ring's first publication, one past the newest of the ring before it.
pub(super) const HEADER_FIRST_PUBLICATION: usize = 3;
/// The newest publication the writer finished, which before its first is the previous ring's.
pub(super) const HEADER_NEWEST_PUBLICATION: usize = 4;
/// When the writer's run started, in nanoseconds since 1970 by the wall clock, which tells
/// runs apart where a restart of the Mac numbers publications from 1 again.
pub(super) const HEADER_RUN_START_NS: usize = 5;
/// The ring's first event, one past the newest of the ring before it.
pub(super) const HEADER_FIRST_EVENT: usize = 6;
/// The newest event the writer finished, which before its first is the previous ring's.
pub(super) const HEADER_NEWEST_EVENT: usize = 7;
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
pub(super) const HEADER_WORDS: usize = 32;
const _: () = assert!(HEADER_EVENT_WORDS < HEADER_WORDS);

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
pub(super) const SLOT_FIXED_WORDS: usize = SLOT_MEMORY + MEMORY_WORDS;

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
                return None;
            }

            if field > SHAPE_FIELD_MAX {
                return None;
            }
        }

        let mut plan = SlotPlan {
            end: SLOT_FIXED_WORDS,
        };
        let cpus = plan.section(shape.cpu_capacity, CPU_WORDS);
        let interfaces = plan.section(shape.interface_capacity, INTERFACE_WORDS);
        let state_capacity = usize_from(shape.state_capacity);
        let gpu_states = plan.section(shape.state_capacity, 1);
        let cores = plan.section(shape.core_capacity, 1 + state_capacity);
        let fans = plan.section(shape.fan_capacity, 1);
        let processes = plan.section(shape.process_capacity, PROCESS_WORDS);
        let coalitions = plan.section(shape.coalition_capacity, COALITION_WORDS);
        let slot_words = plan.end;
        // A core's record grows with the states, so the size may pass what an object holds.
        let size_bytes = u64_from_usize(slot_words)
            .checked_mul(shape.slot_count)?
            .checked_add(u64_from_usize(HEADER_WORDS))?
            .checked_add(u64_from_usize(EVENT_WORDS).checked_mul(shape.event_capacity)?)?
            .checked_mul(u64_from_usize(size_of::<u64>()))?;
        if i64::try_from(size_bytes).is_err() {
            return None;
        }

        Some(Self {
            shape,
            slot_words,
            cpus,
            interfaces,
            gpu_states,
            cores,
            fans,
            processes,
            coalitions,
            events_start: HEADER_WORDS + usize_from(shape.slot_count) * slot_words,
        })
    }

    pub(super) fn word_count(&self) -> usize {
        self.events_start + usize_from(self.shape.event_capacity) * EVENT_WORDS
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
        let start = self.events_start + index * EVENT_WORDS;
        start..start + EVENT_WORDS
    }

    /// The words of the slot that holds `publication`, from the ring's start.
    pub(super) fn slot(&self, publication: u64) -> Range<usize> {
        assert!(publication > 0);

        let index = usize_from((publication - 1) % self.shape.slot_count);
        let start = HEADER_WORDS + index * self.slot_words;
        start..start + self.slot_words
    }
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
fn usize_from(value: u64) -> usize {
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
        (HEADER_LAYOUT_VERSION, LAYOUT_VERSION),
        (HEADER_GENERATION, header.generation),
        (HEADER_FIRST_PUBLICATION, header.first_publication),
        (HEADER_NEWEST_PUBLICATION, header.first_publication - 1),
        (HEADER_RUN_START_NS, header.run_start_ns),
        (HEADER_FIRST_EVENT, header.first_event),
        (HEADER_NEWEST_EVENT, header.first_event - 1),
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
        words[index].store(u64_from_usize(width), Ordering::Relaxed);
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
    pub(super) first_publication: u64,
    pub(super) run_start_ns: u64,
    pub(super) first_event: u64,
}

/// The layout the header `header` describes, or an error where it is no ring of this layout
/// or the object, of `size_bytes`, is too short for it.
pub(super) fn open_layout(header: &[AtomicU64], size_bytes: usize) -> io::Result<Layout> {
    let load = |index: usize| header[index].load(Ordering::Relaxed);
    if load(HEADER_MAGIC) != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a ring"));
    }

    if load(HEADER_LAYOUT_VERSION) != LAYOUT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a ring of layout {}", load(HEADER_LAYOUT_VERSION)),
        ));
    }

    for (index, width) in RECORD_WIDTHS {
        if load(index) != u64_from_usize(width) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "a ring whose header word {index} holds a width of {}",
                    load(index)
                ),
            ));
        }
    }

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
    let Some(layout) = Layout::new(shape) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a ring of shape {shape:?}"),
        ));
    };

    if layout.size_bytes() > size_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a ring of {size_bytes} bytes, short of its layout"),
        ));
    }

    Ok(layout)
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
            first_publication: 1,
            run_start_ns: 1,
            first_event: 1,
        };
        store_header(&words, &layout(), &header);
        words
    }

    #[test]
    fn opens_the_layout_a_header_of_this_code_describes() {
        // Goal: a ring this code wrote opens with the layout it was written with.
        // Method: open a header of a known layout in an object of its size.
        let opened = open_layout(&header(), layout().size_bytes()).unwrap();

        assert_eq!(layout().shape, opened.shape);
    }

    #[test]
    fn refuses_a_header_of_another_program_or_layout() {
        // Goal: a reader never reads an object as a ring where its header is another
        // program's, another layout's, or a shape no ring has.
        // Method: open headers that hold, one at a time, another magic, the next layout
        // version, a slot count of zero, and a process record one word wider.
        let breaks = [
            (HEADER_MAGIC, MAGIC + 1),
            (HEADER_LAYOUT_VERSION, LAYOUT_VERSION + 1),
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
