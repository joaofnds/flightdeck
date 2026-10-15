//! The shared-memory object that holds a ring, and the lock file that names it.

use std::ffi::OsStr;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::layout::{
    HEADER_FIRST_PUBLICATION, HEADER_NEWEST_EVENT, HEADER_NEWEST_PUBLICATION, HEADER_WORDS, Layout,
    open_layout,
};
use crate::kernel::ENOENT;
use crate::u64_from_usize;

/// The longest name macOS gives a shared-memory object, `PSHMNAMLEN` in `sys/posix_shm.h`.
const PSHMNAMLEN: usize = 31;

/// The mode of every ring: its owner reads it and nobody writes it, so after the writer made
/// it, no process can open it for writing, the writer's own included.
const RING_MODE: c_uint = 0o400;
const LOCK_MODE: u32 = 0o600;

/// The generations a writer tries past the previous ring's before it gives up, each name
/// taken by an object it did not make.
const GENERATION_TRIES: u64 = 16;

/// The largest publication or event number a writer continues from. Twice a number past it
/// would overflow the sequence word, and at ten ticks a second no run comes near it.
const NUMBER_MAX: u64 = u64::MAX / 4;

/// The `confstr` name of this user's own temporary directory, from `unistd.h`.
const CS_DARWIN_USER_TEMP_DIR: c_int = 65537;
/// The longest path macOS takes, `PATH_MAX` in `sys/syslimits.h`.
const PATH_MAX: usize = 1024;

const O_RDONLY: c_int = 0x0000;
const O_RDWR: c_int = 0x0002;
const O_CREAT: c_int = 0x0200;
const O_EXCL: c_int = 0x0800;
pub(super) const PROT_READ: c_int = 0x01;
pub(super) const PROT_WRITE: c_int = 0x02;
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
    fn confstr(name: c_int, buffer: *mut c_char, length: usize) -> usize;
}

/// A shared-memory object mapped into this process, which unmaps it when it drops.
pub(super) struct Mapping {
    address: *mut c_void,
    word_count: usize,
}

impl Mapping {
    pub(super) fn new(object: &OwnedFd, word_count: usize, protection: c_int) -> io::Result<Self> {
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

    pub(super) fn words(&self) -> &[AtomicU64] {
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

/// Opens the lock file at `lock_path` and takes its lock, or returns an error where another
/// writer holds it.
pub(super) fn create_lock(lock_path: &Path) -> io::Result<File> {
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

/// The name of a ring a lock file holds, read into a buffer of the longest name macOS gives,
/// since another program of this user may write the lock file at any length.
pub(super) struct LockedName {
    bytes: [u8; PSHMNAMLEN],
    length: usize,
}

impl LockedName {
    /// Reads the name the lock file `lock` holds, which is empty where no writer wrote one, or
    /// an error where it is longer than a ring name or no text.
    pub(super) fn read(lock: &File) -> io::Result<Self> {
        // One byte past the longest name tells a name that fills the buffer from a longer one.
        let mut bytes = [0; PSHMNAMLEN + 1];
        let mut length = 0;
        // Each read but the last fills at least a byte of the buffer, so the loop ends.
        while length < bytes.len() {
            let read = lock.read_at(&mut bytes[length..], u64_from_usize(length))?;
            if read == 0 {
                break;
            }

            length += read;
        }

        if length > PSHMNAMLEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the lock file holds more than a ring name of {PSHMNAMLEN} bytes"),
            ));
        }
        if std::str::from_utf8(&bytes[..length]).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the lock file holds no text",
            ));
        }

        let mut name = [0; PSHMNAMLEN];
        name.copy_from_slice(&bytes[..PSHMNAMLEN]);
        Ok(Self {
            bytes: name,
            length,
        })
    }

    pub(super) fn as_str(&self) -> &str {
        match std::str::from_utf8(&self.bytes[..self.length]) {
            Ok(name) => name,
            Err(error) => unreachable!("read refuses a name that is no text: {error}"),
        }
    }
}

