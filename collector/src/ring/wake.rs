//! How a reader sleeps until the writer publishes, and how the writer wakes it, through a word
//! of the ring that both map. macOS's `os_sync` calls wait on the word's address in every
//! process that maps it, read-only mappings included.

use std::ffi::{c_int, c_void};
use std::io;
use std::sync::atomic::AtomicU64;

unsafe extern "C" {
    fn os_sync_wait_on_address_with_timeout(
        address: *mut c_void,
        value: u64,
        size: usize,
        flags: u32,
        clock: u32,
        timeout: u64,
    ) -> c_int;
    fn os_sync_wake_by_address_all(address: *mut c_void, size: usize, flags: u32) -> c_int;
}

/// The flag that makes a wait or a wake reach every process mapping the word, from
/// `os/os_sync_wait_on_address.h`.
const OS_SYNC_SHARED: u32 = 0x1;
/// The clock that measures a wait's timeout, which is in nanoseconds whatever the clock.
const OS_CLOCK_MACH_ABSOLUTE_TIME: u32 = 32;

const ENOENT: i32 = 2;
const EINTR: i32 = 4;
const EFAULT: i32 = 14;
const ETIMEDOUT: i32 = 60;

/// How a wait ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Waited {
    /// A wake came, the word no longer held the value, the timeout ran out, or a signal
    /// interrupted the wait. The caller looks at the word again in each case.
    Ended,
    /// macOS cannot wait on the word, as on a page this process has not touched yet, so the
    /// caller polls instead.
    Unsupported,
}

/// Sleeps until a wake on `word`, while it holds `value`, for at most `timeout_ns`
/// nanoseconds.
pub(super) fn wait(word: &AtomicU64, value: u64, timeout_ns: u64) -> io::Result<Waited> {
    assert!(timeout_ns > 0);

    let returned = unsafe {
        os_sync_wait_on_address_with_timeout(
            word.as_ptr().cast::<c_void>(),
            value,
            size_of::<u64>(),
            OS_SYNC_SHARED,
            OS_CLOCK_MACH_ABSOLUTE_TIME,
            timeout_ns,
        )
    };
    if returned >= 0 {
        return Ok(Waited::Ended);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(ETIMEDOUT | EINTR) => Ok(Waited::Ended),
        Some(EFAULT) => Ok(Waited::Unsupported),
        _ => Err(error),
    }
}

/// Wakes every thread of every process that waits on `word`.
pub(super) fn wake_all(word: &AtomicU64) {
    let returned = unsafe {
        os_sync_wake_by_address_all(
            word.as_ptr().cast::<c_void>(),
            size_of::<u64>(),
            OS_SYNC_SHARED,
        )
    };
    // ENOENT says no thread waited, which is the usual case. Any other error is a call this
    // code got wrong, since the word is aligned, eight bytes and mapped.
    if returned != 0 {
        let error = io::Error::last_os_error();
        assert_eq!(Some(ENOENT), error.raw_os_error(), "{error}");
    }
}
