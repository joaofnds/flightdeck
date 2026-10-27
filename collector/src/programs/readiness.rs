/// A fresh named right is only a conditional readiness witness, never a task snapshot.
pub struct Readiness {
    name: u32,
    release: fn(u32) -> i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadinessError {
    NotRead,
    Kernel { status: i32, name: u32 },
    Null,
    Dead,
}

impl Drop for Readiness {
    fn drop(&mut self) {
        let status = (self.release)(self.name);
        if status != 0 {
            crate::diagnostics::error(format_args!(
                "mach_port_deallocate readiness name={} status={status}",
                self.name
            ));
        }
    }
}

/// Acquires a new right for this attempt; no PID-only gate result may be reused.
pub fn acquire_readiness(pid: i32, output: &mut Option<Readiness>) -> Result<(), ReadinessError> {
    *output = None;
    let mut name = 0;
    let status = unsafe { task_name_for_pid(crate::kernel::task_self(), pid, &raw mut name) };

    acquire_readiness_result(status, name, output, release_readiness)
}

pub(super) fn acquire_readiness_result(
    status: i32,
    name: u32,
    output: &mut Option<Readiness>,
    release: fn(u32) -> i32,
) -> Result<(), ReadinessError> {
    *output = None;
    if name != 0 {
        if name != u32::MAX {
            *output = Some(Readiness { name, release });
        }
    }

    if status != 0 {
        *output = None;
        return Err(ReadinessError::Kernel { status, name });
    }

    if name == 0 {
        return Err(ReadinessError::Null);
    }

    if name == u32::MAX {
        return Err(ReadinessError::Dead);
    }

    Ok(())
}

fn release_readiness(name: u32) -> i32 {
    unsafe { mach_port_deallocate(crate::kernel::task_self(), name) }
}

unsafe extern "C" {
    fn task_name_for_pid(target_tport: u32, pid: i32, task_name: *mut u32) -> i32;
    fn mach_port_deallocate(task: u32, name: u32) -> i32;
}
