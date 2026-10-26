use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::fs::File;
use std::io;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicU64, Ordering};

use collector::recording::RegisteredRun;
use collector::ring::{Found, FoundEvent, IdentityObservation, RingReader, RunIdentity};

pub(super) struct FrozenRing {
    mapping: Mapping,
    object: OwnedObject,
    pub(super) identity: RunIdentity,
    pub(super) first: u64,
    pub(super) newest: u64,
    pub(super) slots: u64,
    pub(super) retained_first: u64,
    event_first: u64,
    event_newest: u64,
    completed: bool,
}

struct Mapping {
    address: *mut c_void,
    bytes: usize,
}

struct Mappings<'a> {
    source: &'a Mapping,
    target: &'a Mapping,
}

struct OwnedObject {
    name: CString,
    file: File,
}

const COPY_ATTEMPTS_MAX: u32 = 8;
// These wire offsets are shared by every admitted ring layout in collector::ring::layout.
const HEADER_PUBLICATION_FIRST: usize = 3;
const HEADER_PUBLICATION_NEWEST: usize = 4;
const HEADER_EVENT_FIRST: usize = 6;
const HEADER_EVENT_NEWEST: usize = 7;
const HEADER_COMPLETED: usize = 29;
const MAPPING_BYTES_MAX: usize = 1024 * 1024 * 1024;
const O_RDONLY: c_int = 0x0000;
const O_RDWR: c_int = 0x0002;
const O_CREAT: c_int = 0x0200;
const O_EXCL: c_int = 0x0800;
const PROT_READ: c_int = 0x01;
const PROT_WRITE: c_int = 0x02;
const MAP_SHARED: c_int = 0x0001;
const RING_MODE: c_uint = 0o400;
const _: () = {
    assert!(size_of::<AtomicU64>() == size_of::<u64>());
    assert!(align_of::<AtomicU64>() == align_of::<u64>());
};

