use std::ffi::{CString, c_char, c_int, c_void};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicU64, Ordering};

struct Mapping {
    address: *mut c_void,
    bytes: usize,
}

unsafe extern "C" {
    fn shm_open(name: *const c_char, flags: c_int, ...) -> c_int;
    fn mmap(
        address: *mut c_void,
        bytes: usize,
        protection: c_int,
        flags: c_int,
        descriptor: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, bytes: usize) -> c_int;
}

pub(super) fn withhold(source: &str, target: &str, expected: u64) {
    let reader = collector::ring::RingReader::open_generation(source).unwrap();
    assert!(reader.completed());
    assert_eq!(reader.publication_newest().unwrap(), expected);
    let bytes = reader.mapped_size_bytes();
    assert!(bytes <= 1024 * 1024 * 1024);
    drop(reader);
    let source = CString::new(source).unwrap();
    let descriptor = unsafe { shm_open(source.as_ptr(), 0) };
    assert!(descriptor >= 0);
    let file = unsafe { File::from_raw_fd(descriptor) };
    let original = Mapping::new(&file, bytes, 1);
    let target = CString::new(target).unwrap();
    let descriptor = unsafe { shm_open(target.as_ptr(), 0x602, 0o400) };
    assert!(descriptor >= 0);
    let writable = unsafe { File::from_raw_fd(descriptor) };
    writable.set_len(collector::u64_from_usize(bytes)).unwrap();
    let copied = Mapping::new(&writable, bytes, 3);
    // Finished fixture has no concurrent writer; source and exclusive new object are disjoint.
    unsafe {
        std::ptr::copy_nonoverlapping(
            original.address.cast::<u8>(),
            copied.address.cast::<u8>(),
            bytes,
        );
    }
    let publication = unsafe { &*copied.address.cast::<AtomicU64>().add(4) };
    publication.store(0, Ordering::Release);
}

impl Mapping {
    fn new(file: &File, bytes: usize, protection: c_int) -> Self {
        let address = unsafe {
            mmap(
                std::ptr::null_mut(),
                bytes,
                protection,
                1,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(address.addr(), usize::MAX);
        Self { address, bytes }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        assert_eq!(unsafe { munmap(self.address, self.bytes) }, 0);
    }
}
