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
static REQUESTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
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
    REQUESTED.store(true, Ordering::Relaxed);
}