unsafe extern "C" {
    fn shm_open(name: *const c_char, flags: c_int, ...) -> c_int;
    fn shm_unlink(name: *const c_char) -> c_int;
    fn ftruncate(descriptor: c_int, length: i64) -> c_int;
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

impl FrozenRing {
    pub(super) fn capture(run: &RegisteredRun, nonce: u64, index: usize) -> io::Result<Self> {
        let reader = RingReader::open_generation(&run.ring_name)?;
        if reader.identity() != Some(run.identity) {
            return Err(io::Error::other("registered ring identity mismatch"));
        }
        let name = CString::new(run.ring_name.as_str()).map_err(io::Error::other)?;
        let descriptor = unsafe { shm_open(name.as_ptr(), O_RDONLY, 0) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }

        let source_file = unsafe { File::from_raw_fd(descriptor) };
        let bytes = usize::try_from(source_file.metadata()?.len()).map_err(io::Error::other)?;
        if bytes < reader.mapped_size_bytes() {
            return Err(io::Error::other("ring size changed during capture"));
        }
        let source = Mapping::new(&source_file, bytes, PROT_READ)?;
        let name = CString::new(format!("/fr.{nonce:016x}.{index}")).map_err(io::Error::other)?;
        let object = OwnedObject::create(name, bytes)?;
        let mapping = Mapping::new(&object.file, bytes, PROT_READ | PROT_WRITE)?;

        let newest = capture_words(
            &Mappings {
                source: &source,
                target: &mapping,
            },
            &reader,
            object.name()?,
        )?;
        let captured = RingReader::open_generation(object.name()?)?;
        let event_first = mapping.words()[HEADER_EVENT_FIRST].load(Ordering::Acquire);
        let event_newest = mapping.words()[HEADER_EVENT_NEWEST].load(Ordering::Acquire);
        Ok(Self {
            mapping,
            object,
            identity: run.identity,
            first: reader.publication_first(),
            newest,
            slots: reader.shape().slot_count,
            retained_first: captured.next_publication(),
            event_first,
            event_newest,
            completed: captured.completed(),
        })
    }

    pub(super) fn name(&self) -> io::Result<&str> {
        self.object.name()
    }

    pub(super) fn backing_bytes(&self) -> usize {
        self.mapping.bytes
    }

    pub(super) fn publish(&self, newest: u64) {
        assert!(newest < self.newest + 1);
        assert!(newest >= self.first - 1);
        self.mapping.words()[HEADER_PUBLICATION_NEWEST].store(newest, Ordering::Release);
    }

    pub(super) fn replay_publish(&self, newest: u64, event_newest: u64) {
        self.mapping.words()[HEADER_EVENT_NEWEST].store(event_newest, Ordering::Release);
        self.publish(newest);
    }

    pub(super) fn replay_start(&self, event_first: u64) -> io::Result<()> {
        if self.retained_first < self.first {
            return Err(io::Error::other(
                "retained publications precede source numbering",
            ));
        }

        if event_first < self.event_first {
            return Err(io::Error::other("retained events precede source numbering"));
        }

        self.mapping.words()[HEADER_PUBLICATION_FIRST]
            .store(self.retained_first, Ordering::Release);
        self.mapping.words()[HEADER_EVENT_FIRST].store(event_first, Ordering::Release);
        Ok(())
    }

    pub(super) fn complete(&self, completed: bool) {
        self.mapping.words()[HEADER_COMPLETED].store(u64::from(completed), Ordering::Release);
    }

    pub(super) fn restore(&self) {
        self.mapping.words()[HEADER_PUBLICATION_FIRST].store(self.first, Ordering::Release);
        self.mapping.words()[HEADER_EVENT_FIRST].store(self.event_first, Ordering::Release);
        self.replay_publish(self.newest, self.event_newest);
        self.complete(self.completed);
    }

    pub(super) fn export(&self, path: &std::path::Path) -> io::Result<()> {
        let file = collector::recording::private::open_private_file(path, true)?;
        let mut output = io::BufWriter::with_capacity(65_536, file);
        assert!(self.mapping.bytes <= MAPPING_BYTES_MAX);
        for word in self.mapping.words() {
            output.write_all(&word.load(Ordering::Acquire).to_ne_bytes())?;
        }
        output.flush()?;
        output.get_ref().sync_all()
    }
}

fn capture_words(mappings: &Mappings<'_>, reader: &RingReader, name: &str) -> io::Result<u64> {
    assert_eq!(mappings.source.bytes, mappings.target.bytes);
    for _attempt in 0..COPY_ATTEMPTS_MAX {
        let newest = reader.publication_newest()?;
        capture_words_copy(mappings);
        if reader.publication_newest()? != newest {
            continue;
        }
        if !capture_words_equal(mappings) {
            continue;
        }
        if reader.publication_newest()? != newest {
            continue;
        }
        if mappings.target.words()[HEADER_PUBLICATION_NEWEST].load(Ordering::Acquire) != newest {
            continue;
        }
        if capture_words_validate(name, reader)? {
            return Ok(newest);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "ring did not yield a stable complete snapshot within eight attempts",
    ))
}

fn capture_words_copy(mappings: &Mappings<'_>) {
    let source = mappings.source.words();
    let target = mappings.target.words();
    assert_eq!(source.len(), target.len());
    assert!(source.len() <= MAPPING_BYTES_MAX / size_of::<u64>());
    for (source_word, target_word) in source.iter().zip(target) {
        target_word.store(source_word.load(Ordering::Acquire), Ordering::Relaxed);
    }
}

fn capture_words_equal(mappings: &Mappings<'_>) -> bool {
    let source = mappings.source.words();
    let target = mappings.target.words();
    assert_eq!(source.len(), target.len());
    assert!(source.len() <= MAPPING_BYTES_MAX / size_of::<u64>());
    source.iter().zip(target).all(|(source_word, target_word)| {
        source_word.load(Ordering::Acquire) == target_word.load(Ordering::Relaxed)
    })
}

fn capture_words_validate(name: &str, source: &RingReader) -> io::Result<bool> {
    let mut reader = RingReader::open_generation(name)?;
    if reader.identity() != source.identity() {
        return Err(io::Error::other("ring identity changed during capture"));
    }
    let shape = reader.shape();
    let mut tick = persistence::tick_buffer(&shape).map_err(io::Error::other)?;
    let capacity = usize::try_from(shape.process_capacity).map_err(io::Error::other)?;
    let mut identities = Vec::with_capacity(capacity);
    for _publication in 0..shape.slot_count {
        let found = if reader.dictionary_capacity().is_some() {
            reader.read_identity_into(&mut tick, &mut identities)?
        } else {
            reader.read_into(&mut tick)?
        };
        match found {
            Found::Publication { .. } => {}
            Found::Missed { .. } => return Ok(false),
            Found::Nothing => break,
        }
    }
    capture_words_validate_events(&mut reader)
}

fn capture_words_validate_events(reader: &mut RingReader) -> io::Result<bool> {
    let mut identity = IdentityObservation::EMPTY;
    for _event in 0..reader.shape().event_capacity {
        let found = if reader.dictionary_capacity().is_some() {
            reader.read_event_identity(&mut identity)?
        } else {
            reader.read_event()?
        };
        match found {
            FoundEvent::Event { .. } => {}
            FoundEvent::Missed { .. } => return Ok(false),
            FoundEvent::Nothing => return Ok(true),
        }
    }
    Ok(true)
}

impl Mapping {
    fn new(file: &File, bytes: usize, protection: c_int) -> io::Result<Self> {
        if !(1..=MAPPING_BYTES_MAX).contains(&bytes) {
            return Err(io::Error::other("ring mapping size bound"));
        }
        if !bytes.is_multiple_of(size_of::<u64>()) {
            return Err(io::Error::other("ring mapping word alignment"));
        }

        let address = unsafe {
            mmap(
                std::ptr::null_mut(),
                bytes,
                protection,
                MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if address.addr() == usize::MAX {
            return Err(io::Error::last_os_error());
        }
        let mapping = Self { address, bytes };
        if mapping.address.is_null() {
            return Err(io::Error::other("null mmap"));
        }
        Ok(mapping)
    }

    fn words(&self) -> &[AtomicU64] {
        unsafe {
            std::slice::from_raw_parts(
                self.address.cast::<AtomicU64>(),
                self.bytes / size_of::<u64>(),
            )
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if unsafe { munmap(self.address, self.bytes) } != 0 {
            eprintln!(
                "resume fixture: munmap failed: {}",
                io::Error::last_os_error()
            );
        }
    }
}

impl OwnedObject {
    fn create(name: CString, bytes: usize) -> io::Result<Self> {
        let descriptor = unsafe { shm_open(name.as_ptr(), O_RDWR | O_CREAT | O_EXCL, RING_MODE) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }

        let file = unsafe { File::from_raw_fd(descriptor) };
        let object = Self { name, file };
        let length = i64::try_from(bytes).map_err(io::Error::other)?;
        if unsafe { ftruncate(object.file.as_raw_fd(), length) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(object)
    }

    fn name(&self) -> io::Result<&str> {
        self.name.to_str().map_err(io::Error::other)
    }
}

impl Drop for OwnedObject {
    fn drop(&mut self) {
        if unsafe { shm_unlink(self.name.as_ptr()) } != 0 {
            eprintln!(
                "resume fixture: shm_unlink failed: {}",
                io::Error::last_os_error()
            );
        }
    }
}
