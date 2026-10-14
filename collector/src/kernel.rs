//! The parts of the kernel's interface that more than one module calls. A module declares
//! the rest of what it calls itself.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::io;
use std::mem::offset_of;

/// Names a Mach port, as `mach_port_t` does.
pub(crate) type MachPort = u32;

/// Holds what a Mach call returns, as `kern_return_t` does.
pub(crate) type KernReturn = c_int;

/// Says a Mach call succeeded.
pub(crate) const KERN_SUCCESS: KernReturn = 0;

/// Says the caller may not act on the process a call named.
pub(crate) const EPERM: c_int = 1;

/// Says no process has the pid a call named.
pub(crate) const ESRCH: c_int = 3;

/// Says a call was given an argument it does not take.
#[cfg(test)]
pub(crate) const EINVAL: c_int = 22;

/// A time in seconds and nanoseconds, as `struct timespec` declares it.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Timespec {
    pub(crate) tv_sec: i64,
    pub(crate) tv_nsec: i64,
}

const _: () = assert!(size_of::<Timespec>() == 16);
const _: () = assert!(offset_of!(Timespec, tv_sec) == 0);
const _: () = assert!(offset_of!(Timespec, tv_nsec) == 8);

unsafe extern "C" {
    static mach_task_self_: MachPort;

    /// Reads the kernel state that `name`, a path of integers, names.
    pub(crate) fn sysctl(
        name: *mut c_int,
        name_count: c_uint,
        old: *mut c_void,
        old_size: *mut usize,
        new: *mut c_void,
        new_size: usize,
    ) -> c_int;

    fn sysctlnametomib(name: *const c_char, path: *mut c_int, path_count: *mut usize) -> c_int;
}

pub(crate) fn task_self() -> MachPort {
    unsafe { mach_task_self_ }
}

/// Checks that a Mach call, which `call` names, returned success.
pub(crate) fn kern_return_check(returned: KernReturn, call: &str) -> io::Result<()> {
    if returned != KERN_SUCCESS {
        return Err(io::Error::other(format!("{call} returned {returned:#x}")));
    }

    Ok(())
}

/// The most levels the path of a kernel state's name has, as `CTL_MAXNAME` in sys/sysctl.h
/// gives it.
const CTL_MAXNAME: usize = 12;

/// Kernel state named by a dotted string, looked up once as the path of integers each read
/// passes.
pub(crate) struct Sysctl {
    path: [c_int; CTL_MAXNAME],
    path_count: usize,
}

impl Sysctl {
    pub(crate) fn named(name: &CStr) -> io::Result<Self> {
        let mut path = [0; CTL_MAXNAME];
        let mut path_count = path.len();
        if unsafe { sysctlnametomib(name.as_ptr(), path.as_mut_ptr(), &raw mut path_count) } != 0 {
            return Err(io::Error::last_os_error());
        }
        named_check(path_count)?;

        let sysctl = Self { path, path_count };
        assert!(sysctl.path_count > 0);
        assert!(sysctl.path_count <= CTL_MAXNAME);
        Ok(sysctl)
    }

    /// Reads the state as a `T`, and reports an error when the kernel writes a value of
    /// another size, since a `T` would then hold bytes that are not the value.
    pub(crate) fn read<T: Copy + Default>(&self) -> io::Result<T> {
        assert!(self.path_count > 0);
        assert!(self.path_count <= CTL_MAXNAME);
        let path_count = match c_uint::try_from(self.path_count) {
            Ok(count) => count,
            Err(error) => unreachable!("{error}"),
        };

        let mut value = T::default();
        let mut size = size_of::<T>();
        let result = unsafe {
            sysctl(
                self.path.as_ptr().cast_mut(),
                path_count,
                (&raw mut value).cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if size != size_of::<T>() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "sysctl wrote {size} bytes into a value of {}",
                    size_of::<T>()
                ),
            ));
        }

        Ok(value)
    }
}

/// Checks that the path `sysctlnametomib` gave has from 1 to `CTL_MAXNAME` levels, since a
/// read passes that many levels of the path.
fn named_check(path_count: usize) -> io::Result<()> {
    if path_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sysctlnametomib gave a path of 0 levels",
        ));
    }
    if path_count > CTL_MAXNAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("sysctlnametomib gave a path of {path_count} levels, past {CTL_MAXNAME}"),
        ));
    }

    Ok(())
}

