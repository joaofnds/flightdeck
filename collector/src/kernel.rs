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

/// Kernel state named by a dotted string, looked up once as the path of integers each read
/// passes.
pub(crate) struct Sysctl {
    path: [c_int; 12],
    path_count: c_uint,
}

impl Sysctl {
    pub(crate) fn named(name: &CStr) -> io::Result<Self> {
        let mut path = [0; 12];
        let mut path_count = path.len();
        if unsafe { sysctlnametomib(name.as_ptr(), path.as_mut_ptr(), &raw mut path_count) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            path,
            path_count: c_uint::try_from(path_count).map_err(io::Error::other)?,
        })
    }

    /// Reads the state as a `T`, and reports an error when the kernel writes a value of
    /// another size, since a `T` would then hold bytes that are not the value.
    pub(crate) fn read<T: Copy + Default>(&self) -> io::Result<T> {
        let mut value = T::default();
        let mut size = size_of::<T>();
        let result = unsafe {
            sysctl(
                self.path.as_ptr().cast_mut(),
                self.path_count,
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
