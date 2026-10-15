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
mod view;
mod wake;

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering, fence};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use event::{load_event, store_event};
use layout::{
    EVENT_SEQUENCE, HEADER_EVENT_FIRST, HEADER_EVENT_NEWEST, HEADER_PUBLICATION_FIRST,
    HEADER_PUBLICATION_NEWEST, HEADER_RUN_START_NS, Header, Held, Layout, SLOT_SEQUENCE,
    store_header,
};
use object::{
    LockedName, Mapping, NUMBER_MAX, PROT_READ, PROT_WRITE, RingNames, create_lock, create_object,
    create_previous, create_unlink, create_unlink_older, open_ring, place_for_user, ring_name,
};
pub use object::{generation_name, unlink_generation};
pub use tick::{Dropped, IOReportReadings, Readings, SmcReadings, Tick};
pub use view::{RingReader, TickBuffers};

/// An opaque identity, independent of clocks and publication numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity(pub [u64; 2]);

/// The durable recording, this collector run and the boot it observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunIdentity {
    pub recording: Identity,
    pub run: Identity,
    pub boot: Identity,
}

/// Measured clocks in nanoseconds: Unix wall time, continuous time and awake time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationTimes {
    pub wall_ns: u64,
    pub continuous_ns: u64,
    pub awake_ns: u64,
}

use crate::kernel::ENOENT;
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
    /// The place of the ring `name` of this user, whose lock file is in a directory of this
    /// user's Application Support that only this user may enter. Returns an error where `name`
    /// is no file name, or where that directory lets another user in.
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
    name: String,
}

impl Writer {
    /// Takes the lock of `place` and makes a ring of `shape` in it, under the generation past
    /// the ring the lock file names, whose publication numbers it continues. Returns an error
    /// where another writer holds the lock.
    pub fn create(place: &Place, shape: Shape) -> io::Result<Self> {
        Self::create_inner(place, shape, None)
    }

    /// Creates a run without reclaiming any older generation. The owner registers and
    /// explicitly reclaims retained generations after accounting for their loss or seal.
    pub fn create_recording(
        place: &Place,
        shape: Shape,
        identity: &RunIdentity,
        run_start_continuous_ns: u64,
    ) -> io::Result<Self> {
        Self::create_inner(place, shape, Some((identity, run_start_continuous_ns)))
    }

    fn create_inner(
        place: &Place,
        shape: Shape,
        identity: Option<(&RunIdentity, u64)>,
    ) -> io::Result<Self> {
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
        let run_start_ns = u64::try_from(run_start_ns).map_err(io::Error::other)?;

        let lock = create_lock(&place.lock_path)?;

        let previous = create_previous(&place.name_prefix, &lock)?;

        let names = RingNames {
            prefix: &place.name_prefix,
            token: previous.token,
        };
        // A writer stopped after making a ring under a token it just drew would leave a ring
        // nothing can name, so the lock file names the token first. Generation 0 is no ring,
        // and a reader takes it as a ring that is gone.
        if previous.generation == 0 {
            create_name(&lock, &ring_name(&names, 0)?)?;
        }

        let created = create_object(&names, previous.generation, &layout)?;

        // The lock file names no ring a start made and then failed past, so the start removes
        // it, or each failed start would hold one of the generations the next may take.
        let mapping = Mapping::new(&created.object, layout.word_count(), PROT_READ | PROT_WRITE)
            .inspect_err(|_| create_unlink(&created.name))?;

        let header = Header {
            generation: created.generation,
            publication_first: previous.publication_newest + 1,
            run_start_ns,
            event_first: previous.event_newest + 1,
        };
        store_header(mapping.words(), &layout, &header);
        if let Some((identity, continuous_ns)) = identity {
            store_identity(mapping.words(), identity);
            mapping.words()[30].store(continuous_ns, Ordering::Relaxed);
        }

        create_name(&lock, &created.name).inspect_err(|_| create_unlink(&created.name))?;
        // Only once the lock file names the new ring, so a reader that reads the lock file from
        // now on never finds the ring it names gone. One that read it just before finds the
        // empty ring gone and looks again.
        if identity.is_none() {
            create_unlink_older(&names, previous.generation);
        }

        Ok(Self {
            _lock: lock,
            mapping,
            layout,
            publication: previous.publication_newest,
            event: previous.event_newest,
            name: created.name,
        })
    }

