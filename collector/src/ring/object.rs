//! The shared-memory object that holds a ring, and the lock file that names it.

use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::layout::{HEADER_NEWEST_PUBLICATION, HEADER_WORDS, Layout, open_layout};

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

pub(super) fn store(words: &[AtomicU64], index: usize, value: u64) {
    words[index].store(value, Ordering::Relaxed);
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

/// What a writer continues from the ring before it.
pub(super) struct Previous {
    /// The previous ring's generation, or 0 where the lock file names none.
    pub(super) generation: u64,
    /// The previous ring's newest publication, or 0 where it is gone.
    pub(super) newest_publication: u64,
}

/// The ring the lock file `lock` names, of a name that starts with `prefix`.
pub(super) fn create_previous(prefix: &str, lock: &mut File) -> io::Result<Previous> {
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
pub(super) struct Created {
    pub(super) name: String,
    pub(super) generation: u64,
    pub(super) object: OwnedFd,
}

/// Makes the object of a ring of `layout` under the first generation past `previous` whose
/// name no object holds.
pub(super) fn create_object(prefix: &str, previous: u64, layout: Layout) -> io::Result<Created> {
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
pub(super) fn create_unlink_older(prefix: &str, previous: u64) {
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
pub(super) fn open_ring(name: &str) -> io::Result<(Mapping, Layout)> {
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
