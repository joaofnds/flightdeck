//! The shared-memory ring the collector hands each tick to, which other processes of the same
//! user map read-only and read at their own pace. One writer publishes each tick into the
//! next slot and never waits for a reader. A reader copies a slot and checks its sequence word
//! again, as a single-writer seqlock does, so it drops a slot overwritten during the copy
//! instead of reading it torn, and learns from the publication numbers which it missed.

use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering, fence};

use crate::u64_from_usize;

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

/// Marks the start of a ring, so a reader never takes another object for one.
const MAGIC: u64 = u64::from_be_bytes(*b"FLTDRING");
/// The layout this code writes and reads, which a change to the layout raises.
const LAYOUT_VERSION: u64 = 1;

/// The words of the header, which starts the ring.
const HEADER_MAGIC: usize = 0;
const HEADER_LAYOUT_VERSION: usize = 1;
const HEADER_GENERATION: usize = 2;
const HEADER_SLOT_COUNT: usize = 3;
const HEADER_SLOT_WORDS: usize = 4;
/// The ring's first publication, one past the newest of the ring before it.
const HEADER_FIRST_PUBLICATION: usize = 5;
/// The newest publication the writer finished, which before its first is the previous ring's.
const HEADER_NEWEST_PUBLICATION: usize = 6;
const HEADER_WORDS: usize = 8;

/// The words of a slot. The sequence word holds twice the publication the slot holds once
/// the writer finished it, and one less while the writer writes it.
const SLOT_SEQUENCE: usize = 0;
const SLOT_RUN_TICK: usize = 1;
const SLOT_WORDS: usize = 2;

/// The longest name macOS gives a shared-memory object, `PSHMNAMLEN` in `sys/posix_shm.h`.
const PSHMNAMLEN: usize = 31;

/// The mode of every ring: its owner reads it and nobody writes it, so after the writer made
/// it, no process can open it for writing, the writer's own included.
const RING_MODE: c_uint = 0o400;
const LOCK_MODE: u32 = 0o600;

/// The generations a writer tries past the previous ring's before it gives up, each name
/// taken by an object it did not make.
const GENERATION_TRIES: u64 = 16;

const ENOENT: i32 = 2;

const O_RDONLY: c_int = 0x0000;
const O_RDWR: c_int = 0x0002;
const O_CREAT: c_int = 0x0200;
const O_EXCL: c_int = 0x0800;
const PROT_READ: c_int = 0x01;
const PROT_WRITE: c_int = 0x02;
const MAP_SHARED: c_int = 0x0001;

unsafe extern "C" {
    fn shm_open(name: *const c_char, oflag: c_int, ...) -> c_int;
    fn ftruncate(fd: c_int, length: i64) -> c_int;
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
    fn shm_unlink(name: *const c_char) -> c_int;
    fn getuid() -> u32;
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

/// Where each part of a ring lies, in words from its start.
#[derive(Debug, Clone, Copy)]
struct Layout {
    shape: Shape,
}

impl Layout {
    fn new(shape: Shape) -> Self {
        assert!(shape.slot_count > 0);

        Self { shape }
    }

    fn slot_count(self) -> usize {
        match usize::try_from(self.shape.slot_count) {
            Ok(count) => count,
            Err(error) => unreachable!("{error}"),
        }
    }

    fn word_count(self) -> usize {
        HEADER_WORDS + self.slot_count() * SLOT_WORDS
    }

    fn size_bytes(self) -> usize {
        self.word_count() * size_of::<u64>()
    }

    /// The oldest publication the slots hold once `newest` is published, where every
    /// publication numbered from 1 went into them.
    fn oldest_held(self, newest: u64) -> u64 {
        newest.saturating_sub(self.shape.slot_count) + 1
    }

    /// The first word of the slot that holds `publication`.
    fn slot_start(self, publication: u64) -> usize {
        assert!(publication > 0);

        let index = (publication - 1) % self.shape.slot_count;
        match usize::try_from(index) {
            Ok(index) => HEADER_WORDS + index * SLOT_WORDS,
            Err(error) => unreachable!("{error}"),
        }
    }
}

/// A shared-memory object mapped into this process, which unmaps it when it drops.
struct Mapping {
    address: *mut c_void,
    word_count: usize,
}

impl Mapping {
    fn new(object: &OwnedFd, word_count: usize, protection: c_int) -> io::Result<Self> {
        assert!(word_count > 0);

        let address = unsafe {
            mmap(
                std::ptr::null_mut(),
                word_count * size_of::<u64>(),
                protection,
                MAP_SHARED,
                object.as_raw_fd(),
                0,
            )
        };
        // MAP_FAILED, which sys/mman.h declares as -1 cast to a pointer.
        if address.addr() == usize::MAX {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            address,
            word_count,
        })
    }

    fn words(&self) -> &[AtomicU64] {
        unsafe { std::slice::from_raw_parts(self.address.cast::<AtomicU64>(), self.word_count) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if unsafe { munmap(self.address, self.word_count * size_of::<u64>()) } != 0 {
            eprintln!("ring: munmap failed: {}", io::Error::last_os_error());
        }
    }
}

fn store(words: &[AtomicU64], index: usize, value: u64) {
    words[index].store(value, Ordering::Relaxed);
}

/// Opens the lock file at `lock_path` and takes its lock, or returns an error where another
/// writer holds it.
fn create_lock(lock_path: &Path) -> io::Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(LOCK_MODE)
        .open(lock_path)?;

    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "another writer holds the ring's lock",
        )),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