/// Reads the limit the kernel state `name` gives as a `c_int`, the count of entries a list
/// takes to hold one for each thing the limit allows.
pub(crate) fn read_limit(name: &CStr) -> io::Result<usize> {
    let limit: c_int = Sysctl::named(name)?.read()?;
    let count = read_limit_check(name, limit)?;

    assert!(count > 0);
    Ok(count)
}

/// Checks that `limit`, which the kernel state `name` gave, allows one thing or more, and
/// returns it as a count.
fn read_limit_check(name: &CStr, limit: c_int) -> io::Result<usize> {
    let Ok(count) = usize::try_from(limit) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{name:?} gave a negative limit, {limit}"),
        ));
    };
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{name:?} gave a limit of 0"),
        ));
    }

    assert!(count > 0);
    assert_eq!(Ok(limit), c_int::try_from(count));
    Ok(count)
}

/// Reads the kernel state that `name`, a dotted string, names, as the bytes of a number of
/// `SIZE` bytes, so a test can compare a reading with what the kernel reports.
#[cfg(test)]
pub(crate) fn sysctl_bytes<const SIZE: usize>(name: &CStr) -> [u8; SIZE] {
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            old: *mut c_void,
            old_size: *mut usize,
            new: *mut c_void,
            new_size: usize,
        ) -> c_int;
    }

    let mut bytes = [0; SIZE];
    let mut size = SIZE;
    let result = unsafe {
        sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(0, result, "{name:?}");
    assert_eq!(SIZE, size, "{name:?}");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_a_mach_call_that_succeeded() {
        // Goal: a Mach call that returned success passes the check.
        // Method: check KERN_SUCCESS.
        assert!(kern_return_check(KERN_SUCCESS, "a call").is_ok());
    }

    #[test]
    fn refuses_a_mach_call_that_failed() {
        // Goal: a Mach call that returned anything but success is an error.
        // Method: check 0x10000003, which closing an SMC connection a second time returned on
        // an M5 Pro.
        let checked = kern_return_check(0x1000_0003, "closing a connection");

        assert_eq!(io::ErrorKind::Other, checked.unwrap_err().kind());
    }

    #[test]
    fn takes_a_path_of_one_level() {
        // Goal: a path the kernel gave as one level is taken, the fewest that name a state.
        // Method: check a path of 1 level.
        assert!(named_check(1).is_ok());
    }

    #[test]
    fn takes_a_path_of_the_most_levels_a_name_has() {
        // Goal: a path the kernel gave as CTL_MAXNAME levels is taken, the most the path holds.
        // Method: check a path of CTL_MAXNAME levels.
        assert!(named_check(CTL_MAXNAME).is_ok());
    }

    #[test]
    fn refuses_a_path_of_no_levels() {
        // Goal: a path the kernel gave as 0 levels is an error, since it names no state.
        // Method: check a path of 0 levels.
        let checked = named_check(0);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_path_past_the_most_levels_a_name_has() {
        // Goal: a path the kernel gave as more levels than the path holds is an error, never a
        // read past the path.
        // Method: check a path of CTL_MAXNAME + 1 levels.
        let checked = named_check(CTL_MAXNAME + 1);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn takes_a_limit_of_one_or_more() {
        // Goal: a limit of one or more is the count of entries a list takes.
        // Method: check the 18 this Mac's hw.logicalcpu_max gave.
        assert_eq!(18, read_limit_check(c"hw.logicalcpu_max", 18).unwrap());
    }

    #[test]
    fn refuses_a_limit_of_0() {
        // Goal: a limit of 0 is an error, never a list with room for nothing.
        // Method: check a kern.maxproc of 0, which would allow no process, this one included.
        let checked = read_limit_check(c"kern.maxproc", 0);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_negative_limit() {
        // Goal: a negative limit is an error, since no count is negative.
        // Method: check a kern.maxproc of -1.
        let checked = read_limit_check(c"kern.maxproc", -1);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn reads_kernel_state_of_the_size_asked_for() {
        // Goal: kernel state read as a type of its size gives the value the kernel holds.
        // Method: read hw.logicalcpu as a u32 and compare it with its raw bytes.
        let cpus: u32 = Sysctl::named(c"hw.logicalcpu").unwrap().read().unwrap();

        assert_eq!(u32::from_ne_bytes(sysctl_bytes(c"hw.logicalcpu")), cpus);
    }

    #[test]
    fn refuses_kernel_state_of_another_size() {
        // Goal: kernel state read as a type of another size is an error, never a reading.
        // Method: read hw.logicalcpu, which is 4 bytes, as a u64.
        let read = Sysctl::named(c"hw.logicalcpu").unwrap().read::<u64>();

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }
}
