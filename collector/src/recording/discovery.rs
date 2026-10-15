//! Resumable bounded directory enumeration and immutable window time-bound filtering.

use std::ffi::{c_int, c_void};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

/// One bounded kernel directory page, aligned for its attribute records.
pub const DIRECTORY_BUFFER_WORDS: usize = 8192;
const DIRECTORY_BUFFER_BYTES: usize = DIRECTORY_BUFFER_WORDS * size_of::<u64>();
const DIRECTORY_ENTRIES_MAX: u32 = 2048;

/// Owns the kernel enumeration position and borrows fixed caller storage for its pages.
pub struct DirectoryCursor<'buffer> {
    directory: File,
    buffer: &'buffer mut [u64; DIRECTORY_BUFFER_WORDS],
    remaining: u32,
    position: usize,
    complete: bool,
}

/// Only `Complete` establishes EOF. Stopping on a name leaves enumeration incomplete.
#[derive(Debug, PartialEq, Eq)]
pub enum DirectoryEntry<'name> {
    Name(&'name str),
    Complete,
}

#[repr(C)]
struct Attrlist {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

const _: () = assert!(size_of::<Attrlist>() == 24);
const _: () = assert!(DIRECTORY_BUFFER_BYTES == 65_536);

unsafe extern "C" {
    fn getattrlistbulk(
        directory: c_int,
        attributes: *mut Attrlist,
        buffer: *mut c_void,
        size: usize,
        options: u64,
    ) -> c_int;
}

impl<'buffer> DirectoryCursor<'buffer> {
    pub fn open(
        path: &Path,
        buffer: &'buffer mut [u64; DIRECTORY_BUFFER_WORDS],
    ) -> io::Result<Self> {
        let directory = super::private::open_directory(path)?;

        Ok(Self {
            directory,
            buffer,
            remaining: 0,
            position: 0,
            complete: false,
        })
    }

    #[must_use]
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Returns one borrowed name. Refills at most one 64 KiB page per call.
    pub fn next_name(&mut self) -> io::Result<DirectoryEntry<'_>> {
        if self.complete {
            return Ok(DirectoryEntry::Complete);
        }
        if self.remaining == 0 {
            self.remaining = self.read_page()?;
            self.position = 0;
            if self.remaining == 0 {
                self.complete = true;
                return Ok(DirectoryEntry::Complete);
            }
        }

