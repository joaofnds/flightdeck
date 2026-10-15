//! The shared-memory ring the collector hands each tick to, which other processes of the same
//! user map read-only and read at their own pace. One writer publishes each tick into the
//! next slot and never waits for a reader. A reader copies a slot and checks its sequence word
//! again, as a single-writer seqlock does, so it drops a slot overwritten during the copy
//! instead of reading it torn, and learns from the publication numbers which it missed.
//!
//! Process events go to a log of their own beside the slots, so a tick with many events
//! overwrites no other tick's. Each record of the log is a seqlock of its own, numbered by
//! event, and each slot names the events of its tick.

mod event;
mod layout;
mod object;
mod tick;
mod wake;

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering, fence};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use event::{load_event, store_event};
use layout::{
    EVENT_SEQUENCE, HEADER_FIRST_EVENT, HEADER_FIRST_PUBLICATION, HEADER_NEWEST_EVENT,
    HEADER_NEWEST_PUBLICATION, HEADER_RUN_START_NS, Header, Held, Layout, SLOT_SEQUENCE,
    store_header,
};
use object::{
    LockedName, Mapping, PROT_READ, PROT_WRITE, create_lock, create_object, create_previous,
    create_unlink_older, open_ring, place_for_user,
};
pub use tick::{Dropped, IOReportReadings, Readings, SmcReadings, Tick};

use crate::tracker::Event;
use crate::u64_from_usize;
use tick::{load_tick, store_tick};
use wake::{Waited, wait, wake_all};

/// How long a reader whose wait macOS cannot take sleeps before it looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Where a ring lives: the lock file its writer holds, which names the ring, and the start of
/// the name of each ring the writer makes.
pub struct Place {
    pub lock_path: PathBuf,
    pub name_prefix: String,
}

impl Place {
    /// The place of the ring `name` of this user, whose lock file is in this user's own
    /// temporary directory. Returns an error where `name` makes a ring's name too long.
    pub fn for_user(name: &str) -> io::Result<Self> {
        place_for_user(name)
    }
}

/// The room a ring takes, fixed when its writer makes it: the publications it holds before
/// it overwrites the oldest, and the most records of each kind a tick holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub slot_count: u64,
    pub cpu_capacity: u64,
    pub interface_capacity: u64,
    pub core_capacity: u64,
    /// The most active states the GPU or one core has.
    pub state_capacity: u64,
    pub fan_capacity: u64,
    pub process_capacity: u64,
    pub coalition_capacity: u64,
    /// The events the log holds before it overwrites the oldest.
    pub event_capacity: u64,
}

/// What a reader found at its next publication.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    /// The publication it read, which carries the tick of the collector's run. The reader's
    /// `tick` holds the rest of it.
    Publication { publication: u64, tick: u64 },
    /// The publications from `first` up to `end` that the writer overwrote before the reader
    /// read them, which it reads past.
    Missed { first: u64, end: u64 },
    /// No publication past those it has read yet.
    Nothing,
}

/// What a reader found at its next event.
#[derive(Debug, PartialEq)]
pub enum FoundEvent {
    /// The event numbered `number`, which the tick of the collector's run `tick` drained.
    Event {
        number: u64,
        tick: u64,
        event: Event,
    },
    /// The events from `first` up to `end` that the writer overwrote before the reader read
    /// them, which it reads past.
    Missed { first: u64, end: u64 },
    /// No event past those it has read yet.
    Nothing,
}

/// The writer of a ring, which holds the lock file for as long as it lives.
pub struct Writer {
    _lock: File,
    mapping: Mapping,
    layout: Layout,
    /// The newest publication written, or the previous ring's newest before the first.
    publication: u64,
    /// The newest event written, or the previous ring's newest before the first.
    event: u64,
}