/// What a writer continues from the ring before it.
pub(super) struct Previous {
    /// The previous ring's generation, or 0 where the lock file names none.
    pub(super) generation: u64,
    /// The previous ring's newest publication, or 0 where it is gone.
    pub(super) newest_publication: u64,
    /// The previous ring's newest event, or 0 where it is gone.
    pub(super) newest_event: u64,
}

/// The ring the lock file `lock` names, of a name that starts with `prefix`.
pub(super) fn create_previous(prefix: &str, lock: &File) -> io::Result<Previous> {
    let locked = match LockedName::read(lock) {
        Ok(locked) => Some(locked),
        Err(error) => {
            if error.kind() != io::ErrorKind::InvalidData {
                return Err(error);
            }

            eprintln!("ring: numbering from 1, since {error}");
            None
        }
    };
    let name = locked.as_ref().map_or("", LockedName::as_str);

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
            newest_event: 0,
        });
    }

    // A restart after the Mac restarted finds no ring, which is no fault.
    let (newest_publication, newest_event) = match open_ring(name) {
        Ok((mapping, _)) => {
            let words = mapping.words();
            (
                words[HEADER_NEWEST_PUBLICATION].load(Ordering::Relaxed),
                words[HEADER_NEWEST_EVENT].load(Ordering::Relaxed),
            )
        }
        Err(error) if error.raw_os_error() == Some(ENOENT) => (0, 0),
        Err(error) => {
            eprintln!("ring: numbering from 1, since the ring {name} is unreadable: {error}");
            (0, 0)
        }
    };
    Ok(Previous {
        generation,
        newest_publication: create_previous_continued(newest_publication, name),
        newest_event: create_previous_continued(newest_event, name),
    })
}

/// The number `newest` the ring `name` names, to continue from, or 0 where it is past
/// `NUMBER_MAX`, which only another program of this user could have written.
fn create_previous_continued(newest: u64, name: &str) -> u64 {
    if newest > NUMBER_MAX {
        eprintln!("ring: numbering from 1, since the ring {name} names number {newest}");
        return 0;
    }

    newest
}

/// A ring's object a writer made.
pub(super) struct Created {
    pub(super) name: String,
    pub(super) generation: u64,
    pub(super) object: OwnedFd,
}

/// Makes the object of a ring of `layout` under the first generation past `previous` whose
/// name no object holds.
pub(super) fn create_object(prefix: &str, previous: u64, layout: &Layout) -> io::Result<Created> {
    // The lock file another program may write names the previous generation, so it may be
    // any number.
    let Some(last) = previous.checked_add(GENERATION_TRIES) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the lock file names generation {previous}, past the last a ring takes"),
        ));
    };
    for generation in previous + 1..=last {
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

/// Removes every ring before the new one but the newest that holds publications, so at most
/// two rings exist: the new one and the last with something to read, which the persister
/// seals. A writer that died before its first publication left an empty ring, which goes,
/// so a collector failing at each start never costs the publications of the run before. A
/// start that passed over generations held by other objects left a ring further back, so the
/// generations a start may pass over are looked at too.
pub(super) fn create_unlink_older(prefix: &str, previous: u64) {
    let first = previous.saturating_sub(GENERATION_TRIES + 1).max(1);
    let mut kept = false;
    for generation in (first..=previous).rev() {
        let name = match ring_name(prefix, generation) {
            Ok(name) => name,
            Err(error) => {
                unreachable!("the longer name {previous} took was short enough: {error}")
            }
        };
        if !kept {
            if create_unlink_older_holds_publications(&name) {
                kept = true;
                continue;
            }
        }

        let name = match CString::new(name) {
            Ok(name) => name,
            Err(error) => unreachable!("{error}"),
        };
        if unsafe { shm_unlink(name.as_ptr()) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ENOENT) {
                eprintln!("ring: unlink {name:?} failed: {error}");
            }
        }
    }
}

/// Whether the ring `name` is a ring of this user that holds a publication of its own. One
/// that is gone, another program's, of another layout, or empty holds nothing to keep.
fn create_unlink_older_holds_publications(name: &str) -> bool {
    match open_ring(name) {
        Ok((mapping, _)) => {
            let words = mapping.words();
            let first = words[HEADER_FIRST_PUBLICATION].load(Ordering::Relaxed);
            let newest = words[HEADER_NEWEST_PUBLICATION].load(Ordering::Relaxed);
            newest >= first
        }
        Err(error) => {
            if error.raw_os_error() != Some(ENOENT) {
                eprintln!("ring: removing {name}, which is unreadable: {error}");
            }
            false
        }
    }
}

