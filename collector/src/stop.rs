//! Signal delivery requests an orderly stop; the collection loop performs the work.

use std::ffi::c_int;
use std::io;
use std::mem::{offset_of, size_of};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Stop {
    _interrupt: SignalHandler,
    _terminate: SignalHandler,
}

struct SignalHandler {
    signal: c_int,
    previous: Sigaction,
}

#[repr(C)]
union SigactionHandler {
    sa_handler: Option<extern "C" fn(c_int)>,
    sa_sigaction: Option<extern "C" fn(c_int, *mut std::ffi::c_void, *mut std::ffi::c_void)>,
}

#[repr(C)]
struct Sigaction {
    handler: SigactionHandler,
    sa_mask: u32,
    sa_flags: c_int,
}

const _: () = assert!(size_of::<Sigaction>() == 16);
const _: () = assert!(offset_of!(Sigaction, sa_mask) == 8);
const _: () = assert!(offset_of!(Sigaction, sa_flags) == 12);
const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
const SA_RESTART: c_int = 0x0002;
/// How long the collector may take to finish once asked to stop, after which the alarm's
/// default action ends it. The app that asks does not wait, so without this a collector stuck
/// in a tick would hold the recording until the next restart.
pub const LIMIT_SECONDS: u32 = 150;
static REQUESTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn alarm(seconds: u32) -> u32;
    fn sigaction(signal: c_int, action: *const Sigaction, previous: *mut Sigaction) -> c_int;
}

impl Stop {
    pub fn install() -> io::Result<Self> {
        REQUESTED.store(false, Ordering::Relaxed);
        let interrupt = SignalHandler::install(SIGINT)?;
        let terminate = SignalHandler::install(SIGTERM)?;
        Ok(Self {
            _interrupt: interrupt,
            _terminate: terminate,
        })
    }

    #[must_use]
    pub fn requested() -> bool {
        REQUESTED.load(Ordering::Relaxed)
    }
}

impl SignalHandler {
    fn install(signal: c_int) -> io::Result<Self> {
        let action = Sigaction {
            handler: SigactionHandler {
                sa_handler: Some(request_stop),
            },
            sa_mask: 0,
            sa_flags: SA_RESTART,
        };
        let mut previous = std::mem::MaybeUninit::<Sigaction>::uninit();
        if unsafe { sigaction(signal, &raw const action, previous.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            signal,
            previous: unsafe { previous.assume_init() },
        })
    }
}

impl Drop for SignalHandler {
    fn drop(&mut self) {
        if unsafe { sigaction(self.signal, &raw const self.previous, std::ptr::null_mut()) } != 0 {
            eprintln!(
                "collector: restoring stop handler failed: {}",
                io::Error::last_os_error()
            );
        }
    }
}

extern "C" fn request_stop(_signal: c_int) {
    // Only the first request sets the deadline, so further signals cannot push it back.
    if !REQUESTED.swap(true, Ordering::Relaxed) {
        // The alarm cannot fail, and what it returns is the rest of an earlier alarm, which
        // the collector never sets.
        unsafe { alarm(LIMIT_SECONDS) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn raise(signal: c_int) -> c_int;
    }

    #[test]
    fn a_stop_request_sets_the_deadline_once() {
        // Goal: the first stop request starts the deadline that ends a collector stuck in its
        // finish, and a later request does not push the deadline back.
        // Method: raise SIGTERM in this process twice under the installed handler, and read the
        // pending alarm after each, which also cancels it.
        let stop = Stop::install().unwrap();

        assert_eq!(0, unsafe { raise(SIGTERM) });
        let first = unsafe { alarm(0) };
        assert_eq!(0, unsafe { raise(SIGTERM) });
        let second = unsafe { alarm(0) };
        drop(stop);

        assert!(Stop::requested());
        assert_eq!(LIMIT_SECONDS, first);
        assert_eq!(0, second);
    }
}
