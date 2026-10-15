//! How a reader sleeps until the writer publishes, and how the writer wakes it, through a word
//! of the ring that both map. macOS's `os_sync` calls wait on the word's address in every
//! process that maps it, read-only mappings included.

use std::ffi::{c_int, c_void};
use std::io;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

#[cfg(test)]
use crate::kernel::EDOM;
use crate::kernel::{EFAULT, EINTR, ENOENT, ETIMEDOUT};

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

/// The flags that make a wait and a wake reach every process mapping the word, from
/// `os/os_sync_wait_on_address.h`.
const OS_SYNC_WAIT_ON_ADDRESS_SHARED: u32 = 0x1;
const OS_SYNC_WAKE_BY_ADDRESS_SHARED: u32 = 0x1;
/// The clock that measures a wait's timeout, which is in nanoseconds whatever the clock.
const OS_CLOCK_MACH_ABSOLUTE_TIME: u32 = 32;

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

/// Sleeps until a wake on `word`, while it holds `value`, for at most `timeout`.
pub(super) fn wait(word: &AtomicU64, value: u64, timeout: Duration) -> io::Result<Waited> {
    assert!(!timeout.is_zero());
    let timeout_ns = u64::try_from(timeout.as_nanos()).map_err(io::Error::other)?;

    let returned = unsafe {
        os_sync_wait_on_address_with_timeout(
            word.as_ptr().cast::<c_void>(),
            value,
            size_of::<u64>(),
            OS_SYNC_WAIT_ON_ADDRESS_SHARED,
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

/// Wakes every thread of every process that waits on `word`, and returns the errno of a wake
/// macOS refused, as one a reader that waits with another size gets. ENOENT says no thread
/// waited, which is the usual case and no error. The errno leaves without an `io::Error`,
/// whose message allocates, since the writer reports it and goes on.
pub(super) fn wake_all(word: &AtomicU64) -> Result<(), i32> {
    let returned = unsafe {
        os_sync_wake_by_address_all(
            word.as_ptr().cast::<c_void>(),
            size_of::<u64>(),
            OS_SYNC_WAKE_BY_ADDRESS_SHARED,
        )
    };
    if returned == 0 {
        return Ok(());
    }

    match io::Error::last_os_error().raw_os_error() {
        Some(ENOENT) => Ok(()),
        Some(errno) => Err(errno),
        None => unreachable!("last_os_error always carries an errno"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_wake_that_macos_refuses_is_an_error_and_not_a_crash() {
        // Goal: a reader that waits on the word with another size, which macOS then refuses
        // to wake, never stops the writer that wakes it.
        // Method: wait on the word with four bytes on another thread, and wake it with eight
        // until macOS refuses, within a second.
        let word = AtomicU64::new(0);
        std::thread::scope(|scope| {
            scope.spawn(|| unsafe {
                os_sync_wait_on_address_with_timeout(
                    word.as_ptr().cast::<c_void>(),
                    0,
                    size_of::<u32>(),
                    OS_SYNC_WAIT_ON_ADDRESS_SHARED,
                    OS_CLOCK_MACH_ABSOLUTE_TIME,
                    1_100_000_000,
                )
            });
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut woken = wake_all(&word);
            while woken.is_ok() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
                woken = wake_all(&word);
            }

            assert_eq!(Err(EDOM), woken);
        });
    }
}