/// What a writer continues from the ring before it.
struct Previous {
    /// The previous ring's generation, or 0 where the lock file names none.
    generation: u64,
    /// The previous ring's newest publication, or 0 where it is gone.
    newest_publication: u64,
}

/// The ring the lock file `lock` names, of a name that starts with `prefix`.
fn create_previous(prefix: &str, lock: &mut File) -> io::Result<Previous> {
    let mut name = String::new();
    lock.read_to_string(&mut name)?;

    // A lock file a writer never wrote is empty.
    let generation = if let Some(Ok(generation)) = name.strip_prefix(prefix).map(str::parse) {
        generation
    } else {
        if !name.is_empty() {
            eprintln!("ring: numbering from 1, since the lock file names {name:?}");
        }
        0
    };
    if generation == 0 {
        return Ok(Previous {
            generation,
            newest_publication: 0,
        });
    }

    // A restart after the Mac restarted finds no ring, which is no fault.
    let newest_publication = match open_ring(&name) {
        Ok((mapping, _)) => mapping.words()[HEADER_NEWEST_PUBLICATION].load(Ordering::Relaxed),
        Err(error) if error.raw_os_error() == Some(ENOENT) => 0,
        Err(error) => {
            eprintln!("ring: numbering from 1, since the ring {name} is unreadable: {error}");
            0
        }
    };
    Ok(Previous {
        generation,
        newest_publication,
    })
}

/// A ring's object a writer made.
struct Created {
    name: String,
    generation: u64,
    object: OwnedFd,
}

/// Makes the object of a ring of `layout` under the first generation past `previous` whose
/// name no object holds.
fn create_object(prefix: &str, previous: u64, layout: Layout) -> io::Result<Created> {
    for generation in previous + 1..=previous + GENERATION_TRIES {
        let name = ring_name(prefix, generation)?;
        match create_object_named(&name, layout.size_bytes()) {
            Ok(object) => {
                return Ok(Created {
                    name,
                    generation,
                    object,
                });
            }
            Err(error) => {
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("objects hold the {GENERATION_TRIES} ring names past generation {previous}"),
    ))
}

fn ring_name(prefix: &str, generation: u64) -> io::Result<String> {
    let name = format!("{prefix}{generation}");
    if name.len() > PSHMNAMLEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("ring name {name} is past {PSHMNAMLEN} characters"),
        ));
    }
    Ok(name)
}

fn create_object_named(name: &str, size_bytes: usize) -> io::Result<OwnedFd> {
    let name = CString::new(name).map_err(io::Error::other)?;
    // macOS makes the descriptor close on exec itself, and refuses O_CLOEXEC with EINVAL.
    let object = unsafe { shm_open(name.as_ptr(), O_RDWR | O_CREAT | O_EXCL, RING_MODE) };
    if object < 0 {
        return Err(io::Error::last_os_error());
    }
    let object = unsafe { OwnedFd::from_raw_fd(object) };

    let length = i64::try_from(size_bytes).map_err(io::Error::other)?;
    if unsafe { ftruncate(object.as_raw_fd(), length) } != 0 {
        let error = io::Error::last_os_error();
        // An object of no size is no ring, and nothing else would remove it.
        if unsafe { shm_unlink(name.as_ptr()) } != 0 {
            eprintln!(
                "ring: unlink {name:?} failed: {}",
                io::Error::last_os_error()
            );
        }
        return Err(error);
    }
    Ok(object)
}

/// Removes the ring before the previous one, so at most two rings exist: the new one and the
/// previous one, which the persister seals.
fn create_unlink_older(prefix: &str, previous: u64) {
    if previous < 2 {
        return;
    }

    let name = match ring_name(prefix, previous - 1).map(CString::new) {
        Ok(Ok(name)) => name,
        Ok(Err(error)) => unreachable!("{error}"),
        Err(error) => unreachable!("the longer name {previous} took was short enough: {error}"),
    };
    if unsafe { shm_unlink(name.as_ptr()) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ENOENT) {
            eprintln!("ring: unlink {name:?} failed: {error}");
        }
    }
}

/// Maps the header and then the whole of the ring `name`, after checking that this user owns
/// it and nobody may write it.
fn open_ring(name: &str) -> io::Result<(Mapping, Layout)> {
    let name = CString::new(name).map_err(io::Error::other)?;
    let object = unsafe { shm_open(name.as_ptr(), O_RDONLY) };
    if object < 0 {
        return Err(io::Error::last_os_error());
    }
    let object = unsafe { OwnedFd::from_raw_fd(object) };

    let size_bytes = open_check(&File::from(object.try_clone()?))?;
    let header = Mapping::new(&object, HEADER_WORDS, PROT_READ)?;
    let layout = open_layout(header.words(), size_bytes)?;
    let mapping = Mapping::new(&object, layout.word_count(), PROT_READ)?;
    Ok((mapping, layout))
}

/// The size of the object `object`, or an error where this user does not own it or anyone
/// may write it.
fn open_check(object: &File) -> io::Result<usize> {
    let metadata = object.metadata()?;
    if metadata.uid() != unsafe { getuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the ring belongs to uid {}", metadata.uid()),
        ));
    }
    if metadata.mode() & 0o777 != RING_MODE {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the ring has mode {:o}", metadata.mode() & 0o777),
        ));
    }
    usize::try_from(metadata.len()).map_err(io::Error::other)
}

fn open_layout(header: &[AtomicU64], size_bytes: usize) -> io::Result<Layout> {
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

    let layout = Layout::new(Shape {
        slot_count: load(HEADER_SLOT_COUNT),
    });
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
