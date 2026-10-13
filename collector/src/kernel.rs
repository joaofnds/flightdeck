// The parts of the kernel's interface that more than one module calls. A module declares
// the rest of what it calls itself.

use std::ffi::c_int;
use std::mem::offset_of;

/// Names a Mach port, as `mach_port_t` does.
pub(crate) type MachPort = u32;

/// Holds what a Mach call returns, as `kern_return_t` does.
pub(crate) type KernReturn = c_int;

unsafe extern "C" {
    static mach_task_self_: MachPort;
}

pub(crate) fn task_self() -> MachPort {
    unsafe { mach_task_self_ }
}

/// Says no process has the pid a call named.
pub(crate) const ESRCH: c_int = 3;

/// A time in seconds and nanoseconds, as `struct timespec` declares it.
#[repr(C)]
pub(crate) struct Timespec {
    pub(crate) tv_sec: i64,
    pub(crate) tv_nsec: i64,
}

const _: () = assert!(size_of::<Timespec>() == 16);
const _: () = assert!(offset_of!(Timespec, tv_sec) == 0);
const _: () = assert!(offset_of!(Timespec, tv_nsec) == 8);