/// Maps the header and then the whole of the ring `name`, after checking that this user owns
/// it and nobody may write it.
pub(super) fn open_ring(name: &str) -> io::Result<(Mapping, Layout)> {
    let mut buffer = [0; PSHMNAMLEN + 1];
    let name = open_ring_name(name, &mut buffer)?;
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

/// The name `name` with the NUL a kernel call needs, in `buffer`, so a reader that follows a
/// newer ring allocates nothing to name it.
fn open_ring_name<'buffer>(
    name: &str,
    buffer: &'buffer mut [u8; PSHMNAMLEN + 1],
) -> io::Result<&'buffer CStr> {
    let length = name.len();
    if length > PSHMNAMLEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a ring name of more than {PSHMNAMLEN} bytes"),
        ));
    }

    buffer[..length].copy_from_slice(name.as_bytes());
    buffer[length] = 0;
    CStr::from_bytes_with_nul(&buffer[..=length]).map_err(io::Error::other)
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

/// The place of the ring `name` of this user: its lock file in this user's own temporary
/// directory, which no other user may enter, and objects named for the user, since every
/// user shares the names of shared-memory objects.
pub(super) fn place_for_user(name: &str) -> io::Result<super::Place> {
    // The name becomes a file name in the directory, so it holds no slash and no step out of it.
    let named = name
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_'));
    if !named {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("ring name {name:?} holds more than letters, digits, - and _"),
        ));
    }
    if name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the ring name is empty",
        ));
    }

    let mut buffer = [0u8; PATH_MAX];
    let length = unsafe {
        confstr(
            CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast::<c_char>(),
            buffer.len(),
        )
    };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }

    // The length counts the closing NUL, and one past the buffer means the path was cut off.
    if length > buffer.len() {
        return Err(io::Error::other(
            "this user's temporary directory is past PATH_MAX",
        ));
    }

    let directory = PathBuf::from(OsStr::from_bytes(&buffer[..length - 1]));
    let uid = unsafe { getuid() };

    let place = super::Place {
        lock_path: directory.join(format!("{name}.lock")),
        name_prefix: format!("/{name}.{uid}."),
    };
    // A ring's name holds the prefix and a generation, within what macOS takes.
    ring_name(&place.name_prefix, u64::from(u32::MAX))?;
    Ok(place)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_lock_file_name_as_long_as_macos_gives_and_no_longer() {
        // Goal: a lock file holding a name of the longest length macOS gives is read whole,
        // and one a byte longer is refused rather than cut.
        // Method: write names of 31 and 32 bytes into a lock file of this test's own and read.
        let path = std::env::temp_dir().join(format!("flightdeck-name-{}", std::process::id()));
        let longest = "a".repeat(PSHMNAMLEN);
        let longer = "a".repeat(PSHMNAMLEN + 1);

        std::fs::write(&path, &longest).unwrap();
        let read_longest = LockedName::read(&File::open(&path).unwrap());
        std::fs::write(&path, &longer).unwrap();
        let read_longer = LockedName::read(&File::open(&path).unwrap());
        std::fs::remove_file(&path).unwrap();

        assert_eq!(longest, read_longest.unwrap().as_str());
        assert_eq!(
            io::ErrorKind::InvalidData,
            read_longer.err().unwrap().kind()
        );
    }

    #[test]
    fn a_writer_continues_a_numbering_only_far_from_overflow() {
        // Goal: a ring of this user whose header names a number near the largest a word holds
        // starts the numbering again, rather than overflowing when the writer counts on.
        // Method: ask for the number to continue from of the largest number allowed and of
        // the one past it.
        let largest = create_previous_continued(NUMBER_MAX, "/r.1");
        let past = create_previous_continued(NUMBER_MAX + 1, "/r.1");

        assert_eq!(NUMBER_MAX, largest);
        assert_eq!(0, past);
    }
}