        let bytes = unsafe {
            std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), DIRECTORY_BUFFER_BYTES)
        };
        let length = usize::try_from(word(bytes, self.position)?).map_err(|_| invalid())?;
        let end = self.position.checked_add(length).ok_or_else(invalid)?;
        let record = bytes.get(self.position..end).ok_or_else(invalid)?;
        let name = record_name(record)?;
        self.remaining -= 1;
        self.position = end;
        Ok(DirectoryEntry::Name(name))
    }

    fn read_page(&mut self) -> io::Result<u32> {
        let mut attributes = Attrlist {
            bitmapcount: 5,
            reserved: 0,
            commonattr: 0x8000_0001,
            volattr: 0,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        };
        let count = unsafe {
            getattrlistbulk(
                self.directory.as_raw_fd(),
                &raw mut attributes,
                self.buffer.as_mut_ptr().cast(),
                DIRECTORY_BUFFER_BYTES,
                0,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        let count = u32::try_from(count).map_err(|_| invalid())?;
        if count > DIRECTORY_ENTRIES_MAX {
            return Err(invalid());
        }
        Ok(count)
    }
}

fn record_name(record: &[u8]) -> io::Result<&str> {
    if word(record, 4)? & 1 == 0 {
        return Err(invalid());
    }
    let offset = i32::from_ne_bytes(word(record, 24)?.to_ne_bytes());
    let start = usize::try_from(24_i64 + i64::from(offset)).map_err(|_| invalid())?;
    let length = usize::try_from(word(record, 28)?).map_err(|_| invalid())?;
    let end = start.checked_add(length).ok_or_else(invalid)?;
    let bytes = record.get(start..end).ok_or_else(invalid)?;
    if bytes.last() != Some(&0) {
        return Err(invalid());
    }
    std::str::from_utf8(&bytes[..bytes.len() - 1]).map_err(|_| invalid())
}

fn word(bytes: &[u8], position: usize) -> io::Result<u32> {
    let end = position.checked_add(4).ok_or_else(invalid)?;
    let bytes = bytes.get(position..end).ok_or_else(invalid)?;
    Ok(u32::from_ne_bytes(bytes.try_into().map_err(|_| invalid())?))
}

/// Parsed bounds filter candidates; callers still validate markers and actual tick times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowName {
    pub window: u64,
    pub bounds: Option<TimeBounds>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBounds {
    pub min: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Match,
    Outside,
    Unknown,
}

impl WindowName {
    pub fn parse(name: &str) -> io::Result<Self> {
        let (window, suffix) = name.split_once('_').ok_or_else(invalid)?;
        let window = decimal(window)?;
        if suffix == "unknown" {
            return Ok(Self {
                window,
                bounds: None,
            });
        }

        let bounds = suffix.strip_prefix('w').ok_or_else(invalid)?;
        let (min, max) = bounds.split_once('-').ok_or_else(invalid)?;
        let bounds = TimeBounds {
            min: decimal(min)?,
            max: decimal(max)?,
        };
        if bounds.min > bounds.max {
            return Err(invalid());
        }

        Ok(Self {
            window,
            bounds: Some(bounds),
        })
    }

    /// Selects candidates overlapping an exact wall-clock instant in nanoseconds.
    #[must_use]
    pub fn select(&self, wall_ns: u64) -> Selection {
        self.select_range(TimeBounds {
            min: wall_ns,
            max: wall_ns,
        })
    }

    /// Selects all candidates overlapping any instant of a Unix wall-clock second.
    pub fn select_second(&self, wall_second: u64) -> io::Result<Selection> {
        let min = wall_second.checked_mul(1_000_000_000).ok_or_else(invalid)?;
        let max = min.checked_add(999_999_999).ok_or_else(invalid)?;
        Ok(self.select_range(TimeBounds { min, max }))
    }

    #[must_use]
    pub fn select_range(&self, requested: TimeBounds) -> Selection {
        assert!(requested.min <= requested.max);

        let Some(bounds) = self.bounds else {
            return Selection::Unknown;
        };
        if bounds.min > requested.max {
            return Selection::Outside;
        }
        if bounds.max < requested.min {
            return Selection::Outside;
        }
        Selection::Match
    }
}

fn decimal(text: &str) -> io::Result<u64> {
    if text.len() != 20 {
        return Err(invalid());
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    text.parse().map_err(|_| invalid())
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Counting;

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    thread_local! {
        static COUNTING: Cell<bool> = const { Cell::new(false) };
        static CALLS: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn allocator_calls_in(work: impl FnOnce()) -> u64 {
        CALLS.set(0);
        COUNTING.set(true);
        work();
        COUNTING.set(false);
        CALLS.get()
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if COUNTING.get() {
                CALLS.set(CALLS.get() + 1);
            }
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            if COUNTING.get() {
                CALLS.set(CALLS.get() + 1);
            }
            unsafe { System.dealloc(pointer, layout) }
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            if COUNTING.get() {
                CALLS.set(CALLS.get() + 1);
            }
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "flightdeck-discovery-{}-{number}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn continues_past_a_week_of_names_without_allocating_or_declaring_early_absence() {
        // Goal: bounded calls resume to actual EOF even beyond a week of minute names.
        // Method: enumerate 10,082 files through caller storage and count allocations.
        let directory = Directory::new();
        for window in 0..10_082_u64 {
            std::fs::write(directory.0.join(format!("{window:020}_unknown")), []).unwrap();
        }
        let mut buffer: Box<[u64; DIRECTORY_BUFFER_WORDS]> = vec![0; DIRECTORY_BUFFER_WORDS]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        let mut cursor = DirectoryCursor::open(&directory.0, &mut buffer).unwrap();
        let mut count = 0;
        let calls = allocator_calls_in(|| {
            for _ in 0..10_083 {
                match cursor.next_name().unwrap() {
                    DirectoryEntry::Name(name) => {
                        assert_eq!(
                            Selection::Unknown,
                            WindowName::parse(name).unwrap().select(0)
                        );
                        count += 1;
                    }
                    DirectoryEntry::Complete => break,
                }
            }
        });

        assert_eq!(10_082, count);
        assert!(cursor.complete());
        assert_eq!(0, calls);
    }

    #[test]
    fn selects_every_window_whose_bounds_overlap_the_requested_second() {
        // Goal: backward clocks can make several windows candidates for one second.
        // Method: classify overlapping measured bounds and a separate unknown window.
        let first =
            WindowName::parse("00000000000000000017_w00000000000000000100-00000000000000000200")
                .unwrap();
        let second =
            WindowName::parse("00000000000000000018_w00000000000000000050-00000000000000000150")
                .unwrap();
        let unknown = WindowName::parse("00000000000000000019_unknown").unwrap();

        assert_eq!(Selection::Match, first.select(150));
        assert_eq!(Selection::Match, second.select(150));
        assert_eq!(Selection::Unknown, unknown.select(150));
        assert_eq!(Selection::Outside, first.select(201));
    }

    #[test]
    fn refuses_malformed_bounds_instead_of_treating_them_as_absent() {
        // Goal: malformed bounds never authorize absence or become invented times.
        // Method: parse reversed bounds and an incompletely written decimal suffix.
        assert!(
            WindowName::parse("00000000000000000017_w00000000000000000200-00000000000000000100",)
                .is_err()
        );
        assert!(WindowName::parse("00000000000000000017_w100-200").is_err());
    }
}