impl Writer {
    /// Takes the lock of `place` and makes a ring of `shape` in it, under the generation past
    /// the ring the lock file names, whose publication numbers it continues. Returns an error
    /// where another writer holds the lock.
    pub fn create(place: &Place, shape: Shape) -> io::Result<Self> {
        let Some(layout) = Layout::new(shape) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a ring of shape {shape:?}"),
            ));
        };
        let run_start_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();

        let lock = create_lock(&place.lock_path)?;
        let previous = create_previous(&place.name_prefix, &lock)?;
        let created = create_object(&place.name_prefix, previous.generation, &layout)?;
        let mapping = Mapping::new(&created.object, layout.word_count(), PROT_READ | PROT_WRITE)?;
        let header = Header {
            generation: created.generation,
            first_publication: previous.newest_publication + 1,
            run_start_ns: u64::try_from(run_start_ns).map_err(io::Error::other)?,
            first_event: previous.newest_event + 1,
        };
        store_header(mapping.words(), &layout, &header);

        create_unlink_older(&place.name_prefix, previous.generation);
        // Writing the name before cutting the file to its length never leaves the lock file
        // empty, as cutting first would for a writer stopped between the two, which would make
        // the next writer number from 1.
        lock.write_all_at(created.name.as_bytes(), 0)?;
        lock.set_len(u64_from_usize(created.name.len()))?;
        Ok(Self {
            _lock: lock,
            mapping,
            layout,
            publication: previous.newest_publication,
            event: previous.newest_event,
        })
    }

    /// Publishes `readings` in the next slot, overwriting the oldest publication once every
    /// slot holds one, and its events in the log, overwriting the oldest events once every
    /// record holds one.
    pub fn publish(&mut self, readings: &Readings) {
        // A tick's events fit in the log, so a reader can read every one of them.
        assert!(u64_from_usize(readings.events.len()) <= self.layout.shape.event_capacity);

        let words = self.mapping.words();
        let events_first = self.event + 1;
        for event in readings.events {
            let number = self.event + 1;
            let record = &words[self.layout.event(number)];
            record[EVENT_SEQUENCE].store(2 * number - 1, Ordering::Relaxed);
            fence(Ordering::Release);
            store_event(record, readings.run_tick, event);
            record[EVENT_SEQUENCE].store(2 * number, Ordering::Release);
            self.event = number;
        }
        words[HEADER_NEWEST_EVENT].store(self.event, Ordering::Release);

        let publication = self.publication + 1;
        let slot = &words[self.layout.slot(publication)];
        slot[SLOT_SEQUENCE].store(2 * publication - 1, Ordering::Relaxed);
        fence(Ordering::Release);
        store_tick(slot, &self.layout, readings, events_first..self.event + 1);
        slot[SLOT_SEQUENCE].store(2 * publication, Ordering::Release);

        words[HEADER_NEWEST_PUBLICATION].store(publication, Ordering::Release);
        self.publication = publication;
        // A refused wake costs the readers it missed one wait at most, so the writer goes on.
        if let Err(errno) = wake_all(&words[HEADER_NEWEST_PUBLICATION]) {
            eprintln!("ring: waking readers failed with errno {errno}");
        }
    }
}

/// A reader of a ring, which maps it read-only.
pub struct Reader {
    /// The name of the ring, which the lock file named when the reader opened it.
    name: LockedName,
    mapping: Mapping,
    layout: Layout,
    /// Where the reader stands in the publications, which start past the previous ring's.
    publications: Cursor,
    /// When the writer's run started, in nanoseconds since 1970 by the wall clock.
    run_start_ns: u64,
    tick: LoadedTick,
    /// Where the reader stands in the event log, which starts past the previous ring's.
    events: Cursor,
}

