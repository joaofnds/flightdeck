//! The shared-memory object that holds a ring, and the lock file that names it.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::layout::{
    HEADER_EVENT_NEWEST, HEADER_PUBLICATION_FIRST, HEADER_PUBLICATION_NEWEST, HEADER_WORDS, Layout,
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

/// The start of the name of every ring of a user's place. Every user shares the names of
/// shared-memory objects, and the token after it tells one place's rings from another's.
const RING_NAME_PREFIX: &str = "/fd.";

/// The characters of a token, the lowercase base32 alphabet of RFC 4648, 5 bits each.
const TOKEN_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
/// The characters of a token, 60 random bits, so another user who cannot read the lock file
/// cannot guess the names a writer will make and take them first.
const TOKEN_LENGTH: usize = 12;

const _: () = {
    // A ring of a user's place takes a generation of at least 10 digits, past what the starts
    // of a lifetime reach.
    assert!(RING_NAME_PREFIX.len() + TOKEN_LENGTH + 1 + 10 <= PSHMNAMLEN);
    // A lock file holds a name of at most PSHMNAMLEN bytes, a token and a dot among them, so
    // the generation it names has at most 18 digits, and the tries past it fit in a u64.
    assert!(PSHMNAMLEN - TOKEN_LENGTH - 1 <= 18);
};

/// The largest publication or event number a ring may name. A record's sequence word holds
/// twice its number, so a number past half of what a word holds overflows it, and this bound
/// leaves a writer room to count on from a ring at the bound. At ten ticks a second no run
/// comes near it.
pub(super) const NUMBER_MAX: u64 = u64::MAX / 4;

/// The mode of the directory of a user's lock files, which no other user may enter.
const DIRECTORY_MODE: u32 = 0o700;

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
    fn getentropy(buffer: *mut c_void, size: usize) -> c_int;
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
        .append(false)
        .create_new(false)
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

/// The random part of the names of a place's rings, which only the lock file holds.
#[derive(Clone, Copy)]
pub(super) struct Token {
    bytes: [u8; TOKEN_LENGTH],
}

impl Token {
    /// A token of the operating system's random bits.
    fn random() -> io::Result<Self> {
        let mut bytes = [0; TOKEN_LENGTH];
        if unsafe { getentropy(bytes.as_mut_ptr().cast::<c_void>(), bytes.len()) } != 0 {
            return Err(io::Error::last_os_error());
        }

        // The alphabet's 32 characters divide a byte's 256 values, so each is as likely.
        for byte in &mut bytes {
            *byte = TOKEN_ALPHABET[usize::from(*byte % 32)];
        }
        Ok(Self { bytes })
    }

    /// The token `text` spells, or `None` where it is no token.
    fn parse(text: &str) -> Option<Self> {
        let bytes: [u8; TOKEN_LENGTH] = text.as_bytes().try_into().ok()?;
        if !bytes.iter().all(|byte| TOKEN_ALPHABET.contains(byte)) {
            return None;
        }

        Some(Self { bytes })
    }

    fn as_str(&self) -> &str {
        match std::str::from_utf8(&self.bytes) {
            Ok(token) => token,
            Err(error) => unreachable!("a token holds only its alphabet: {error}"),
        }
    }
}

/// What a writer continues from the ring before it.
pub(super) struct Previous {
    /// The token of the previous ring's name, or a new one where the lock file names none.
    pub(super) token: Token,
    /// The previous ring's generation, or 0 where the lock file names none.
    pub(super) generation: u64,
    /// The previous ring's newest publication, or 0 where it is gone.
    pub(super) publication_newest: u64,
    /// The previous ring's newest event, or 0 where it is gone.
    pub(super) event_newest: u64,
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
    let (token, generation) = if let Some(named) = create_previous_parse(prefix, name) {
        named
    } else {
        if !name.is_empty() {
            eprintln!("ring: numbering from 1, since the lock file names {name:?}");
        }
        (Token::random()?, 0)
    };
    if generation == 0 {
        return Ok(Previous {
            token,
            generation,
            publication_newest: 0,
            event_newest: 0,
        });
    }

    // A restart after the Mac restarted finds no ring, which is no fault.
    let (publication_newest, event_newest) = match open_ring(name) {
        Ok((mapping, _)) => {
            let words = mapping.words();
            (
                words[HEADER_PUBLICATION_NEWEST].load(Ordering::Relaxed),
                words[HEADER_EVENT_NEWEST].load(Ordering::Relaxed),
            )
        }
        Err(error) if error.raw_os_error() == Some(ENOENT) => (0, 0),
        Err(error) => {
            eprintln!("ring: numbering from 1, since the ring {name} is unreadable: {error}");
            (0, 0)
        }
    };
    Ok(Previous {
        token,
        generation,
        publication_newest: create_previous_continued(publication_newest, name),
        event_newest: create_previous_continued(event_newest, name),
    })
}

