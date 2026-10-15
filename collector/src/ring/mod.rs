//! The shared-memory ring the collector hands each tick to, which other processes of the same
//! user map read-only and read at their own pace. One writer publishes each tick into the
//! next slot and never waits for a reader. A reader copies a slot and checks its sequence word
//! again, as a single-writer seqlock does, so it drops a slot overwritten during the copy
//! instead of reading it torn, and learns from the publication numbers which it missed.

mod layout;
mod object;

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering, fence};

use crate::u64_from_usize;
use layout::{
    HEADER_FIRST_PUBLICATION, HEADER_GENERATION, HEADER_LAYOUT_VERSION, HEADER_MAGIC,
    HEADER_NEWEST_PUBLICATION, HEADER_SLOT_COUNT, HEADER_SLOT_WORDS, LAYOUT_VERSION, Layout, MAGIC,
    SLOT_RUN_TICK, SLOT_SEQUENCE, SLOT_WORDS,
};
use object::{
    Mapping, PROT_READ, PROT_WRITE, create_lock, create_object, create_previous,
    create_unlink_older, open_ring, store,
};

/// Where a ring lives: the lock file its writer holds, which names the ring, and the start of
/// the name of each ring the writer makes.
pub struct Place {
    pub lock_path: PathBuf,
    pub name_prefix: String,
}

/// The room a ring takes, fixed when its writer makes it.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    /// The publications the ring holds before it overwrites the oldest.
    pub slot_count: u64,
}

/// What a reader found at its next publication.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    /// The publication it read, which carries the tick of the collector's run.
    Publication { publication: u64, tick: u64 },
    /// The publications from `first` up to `end` that the writer overwrote before the reader
    /// read them, which it reads past.
    Missed { first: u64, end: u64 },
    /// No publication past those it has read yet.
    Nothing,
}

/// The writer of a ring, which holds the lock file for as long as it lives.
pub struct Writer {
    _lock: File,
    mapping: Mapping,
    layout: Layout,
    /// The newest publication written, or the previous ring's newest before the first.
    publication: u64,
}

impl Writer {
    /// Takes the lock of `place` and makes a ring of `shape` in it, under the generation past
    /// the ring the lock file names, whose publication numbers it continues. Returns an error
    /// where another writer holds the lock.
    pub fn create(place: &Place, shape: Shape) -> io::Result<Self> {
        assert!(shape.slot_count > 0);

        let mut lock = create_lock(&place.lock_path)?;
        let previous = create_previous(&place.name_prefix, &mut lock)?;
        let layout = Layout::new(shape);
        let created = create_object(&place.name_prefix, previous.generation, layout)?;
        let mapping = Mapping::new(&created.object, layout.word_count(), PROT_READ | PROT_WRITE)?;

        let words = mapping.words();
        store(words, HEADER_MAGIC, MAGIC);
        store(words, HEADER_LAYOUT_VERSION, LAYOUT_VERSION);
        store(words, HEADER_GENERATION, created.generation);
        store(words, HEADER_SLOT_COUNT, shape.slot_count);
        store(words, HEADER_SLOT_WORDS, u64_from_usize(SLOT_WORDS));
        store(
            words,
            HEADER_FIRST_PUBLICATION,
            previous.newest_publication + 1,
        );
        store(
            words,
            HEADER_NEWEST_PUBLICATION,
            previous.newest_publication,
        );

        create_unlink_older(&place.name_prefix, previous.generation);
        lock.set_len(0)?;
        lock.write_all_at(created.name.as_bytes(), 0)?;
        Ok(Self {
            _lock: lock,
            mapping,
            layout,
            publication: previous.newest_publication,
        })
    }

    /// Publishes the tick `tick` of the collector's run in the next slot, overwriting the
    /// oldest publication once every slot holds one.
    pub fn publish(&mut self, tick: u64) -> io::Result<()> {
        let publication = self.publication + 1;
        let words = self.mapping.words();
        let slot = self.layout.slot_start(publication);

        words[slot + SLOT_SEQUENCE].store(2 * publication - 1, Ordering::Relaxed);
        fence(Ordering::Release);
        store(words, slot + SLOT_RUN_TICK, tick);
        words[slot + SLOT_SEQUENCE].store(2 * publication, Ordering::Release);

        words[HEADER_NEWEST_PUBLICATION].store(publication, Ordering::Release);
        self.publication = publication;
        Ok(())
    }
}

