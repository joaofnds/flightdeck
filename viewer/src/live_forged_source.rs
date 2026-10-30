//! A copy of a run's ring that tests change, for the states a writer before the event room
//! guard, or another program writing the shared memory, could leave a ring in.
use collector::recording::Registry;
use collector::ring::RingReader;
use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// A copy of a run's ring under a generation of its own, which the registry names in the
/// ring's place, whose log can claim a newer event than the writer finished. That is the log a
/// writer before the event room guard left when the newest events overwrote events of ticks
/// the ring still held, or one that another program writing the shared memory leaves. The copy
/// follows the writer only when `refresh` copies the ring again.
pub struct ForgedSource {
    original: *mut c_void,
    address: *mut c_void,
    bytes: usize,
    name: ForgedName,
    event_newest: Option<u64>,
}

/// The name of the copy, which unlinks the copy when dropped, so a failed step leaves no copy.
struct ForgedName(String);

impl Drop for ForgedName {
    fn drop(&mut self) {
        collector::ring::unlink_generation(&self.0).unwrap();
    }
}

// The header word of the newest event, shared by every admitted ring layout.
const HEADER_EVENT_NEWEST: usize = 7;
const O_RDONLY: c_int = 0x0000;
const O_RDWR: c_int = 0x0002;
const O_CREAT: c_int = 0x0200;
const O_EXCL: c_int = 0x0800;
const PROT_READ: c_int = 0x01;
const PROT_WRITE: c_int = 0x02;
const MAP_SHARED: c_int = 0x0001;
const RING_MODE: c_uint = 0o400;

unsafe extern "C" {
    fn shm_open(name: *const c_char, flags: c_int, ...) -> c_int;
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        descriptor: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
}

impl ForgedSource {
    /// Copies the ring `source`, and names the copy in its place in the registry of `root`.
    pub fn new(root: &Path, source: &str) -> Self {
        let bytes = RingReader::open_generation(source)
            .unwrap()
            .mapped_size_bytes();
        let name = format!("{}.99", source.rsplit_once('.').unwrap().0);
        let original = forged_source_open(source, O_RDONLY);
        let copy = forged_source_open(&name, O_RDWR | O_CREAT | O_EXCL);
        let name = ForgedName(name);
        copy.set_len(u64::try_from(bytes).unwrap()).unwrap();
        let forged = Self {
            original: forged_source_map(&original, bytes, PROT_READ),
            address: forged_source_map(&copy, bytes, PROT_READ | PROT_WRITE),
            bytes,
            name,
            event_newest: None,
        };
        forged.refresh();

        let mut registry = Registry::read(root).unwrap();
        let run = registry
            .runs
            .iter_mut()
            .find(|run| run.ring_name == source)
            .unwrap();
        run.ring_name.clone_from(&forged.name.0);
        registry.write(root).unwrap();
        forged
    }

    /// Copies the ring again, so the copy holds what the writer published since, and claims
    /// the forged newest event again.
    pub fn refresh(&self) {
        let original = unsafe {
            std::slice::from_raw_parts(self.original.cast::<AtomicU64>(), self.bytes / 8)
        };
        for (copy, word) in self.words().iter().zip(original) {
            copy.store(word.load(Ordering::Acquire), Ordering::Release);
        }
        if let Some(newest) = self.event_newest {
            let finished = self.words()[HEADER_EVENT_NEWEST].load(Ordering::Acquire);
            assert!(finished <= newest);
            self.words()[HEADER_EVENT_NEWEST].store(newest, Ordering::Release);
        }
    }

    /// Claims `newest` as the newest event of the log, so the events the log held that are a
    /// whole log before it read as overwritten.
    pub fn claim_event_newest(&mut self, newest: u64) {
        self.event_newest = Some(newest);
        self.refresh();
    }

    fn words(&self) -> &[AtomicU64] {
        unsafe { std::slice::from_raw_parts(self.address.cast::<AtomicU64>(), self.bytes / 8) }
    }
}

impl Drop for ForgedSource {
    fn drop(&mut self) {
        assert_eq!(unsafe { munmap(self.address, self.bytes) }, 0);
        assert_eq!(unsafe { munmap(self.original, self.bytes) }, 0);
    }
}

fn forged_source_open(name: &str, flags: c_int) -> std::fs::File {
    let name = CString::new(name).unwrap();
    let descriptor = unsafe { shm_open(name.as_ptr(), flags, RING_MODE) };
    assert!(descriptor >= 0);
    unsafe { std::fs::File::from_raw_fd(descriptor) }
}

fn forged_source_map(object: &std::fs::File, bytes: usize, protection: c_int) -> *mut c_void {
    let address = unsafe {
        mmap(
            std::ptr::null_mut(),
            bytes,
            protection,
            MAP_SHARED,
            object.as_raw_fd(),
            0,
        )
    };
    assert_ne!(address.addr(), usize::MAX);
    address
}