    /// Publishes `readings` in the next slot, overwriting the oldest publication once every
    /// slot holds one, and its events in the log, overwriting the oldest events once every
    /// record holds one.
    pub fn publish(&mut self, readings: &Readings) {
        self.publish_inner(readings, None);
    }

    pub fn publish_observed(&mut self, readings: &Readings, times: ObservationTimes) {
        self.publish_inner(readings, Some(times));
    }

    fn publish_inner(&mut self, readings: &Readings, times: Option<ObservationTimes>) {
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
        words[HEADER_EVENT_NEWEST].store(self.event, Ordering::Release);

        let publication = self.publication + 1;
        let slot = &words[self.layout.slot(publication)];
        slot[SLOT_SEQUENCE].store(2 * publication - 1, Ordering::Relaxed);
        fence(Ordering::Release);
        store_tick(slot, &self.layout, readings, events_first..self.event + 1);
        if let Some(times) = times {
            slot[layout::SLOT_WALL_NS].store(times.wall_ns, Ordering::Relaxed);
            slot[layout::SLOT_CONTINUOUS_NS].store(times.continuous_ns, Ordering::Relaxed);
            slot[layout::SLOT_AWAKE_NS].store(times.awake_ns, Ordering::Relaxed);
            slot[layout::SLOT_SOURCES]
                .fetch_or(layout::SOURCE_OBSERVATION_TIMES, Ordering::Relaxed);
        }
        slot[SLOT_SEQUENCE].store(2 * publication, Ordering::Release);

        words[HEADER_PUBLICATION_NEWEST].store(publication, Ordering::Release);
        self.publication = publication;
        // A refused wake costs the readers it missed one wait at most, so the writer goes on.
        if let Err(errno) = wake_all(&words[HEADER_PUBLICATION_NEWEST]) {
            eprintln!("ring: waking readers failed with errno {errno}");
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.mapping.words()[layout::HEADER_GENERATION].load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn identity(&self) -> Option<RunIdentity> {
        read_identity(self.mapping.words(), &self.layout)
    }

    /// Marks a known orderly end; a crash leaves this false.
    pub fn finish(&mut self) {
        self.mapping.words()[layout::HEADER_COMPLETED].store(1, Ordering::Release);
    }
}

fn store_identity(words: &[AtomicU64], identity: &RunIdentity) {
    for (index, value) in [identity.recording.0, identity.run.0, identity.boot.0]
        .into_iter()
        .flatten()
        .enumerate()
    {
        words[layout::HEADER_IDENTITY + index].store(value, Ordering::Relaxed);
    }
    words[31].store(1, Ordering::Relaxed);
}

fn read_identity(words: &[AtomicU64], layout: &Layout) -> Option<RunIdentity> {
    if layout.version < 3 {
        return None;
    }
    if words[31].load(Ordering::Relaxed) != 1 {
        return None;
    }
    let load = |index: usize| words[index].load(Ordering::Relaxed);
    Some(RunIdentity {
        recording: Identity([load(23), load(24)]),
        run: Identity([load(25), load(26)]),
        boot: Identity([load(27), load(28)]),
    })
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

/// A ring a reader opened, before it has room for the ring's ticks.
struct Opened {
    name: LockedName,
    mapping: Mapping,
    publications: Cursor,
    run_start_ns: u64,
    events: Cursor,
    layout: Layout,
}

impl Reader {
    /// Opens the ring the lock file at `lock_path` names, starting at the oldest publication
    /// it holds.
    pub fn open(lock_path: &Path) -> io::Result<Self> {
        let opened = Self::open_named(lock_path)?;
        Ok(Self {
            tick: LoadedTick::new(&opened.layout),
            name: opened.name,
            mapping: opened.mapping,
            publications: opened.publications,
            run_start_ns: opened.run_start_ns,
            events: opened.events,
            layout: opened.layout,
        })
    }

    /// The ring the lock file at `lock_path` names, with the reader's place at the oldest
    /// publication and event it holds, and no room for its ticks, which `follow` keeps.
    fn open_named(lock_path: &Path) -> io::Result<Opened> {
        let name = LockedName::read(&File::open(lock_path)?)?;
        if name.as_str().is_empty() {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }

        Self::open_name(name)
    }

    fn open_name(name: LockedName) -> io::Result<Opened> {
        let (mapping, layout) = open_ring(name.as_str())?;
        let words = mapping.words();
        let first = words[HEADER_PUBLICATION_FIRST].load(Ordering::Relaxed);
        let newest = words[HEADER_PUBLICATION_NEWEST].load(Ordering::Relaxed);
        let run_start_ns = words[HEADER_RUN_START_NS].load(Ordering::Relaxed);
        let event_first = words[HEADER_EVENT_FIRST].load(Ordering::Relaxed);
        let event_newest = words[HEADER_EVENT_NEWEST].load(Ordering::Relaxed);
        open_named_check(&Numbering { first, newest })?;
        open_named_check(&Numbering {
            first: event_first,
            newest: event_newest,
        })?;

        Ok(Opened {
            name,
            publications: Cursor::new(first, layout.publications_held(newest)),
            run_start_ns,
            events: Cursor::new(event_first, layout.events_held(event_newest)),
            mapping,
            layout,
        })
    }

    /// Whether the lock file at `lock_path` names another ring than the reader's, as it does
    /// once the writer restarted, so the reader opens the newer ring after it has read every
    /// publication of its own. A lock file the writer is part-way through writing, or one
    /// naming generation 0, which a writer names before it makes its first ring, names no
    /// ring yet, which is no move.
    pub fn moved(&self, lock_path: &Path) -> io::Result<bool> {
        let locked = LockedName::read(&File::open(lock_path)?)?;
        let name = locked.as_str();
        if name.is_empty() {
            return Ok(false);
        }

        if name.ends_with(".0") {
            return Ok(false);
        }

        Ok(name != self.name.as_str())
    }

    /// Opens the ring the lock file at `lock_path` names in place of the reader's own, once
    /// the writer restarted and the reader has read every publication its own ring holds and
    /// the events of the last, and returns whether it did. The reader reads on from where it
    /// stood, so the next reads return what the newer writer overwrote first as missed. A newer
    /// ring whose ticks have other capacities is refused with `io::ErrorKind::Unsupported`.
    pub fn follow(&mut self, lock_path: &Path) -> io::Result<bool> {
        if !self.moved(lock_path)? {
            return Ok(false);
        }

        // The writer that left this ring may have published once more before it stopped.
        let words = self.mapping.words();
        if self.publications.next <= words[HEADER_PUBLICATION_NEWEST].load(Ordering::Relaxed) {
            return Ok(false);
        }

        // Only the events a publication names are the reader's to read before it moves. A
        // writer that died between logging a tick's events and publishing its slot left
        // events no publication names, which the reader learns as missed on the newer ring.
        if self.events.next < self.tick.last.events.end {
            return Ok(false);
        }

        let mut newer = match Self::open_named(lock_path) {
            Ok(newer) => newer,
            // A writer started after the lock file was read, and removed the empty ring it
            // named, so the reader looks again on its next follow.
            Err(error) if error.raw_os_error() == Some(ENOENT) => return Ok(false),
            Err(error) => return Err(error),
        };

        // The room for ticks is sized to the capacities of a tick, so the reader keeps its own
        // and allocates nothing only when the newer ring's ticks have the same capacities. A
        // ring whose ticks hold more or fewer is refused as Unsupported, which allocates no
        // message, and a caller that wants it opens it afresh. Slot and event counts size
        // nothing the reader holds, so they may differ.
        let tick_shape = Shape {
            slot_count: newer.layout.shape.slot_count,
            event_capacity: newer.layout.shape.event_capacity,
            ..self.layout.shape
        };
        if tick_shape != newer.layout.shape {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }

        newer.publications.carry(&self.publications);
        newer.events.carry(&self.events);
        self.name = newer.name;
        self.mapping = newer.mapping;
        self.publications = newer.publications;
        self.run_start_ns = newer.run_start_ns;
        self.events = newer.events;
        self.layout = newer.layout;
        Ok(true)
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

        let newest_word = &self.mapping.words()[HEADER_PUBLICATION_NEWEST];
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
        let newest = read_newest(&words[HEADER_PUBLICATION_NEWEST])?;
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

    /// The number of the event the reader reads next, so it can read the events of one tick
    /// and no further.
    #[must_use]
    pub fn next_event(&self) -> u64 {
        self.events.next
    }

    /// Reads the next event, or the range of those the writer overwrote first.
    pub fn read_event(&mut self) -> io::Result<FoundEvent> {
        let words = self.mapping.words();
        let newest = read_newest(&words[HEADER_EVENT_NEWEST])?;
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

/// Writes `name` into the lock file. Writing the name before cutting the file to its length
/// never leaves the lock file empty, as cutting first would for a writer stopped between the
/// two, which would make the next writer number from 1.
fn create_name(lock: &File, name: &str) -> io::Result<()> {
    lock.write_all_at(name.as_bytes(), 0)?;
    lock.set_len(u64_from_usize(name.len()))
}

/// The newest number the header word `newest_word` holds, with what the writer wrote before
/// it made visible, or an error where it is past any number a writer stores.
fn read_newest(newest_word: &AtomicU64) -> io::Result<u64> {
    let newest = newest_word.load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    if newest > NUMBER_MAX {
        return Err(number_past_max(newest));
    }

    Ok(newest)
}

/// Where a reader stands in numbers the writer hands out from 1 and overwrites oldest first,
/// as its publications and its events.
struct Cursor {
    /// The first number of the ring, past the previous ring's.
    first: u64,
    /// The number the reader reads next.
    next: u64,
}

/// Refuses the numbers a header names in `numbering`, where no writer hands them
/// out: a numbering that starts at 0, or a number past `NUMBER_MAX`, which only another
/// program of this user could have written.
fn open_named_check(numbering: &Numbering) -> io::Result<()> {
    let Numbering { first, newest } = *numbering;
    if first == 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }

    if first > NUMBER_MAX {
        return Err(number_past_max(first));
    }

    if newest > NUMBER_MAX {
        return Err(number_past_max(newest));
    }

    Ok(())
}

/// The first and newest number a ring's header names for its publications or its events,
/// held apart by name since the two would swap unnoticed as arguments.
#[derive(Clone, Copy)]
struct Numbering {
    first: u64,
    newest: u64,
}

/// The error of a ring whose header names `number`, past any a writer hands out.
fn number_past_max(number: u64) -> io::Error {
    assert!(number > NUMBER_MAX);
    io::Error::from(io::ErrorKind::InvalidData)
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
    /// a numbering of its own, as after a lock file nobody could read, so the cursor starts
    /// at that numbering's first and the next step reports what the ring no longer holds.
    fn carry(&mut self, older: &Self) {
        self.next = older.next.min(self.first);
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

    // The reader refuses a header naming a newer number, so the sequence cannot overflow.
    assert!(number <= NUMBER_MAX);

    let finished = 2 * number;
    let sequence = record[sequence_index].load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    let loaded = load();
    fence(Ordering::Acquire);
    let sequence_after = record[sequence_index].load(Ordering::Relaxed);

    if sequence < finished {
        // A record only moves on to later numbers.
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }

    if sequence == finished {
        if sequence_after == sequence {
            if loaded.is_none() {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
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
    fn drops_a_slot_its_writer_rewrote_during_the_load() {
        // Goal: a slot the writer starts rewriting while a reader loads it is dropped, never
        // returned with words of two publications, however the two threads are paced.
        // Method: read publication 3 from a slot finished for it, with a load that moves the
        // slot's sequence to twice 5 less 1, as a writer a lap ahead in a ring of two slots
        // leaves it when it starts publication 5.
        let slot = slot_holding(6, 40);

        let loaded = read_finished(&slot, SLOT_SEQUENCE, 3, || {
            slot[SLOT_SEQUENCE].store(9, Ordering::Relaxed);
            Some(())
        });

        assert_eq!(None, loaded.unwrap());
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

    #[test]
    fn layout_two_readings_keep_their_values_and_have_unknown_exact_clocks() {
        // Goal: old measured readings survive without invented observation timestamps.
        // Method: remove layout-three extension words from a published slot and decode it.
        let current = layout();
        let legacy = Layout::versioned(current.shape, 2).unwrap();
        let mut slot = slot_holding(2, 73);
        slot.drain(layout::SLOT_WALL_NS..layout::SLOT_FIXED_WORDS);
        let mut tick = Tick::new(&legacy);

        assert!(load_tick(&slot, &legacy, &mut tick));

        assert_eq!(73, tick.run_tick);
        assert_eq!(None, tick.times);
        assert_eq!(Memory::default(), tick.memory);
    }
}
