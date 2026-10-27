//! Signal delivery requests an orderly stop; the collection loop performs the work.

use std::ffi::c_int;
use std::io;
use std::mem::{offset_of, size_of};
use std::sync::atomic::{AtomicBool, Ordering};

use collector::recording::STOP_LIMIT_SECONDS;

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
            collector::diagnostics::error(format_args!(
                "collector: restoring stop handler failed: {}",
                io::Error::last_os_error()
            ));
        }
    }
}

extern "C" fn request_stop(_signal: c_int) {
    // Only the first request sets the deadline, so further signals cannot push it back.
    if !REQUESTED.swap(true, Ordering::Relaxed) {
        // The alarm's default action ends the collector, since the app that asked does not
        // wait, and a collector stuck in a tick would otherwise hold the recording until the
        // next restart. The alarm cannot fail, and what it returns is the rest of an earlier
        // alarm, which the collector never sets.
        unsafe { alarm(STOP_LIMIT_SECONDS) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn raise(signal: c_int) -> c_int;
    }

    /// Held by each test that installs the handlers, since a signal's action belongs to the
    /// whole process and these tests would otherwise change it under each other.
    static HANDLERS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The function the process now runs on `signal`, or `None` for the default action.
    fn handler_of(signal: c_int) -> Option<extern "C" fn(c_int)> {
        let mut current = std::mem::MaybeUninit::<Sigaction>::uninit();
        assert_eq!(0, unsafe {
            sigaction(signal, std::ptr::null(), current.as_mut_ptr())
        });
        unsafe { current.assume_init().handler.sa_handler }
    }

    #[test]
    fn restores_each_signals_found_action_when_dropped() {
        // Goal: a collector that finishes hands SIGINT and SIGTERM back as it found them, so a
        // later signal ends the process instead of asking a loop that has stopped.
        // Method: give each signal a handler of the test's own, then read each signal's action
        // while installed and after drop. The process's own actions are not a known start,
        // since a shell starts a background job with SIGINT ignored.
        let _handlers = HANDLERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let originals = [replace(SIGINT, &found()), replace(SIGTERM, &found())];

        let stop = Stop::install().unwrap();
        let installed = [handler_of(SIGINT), handler_of(SIGTERM)];
        drop(stop);
        let restored = [handler_of(SIGINT), handler_of(SIGTERM)];
        replace(SIGINT, &originals[0]);
        replace(SIGTERM, &originals[1]);

        let requested: extern "C" fn(c_int) = request_stop;
        for handler in installed {
            assert!(std::ptr::fn_addr_eq(handler.unwrap(), requested));
        }
        let unrequested: extern "C" fn(c_int) = found_handler;
        for handler in restored {
            assert!(std::ptr::fn_addr_eq(handler.unwrap(), unrequested));
        }
    }

    extern "C" fn found_handler(_signal: c_int) {}

    /// The action a signal has before the collector installs its own.
    fn found() -> Sigaction {
        Sigaction {
            handler: SigactionHandler {
                sa_handler: Some(found_handler),
            },
            sa_mask: 0,
            sa_flags: 0,
        }
    }

    /// Gives `signal` the action `action`, and returns the one it had.
    fn replace(signal: c_int, action: &Sigaction) -> Sigaction {
        let mut previous = std::mem::MaybeUninit::<Sigaction>::uninit();
        assert_eq!(0, unsafe {
            sigaction(signal, &raw const *action, previous.as_mut_ptr())
        });
        unsafe { previous.assume_init() }
    }

    #[test]
    fn a_stop_request_sets_the_deadline_once() {
        // Goal: the first stop request starts the deadline that ends a collector stuck in its
        // finish, and a later request does not push the deadline back.
        // Method: raise SIGTERM in this process twice under the installed handler, and read the
        // pending alarm after each, which also cancels it.
        let _handlers = HANDLERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stop = Stop::install().unwrap();

        assert_eq!(0, unsafe { raise(SIGTERM) });
        let first = unsafe { alarm(0) };
        assert_eq!(0, unsafe { raise(SIGTERM) });
        let second = unsafe { alarm(0) };
        drop(stop);

        assert!(Stop::requested());
        assert_eq!(STOP_LIMIT_SECONDS, first);
        assert_eq!(0, second);
    }
}
