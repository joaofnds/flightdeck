unsafe extern "C" {
    static mach_task_self_: libc::mach_port_t;
}

pub(crate) fn task_self() -> libc::mach_port_t {
    unsafe { mach_task_self_ }
}