impl Reader {
    /// Opens the ring the lock file at `lock_path` names, starting at the oldest publication
    /// it holds.
    pub fn open(lock_path: &Path) -> io::Result<Self> {
        let locked = LockedName::read(&File::open(lock_path)?)?;
        let name = locked.as_str();
        if name.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the lock file names no ring yet",
            ));
        }

        let (mapping, layout) = open_ring(name)?;
        let words = mapping.words();
        let first = words[HEADER_FIRST_PUBLICATION].load(Ordering::Relaxed);
        let newest = words[HEADER_NEWEST_PUBLICATION].load(Ordering::Relaxed);
        let run_start_ns = words[HEADER_RUN_START_NS].load(Ordering::Relaxed);
        let first_event = words[HEADER_FIRST_EVENT].load(Ordering::Relaxed);
        let newest_event = words[HEADER_NEWEST_EVENT].load(Ordering::Relaxed);
        Ok(Self {
            name: locked,
            mapping,
            publications: Cursor::new(first, layout.publications_held(newest)),
            run_start_ns,
            tick: LoadedTick::new(&layout),
            events: Cursor::new(first_event, layout.events_held(newest_event)),
            // Last, since the fields above read it.
            layout,
        })
    }

    /// Whether the lock file at `lock_path` names another ring than the reader's, as it does
    /// once the writer restarted, so the reader opens the newer ring after it has read every
    /// publication of its own. A lock file the writer is part-way through writing names no
    /// ring yet, which is no move.
    pub fn moved(&self, lock_path: &Path) -> io::Result<bool> {
        let locked = LockedName::read(&File::open(lock_path)?)?;
        let name = locked.as_str();
        if name.is_empty() {
            return Ok(false);
        }
        Ok(name != self.name.as_str())
    }

    /// Opens the ring the lock file at `lock_path` names in place of the reader's own, as
    /// once `moved` says the writer restarted, and reads on from where the reader stood, so
    /// the next reads return what the newer writer overwrote first as missed.
    pub fn follow(&mut self, lock_path: &Path) -> io::Result<()> {
        let mut newer = Self::open(lock_path)?;
        newer.publications.carry(&self.publications);
        newer.events.carry(&self.events);
        *self = newer;
        Ok(())
    }

    /// The tick of the publication the reader read last.
    #[must_use]
    pub fn tick(&self) -> &Tick {
        &self.tick.last
    }

    /// When the writer's run started, in nanoseconds since 1970 by the wall clock.
    #[must_use]
    pub fn run_start_ns(&self) -> u64 {
        self.run_start_ns
    }

    /// Sleeps until the writer publishes past the publications the reader has read, for at
    /// most `timeout`, and returns at once where it already has. A reader whose wait macOS
    /// cannot take sleeps `POLL_INTERVAL` instead, or `timeout` where that is shorter.
    pub fn wait(&self, timeout: Duration) -> io::Result<()> {
        assert!(!timeout.is_zero());

        let newest_word = &self.mapping.words()[HEADER_NEWEST_PUBLICATION];
        let newest = newest_word.load(Ordering::Relaxed);
        if self.publications.next <= newest {
            return Ok(());
        }
        if wait(newest_word, newest, timeout)? == Waited::Unsupported {
            std::thread::sleep(timeout.min(POLL_INTERVAL));
        }
        Ok(())
    }

    /// Reads the next publication, or the range of those the writer overwrote first.
    pub fn read(&mut self) -> io::Result<Found> {
        let words = self.mapping.words();
        let newest = words[HEADER_NEWEST_PUBLICATION].load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        let publication = match self
            .publications
            .step(self.layout.publications_held(newest))
        {
            Step::Nothing => return Ok(Found::Nothing),
            Step::Missed { first, end } => return Ok(Found::Missed { first, end }),
            Step::Read(publication) => publication,
        };
        let slot = &words[self.layout.slot(publication)];
        if self.tick.read_slot(slot, &self.layout, publication)? {
            Ok(Found::Publication {
                publication,
                tick: self.tick.last.run_tick,
            })
        } else {
            Ok(Found::Missed {
                first: publication,
                end: publication + 1,
            })
        }
    }

    /// Reads the next event, or the range of those the writer overwrote first.
    pub fn read_event(&mut self) -> io::Result<FoundEvent> {
        let words = self.mapping.words();
        let newest = words[HEADER_NEWEST_EVENT].load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        let number = match self.events.step(self.layout.events_held(newest)) {
            Step::Nothing => return Ok(FoundEvent::Nothing),
            Step::Missed { first, end } => return Ok(FoundEvent::Missed { first, end }),
            Step::Read(number) => number,
        };
        let record = &words[self.layout.event(number)];
        match read_finished(record, EVENT_SEQUENCE, number, || load_event(record))? {
            Some((tick, event)) => Ok(FoundEvent::Event {
                number,
                tick,
                event,
            }),
            None => Ok(FoundEvent::Missed {
                first: number,
                end: number + 1,
            }),
        }
    }
}