/// The token and the generation of the ring name `name`, which a writer wrote under
/// `prefix`, or `None` where another program wrote something else.
fn create_previous_parse(prefix: &str, name: &str) -> Option<(Token, u64)> {
    let (token, generation) = name.strip_prefix(prefix)?.split_once('.')?;
    let token = Token::parse(token)?;
    let generation = generation.parse().ok()?;
    Some((token, generation))
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

/// The names of a place's rings: the prefix, the place's token, and a generation.
#[derive(Clone, Copy)]
pub(super) struct Naming<'prefix> {
    pub(super) prefix: &'prefix str,
    pub(super) token: Token,
}

/// Makes the object of a ring of `layout` under the first generation past `previous` whose
/// name no object holds.
pub(super) fn create_object(naming: Naming, previous: u64, layout: &Layout) -> io::Result<Created> {
    for generation in previous + 1..=previous + GENERATION_TRIES {
        let name = ring_name(naming, generation)?;
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

fn ring_name(naming: Naming, generation: u64) -> io::Result<String> {
    let name = format!("{}{}.{generation}", naming.prefix, naming.token.as_str());
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
pub(super) fn create_unlink_older(naming: Naming, previous: u64) {
    let first = previous.saturating_sub(GENERATION_TRIES + 1).max(1);
    let mut kept = false;
    for generation in (first..=previous).rev() {
        let name = match ring_name(naming, generation) {
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
            let first = words[HEADER_PUBLICATION_FIRST].load(Ordering::Relaxed);
            let newest = words[HEADER_PUBLICATION_NEWEST].load(Ordering::Relaxed);
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

    let size_bytes = open_ring_check(&File::from(object.try_clone()?))?;

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
    // Every name comes from a lock file read, or from ring_name, and both refuse a longer one.
    let length = name.len();
    assert!(length <= PSHMNAMLEN);
    buffer[..length].copy_from_slice(name.as_bytes());
    buffer[length] = 0;
    CStr::from_bytes_with_nul(&buffer[..=length]).map_err(io::Error::other)
}

/// The size of the object `object`, or an error where this user does not own it or anyone
/// may write it.
fn open_ring_check(object: &File) -> io::Result<usize> {
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

/// The place of the ring `name` of this user: its lock file in a directory of this user's
/// Application Support that no other user may enter, and which macOS never cleans as it
/// cleans the temporary directories. Its rings' names hold a token only that lock file
/// holds, since every user shares the names of shared-memory objects.
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

    let Some(home) = std::env::home_dir() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "this user has no home directory",
        ));
    };
    let directory = home.join("Library/Application Support/Flightdeck");
    place_directory(&directory)?;

    Ok(super::Place {
        lock_path: directory.join(format!("{name}.lock")),
        name_prefix: RING_NAME_PREFIX.to_owned(),
    })
}

/// Makes `directory` for this user alone where it is missing, or an error where it is no
/// directory, another user's, or one another user may enter, since its lock files name the
/// rings.
fn place_directory(directory: &Path) -> io::Result<()> {
    let made = DirBuilder::new()
        .recursive(false)
        .mode(DIRECTORY_MODE)
        .create(directory);
    if let Err(error) = made {
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }

    let metadata = std::fs::metadata(directory)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is no directory", directory.display()),
        ));
    }

    if metadata.uid() != unsafe { getuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} belongs to uid {}", directory.display(), metadata.uid()),
        ));
    }

    if metadata.mode() & 0o777 != DIRECTORY_MODE {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} has mode {:o}, where the rings need {DIRECTORY_MODE:o}",
                directory.display(),
                metadata.mode() & 0o777
            ),
        ));
    }

    Ok(())
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

    #[test]
    fn makes_a_missing_place_directory_for_its_user_alone() {
        // Goal: the directory of a user's lock files, where it is missing, is made so that no
        // other user may enter it.
        // Method: ask for a directory of this test's own that does not exist, and read its
        // mode.
        let directory =
            std::env::temp_dir().join(format!("flightdeck-made-{}", std::process::id()));

        let placed = place_directory(&directory);

        let mode = std::fs::metadata(&directory).unwrap().mode() & 0o777;
        std::fs::remove_dir(&directory).unwrap();
        placed.unwrap();
        assert_eq!(DIRECTORY_MODE, mode);
    }

    #[test]
    fn refuses_a_place_directory_another_user_may_enter() {
        // Goal: lock files never go where another user could read the ring names they hold.
        // Method: make a directory of this test's own that every user may enter, and ask for
        // it.
        let directory =
            std::env::temp_dir().join(format!("flightdeck-open-{}", std::process::id()));
        DirBuilder::new().mode(0o755).create(&directory).unwrap();

        let placed = place_directory(&directory);

        std::fs::remove_dir(&directory).unwrap();
        assert_eq!(io::ErrorKind::PermissionDenied, placed.unwrap_err().kind());
    }
}