/// A reader of a ring, which maps it read-only.
pub struct Reader {
    mapping: Mapping,
    layout: Layout,
    /// The first publication of the ring, past the previous ring's.
    first: u64,
    /// The publication the reader reads next.
    next: u64,
}

impl Reader {
    /// Opens the ring the lock file at `lock_path` names, starting at the oldest publication
    /// it holds.
    pub fn open(lock_path: &Path) -> io::Result<Self> {
        let mut name = String::new();
        File::open(lock_path)?.read_to_string(&mut name)?;

        let (mapping, layout) = open_ring(&name)?;
        let words = mapping.words();
        let first = words[HEADER_FIRST_PUBLICATION].load(Ordering::Relaxed);
        let newest = words[HEADER_NEWEST_PUBLICATION].load(Ordering::Relaxed);
        Ok(Self {
            mapping,
            layout,
            first,
            next: first.max(layout.oldest_held(newest)),
        })
    }

    /// Reads the next publication, or the range of those the writer overwrote first.
    pub fn read(&mut self) -> io::Result<Found> {
        let words = self.mapping.words();
        let newest = words[HEADER_NEWEST_PUBLICATION].load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        if self.next > newest {
            return Ok(Found::Nothing);
        }

        let oldest = self.first.max(self.layout.oldest_held(newest));
        if self.next < oldest {
            let first = self.next;
            self.next = oldest;
            return Ok(Found::Missed { first, end: oldest });
        }

        let publication = self.next;
        self.next += 1;
        let slot = self.layout.slot_start(publication);
        match read_slot(&words[slot..slot + SLOT_WORDS], publication)? {
            Some(tick) => Ok(Found::Publication { publication, tick }),
            None => Ok(Found::Missed {
                first: publication,
                end: publication + 1,
            }),
        }
    }
}

/// The tick the slot `slot` holds for `publication`, or `None` where the writer overwrote
/// the slot with a later publication before or during the copy.
fn read_slot(slot: &[AtomicU64], publication: u64) -> io::Result<Option<u64>> {
    assert!(publication > 0);

    let sequence = slot[SLOT_SEQUENCE].load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    let tick = slot[SLOT_RUN_TICK].load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    let sequence_after = slot[SLOT_SEQUENCE].load(Ordering::Relaxed);

    if sequence < 2 * publication {
        // The header said the writer finished this publication, and a slot only moves on.
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the slot of publication {publication} holds sequence {sequence}"),
        ));
    }
    if sequence == 2 * publication {
        if sequence_after == sequence {
            return Ok(Some(tick));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot_holding(sequence: u64, tick: u64) -> [AtomicU64; SLOT_WORDS] {
        let mut slot = [const { AtomicU64::new(0) }; SLOT_WORDS];
        slot[SLOT_SEQUENCE] = AtomicU64::new(sequence);
        slot[SLOT_RUN_TICK] = AtomicU64::new(tick);
        slot
    }

    #[test]
    fn reads_the_tick_of_a_slot_its_writer_finished() {
        // Goal: a finished slot gives its tick.
        // Method: read publication 3 from a slot whose sequence is twice 3.
        let slot = slot_holding(6, 40);

        assert_eq!(Some(40), read_slot(&slot, 3).unwrap());
    }

    #[test]
    fn drops_a_slot_its_writer_is_writing_a_later_publication_into() {
        // Goal: a reader never reads a slot the writer is part-way through.
        // Method: read publication 3 from a slot of four whose writer started publication 7,
        // which leaves its sequence at twice 7 less 1.
        let slot = slot_holding(13, 40);

        assert_eq!(None, read_slot(&slot, 3).unwrap());
    }

    #[test]
    fn drops_a_slot_its_writer_finished_a_later_publication_in() {
        // Goal: a slot overwritten before the reader came to it is missed, never read as the
        // publication the reader asked for.
        // Method: read publication 3 from a slot that holds publication 7.
        let slot = slot_holding(14, 40);

        assert_eq!(None, read_slot(&slot, 3).unwrap());
    }

    #[test]
    fn refuses_a_slot_behind_the_publication_the_header_finished() {
        // Goal: a slot behind a publication the header calls finished is a broken ring, which
        // the reader reports rather than reading.
        // Method: read publication 3 from a slot that holds publication 2.
        let slot = slot_holding(4, 40);

        assert_eq!(
            io::ErrorKind::InvalidData,
            read_slot(&slot, 3).unwrap_err().kind()
        );
    }
}