/// Where a reader stands in numbers the writer hands out from 1 and overwrites oldest first,
/// as its publications and its events.
struct Cursor {
    /// The first number of the ring, past the previous ring's.
    first: u64,
    /// The number the reader reads next.
    next: u64,
}

/// What a reader does next with a cursor's numbers.
enum Step {
    /// Waits, since the writer has written no number past those read.
    Nothing,
    /// Reads past the numbers from `first` up to `end`, which the writer overwrote.
    Missed { first: u64, end: u64 },
    /// Reads the number it holds.
    Read(u64),
}

impl Cursor {
    /// A cursor at the oldest number the ring holds, or at `first` where the ring has not
    /// written that far.
    fn new(first: u64, held: Held) -> Self {
        Self {
            first,
            next: first.max(held.oldest),
        }
    }

    /// Takes the position of `older`, a cursor over the previous ring, where the numbers
    /// this ring continues start at or past it. A ring whose numbers start before it begins
    /// a numbering of its own, as after a reboot, and the cursor stays where it opened.
    fn carry(&mut self, older: &Self) {
        if older.next <= self.first {
            self.next = older.next;
        }
    }

    /// The next step once the writer holds the numbers `held`, moving the cursor past what
    /// the step covers.
    fn step(&mut self, held: Held) -> Step {
        if self.next > held.newest {
            return Step::Nothing;
        }
        let oldest = self.first.max(held.oldest);
        if self.next < oldest {
            let first = self.next;
            self.next = oldest;
            return Step::Missed { first, end: oldest };
        }
        let number = self.next;
        self.next += 1;
        Step::Read(number)
    }
}

/// The tick a reader read last, and the room it loads the next tick into, so a load the
/// writer overwrites leaves the last tick whole rather than part of a later one.
struct LoadedTick {
    last: Tick,
    next: Tick,
}

impl LoadedTick {
    fn new(layout: &Layout) -> Self {
        Self {
            last: Tick::new(layout),
            next: Tick::new(layout),
        }
    }

    /// Loads the tick the slot `slot` holds for `publication` in place of the last, and
    /// returns whether it did, which it did not where the writer overwrote the slot with a
    /// later publication before or during the load.
    fn read_slot(
        &mut self,
        slot: &[AtomicU64],
        layout: &Layout,
        publication: u64,
    ) -> io::Result<bool> {
        let loaded = read_finished(slot, SLOT_SEQUENCE, publication, || {
            load_tick(slot, layout, &mut self.next).then_some(())
        })?;
        if loaded.is_some() {
            std::mem::swap(&mut self.last, &mut self.next);
        }
        Ok(loaded.is_some())
    }
}

