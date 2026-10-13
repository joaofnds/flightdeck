// The parts of the kernel's interface that more than one module calls. A module declares
// the rest of what it calls itself.

use std::ffi::c_int;

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
