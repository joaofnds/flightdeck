use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicU64, Ordering};

/// The header word that holds the newest event's number, in every ring layout.
pub(super) const HEADER_EVENT_NEWEST: usize = 7;

/// A copy of a ring under a generation of its own, with one word changed, as another program
/// writing the shared memory, or a writer before the event room guard, could leave it.
pub(super) struct WritableSource {
    address: *mut std::ffi::c_void,
    original: *mut std::ffi::c_void,
    bytes: usize,
    pub(super) name: String,
    fault: ForgedWord,
}

/// The word at `index` changed to `value` while the word at `watched` holds `expected`, so the
/// copy follows the real writer once it moves past the state the change forges.
pub(super) struct ForgedWord {
    pub(super) watched: usize,
    pub(super) expected: u64,
    pub(super) index: usize,
    pub(super) value: u64,
}

impl WritableSource {
    pub(super) fn new(name: &str, bytes: usize, fault: ForgedWord) -> Self {
        let target = format!("{}.99", name.rsplit_once('.').unwrap().0);
        let name = std::ffi::CString::new(name).unwrap();
        let descriptor = unsafe { shm_open(name.as_ptr(), 0) };
        assert!(descriptor >= 0);
        let object = unsafe { std::fs::File::from_raw_fd(descriptor) };
        let original = unsafe { mmap(std::ptr::null_mut(), bytes, 1, 1, object.as_raw_fd(), 0) };
        assert_ne!(original.addr(), usize::MAX);
        let target_name = std::ffi::CString::new(target.clone()).unwrap();
        let descriptor = unsafe { shm_open(target_name.as_ptr(), 0x602, 0o400) };
        assert!(descriptor >= 0);
        let writable = unsafe { std::fs::File::from_raw_fd(descriptor) };
        assert_eq!(
            unsafe { ftruncate(descriptor, i64::try_from(bytes).unwrap()) },
            0
        );
        let address = unsafe { mmap(std::ptr::null_mut(), bytes, 3, 1, writable.as_raw_fd(), 0) };
        assert_ne!(address.addr(), usize::MAX);
        let snapshot = Self {
            address,
            original,
            bytes,
            name: target,
            fault,
        };
        snapshot.refresh();
        snapshot
    }

    pub(super) fn refresh(&self) {
        // A forged state follows the real writer until the watched word moves on. There is no
        // concurrent writer during this bounded copy or the following worker pass.
        let original = unsafe {
            std::slice::from_raw_parts(self.original.cast::<AtomicU64>(), self.bytes / 8)
        };
        for (target, source) in self.words().iter().zip(original) {
            target.store(source.load(Ordering::Acquire), Ordering::Release);
        }
        if self.words()[self.fault.watched].load(Ordering::Acquire) == self.fault.expected {
            self.words()[self.fault.index].store(self.fault.value, Ordering::Release);
        }
    }

    fn words(&self) -> &[AtomicU64] {
        unsafe { std::slice::from_raw_parts(self.address.cast::<AtomicU64>(), self.bytes / 8) }
    }
}

impl Drop for WritableSource {
    fn drop(&mut self) {
        assert_eq!(unsafe { munmap(self.address, self.bytes) }, 0);
        assert_eq!(unsafe { munmap(self.original, self.bytes) }, 0);
        collector::ring::unlink_generation(&self.name).unwrap();
    }
}

unsafe extern "C" {
    fn shm_open(name: *const std::ffi::c_char, flags: std::ffi::c_int, ...) -> std::ffi::c_int;
    fn ftruncate(descriptor: std::ffi::c_int, length: i64) -> std::ffi::c_int;
    fn mmap(
        address: *mut std::ffi::c_void,
        length: usize,
        protection: std::ffi::c_int,
        flags: std::ffi::c_int,
        descriptor: std::ffi::c_int,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    fn munmap(address: *mut std::ffi::c_void, length: usize) -> std::ffi::c_int;
}