/// What `load` makes of the words `record` holds for the number `number`, under the sequence
/// word at `sequence_index`, or `None` where the writer overwrote the record with a later
/// number before or during the load. The header said the writer finished `number`, so a
/// record behind it, or one that holds it and makes nothing, is a broken ring.
fn read_finished<T>(
    record: &[AtomicU64],
    sequence_index: usize,
    number: u64,
    load: impl FnOnce() -> Option<T>,
) -> io::Result<Option<T>> {
    assert!(number > 0);

    let sequence = record[sequence_index].load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    let loaded = load();
    fence(Ordering::Acquire);
    let sequence_after = record[sequence_index].load(Ordering::Relaxed);

    if sequence < 2 * number {
        // A record only moves on to later numbers.
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the record of {number} holds sequence {sequence}"),
        ));
    }
    if sequence == 2 * number {
        if sequence_after == sequence {
            if loaded.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the record of {number} holds words that make nothing"),
                ));
            }
            return Ok(loaded);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Memory;

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

    /// A slot of `layout()` that holds a tick of `run_tick` under the sequence `sequence`.
    fn slot_holding(sequence: u64, run_tick: u64) -> Vec<AtomicU64> {
        let layout = layout();
        let slot: Vec<AtomicU64> = (0..layout.slot_words).map(|_| AtomicU64::new(0)).collect();
        let readings = Readings {
            run_tick,
            cpus: &[],
            memory: &Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        };
        store_tick(&slot, &layout, &readings, 1..1);
        slot[SLOT_SEQUENCE].store(sequence, Ordering::Relaxed);
        slot
    }

    fn read(slot: &[AtomicU64], publication: u64) -> io::Result<Option<u64>> {
        let mut tick = LoadedTick::new(&layout());
        let read = tick.read_slot(slot, &layout(), publication)?;
        Ok(read.then_some(tick.last.run_tick))
    }

    #[test]
    fn a_dropped_slot_leaves_the_last_tick_read() {
        // Goal: after a slot the writer overwrote, the reader still holds the tick it read
        // last, never words of the later publication.
        // Method: read publication 3 from a finished slot of tick 40, then publication 4 from
        // a slot the writer is rewriting with tick 99, and look at the tick held.
        let finished = slot_holding(6, 40);
        let rewritten = slot_holding(13, 99);
        let mut tick = LoadedTick::new(&layout());
        assert!(tick.read_slot(&finished, &layout(), 3).unwrap());

        let read = tick.read_slot(&rewritten, &layout(), 4).unwrap();

        assert!(!read);
        assert_eq!(40, tick.last.run_tick);
    }

    #[test]
    fn reads_the_tick_of_a_slot_its_writer_finished() {
        // Goal: a finished slot gives its tick.
        // Method: read publication 3 from a slot whose sequence is twice 3.
        let slot = slot_holding(6, 40);

        assert_eq!(Some(40), read(&slot, 3).unwrap());
    }

    #[test]
    fn drops_a_slot_its_writer_is_writing_a_later_publication_into() {
        // Goal: a reader never reads a slot the writer is part-way through.
        // Method: read publication 3 from a slot of four whose writer started publication 7,
        // which leaves its sequence at twice 7 less 1.
        let slot = slot_holding(13, 40);

        assert_eq!(None, read(&slot, 3).unwrap());
    }

    #[test]
    fn drops_a_slot_its_writer_finished_a_later_publication_in() {
        // Goal: a slot overwritten before the reader came to it is missed, never read as the
        // publication the reader asked for.
        // Method: read publication 3 from a slot that holds publication 7.
        let slot = slot_holding(14, 40);

        assert_eq!(None, read(&slot, 3).unwrap());
    }

    #[test]
    fn refuses_a_slot_behind_the_publication_the_header_finished() {
        // Goal: a slot behind a publication the header calls finished is a broken ring, which
        // the reader reports rather than reading.
        // Method: read publication 3 from a slot that holds publication 2.
        let slot = slot_holding(4, 40);

        assert_eq!(
            io::ErrorKind::InvalidData,
            read(&slot, 3).unwrap_err().kind()
        );
    }

    #[test]
    fn refuses_a_finished_slot_whose_words_make_no_tick() {
        // Goal: a slot the writer finished that holds no tick is a broken ring, which the
        // reader reports rather than reading.
        // Method: read publication 3 from a finished slot that says it holds two processes in
        // room for one.
        let slot = slot_holding(6, 40);
        slot[layout::SLOT_PROCESS_COUNT].store(2, Ordering::Relaxed);

        assert_eq!(
            io::ErrorKind::InvalidData,
            read(&slot, 3).unwrap_err().kind()
        );
    }
}
