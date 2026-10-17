use std::ffi::{c_int, c_void};
use std::mem::{align_of, offset_of, size_of};

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(clippy::struct_field_names)] // Mirrors sys/proc_info.h verbatim for ABI review.
pub(super) struct ProcUniqidentifierinfo {
    pub(super) p_uuid: [u8; 16],
    pub(super) p_uniqueid: u64,
    pub(super) p_puniqueid: u64,
    pub(super) p_idversion: i32,
    pub(super) p_orig_ppidversion: i32,
    pub(super) p_reserve2: u64,
    pub(super) p_reserve3: u64,
}

impl ProcUniqidentifierinfo {
    pub(super) const EMPTY: Self = Self {
        p_uuid: [0; 16],
        p_uniqueid: 0,
        p_puniqueid: 0,
        p_idversion: 0,
        p_orig_ppidversion: 0,
        p_reserve2: 0,
        p_reserve3: 0,
    };
}

#[derive(Clone, Copy)]
pub(super) struct CallResult {
    pub(super) returned: i32,
    pub(super) errno: i32,
}

const _: () = {
    assert!(size_of::<ProcUniqidentifierinfo>() == 56);
    assert!(align_of::<ProcUniqidentifierinfo>() == 8);
    assert!(offset_of!(ProcUniqidentifierinfo, p_uuid) == 0);
    assert!(offset_of!(ProcUniqidentifierinfo, p_uniqueid) == 16);
    assert!(offset_of!(ProcUniqidentifierinfo, p_puniqueid) == 24);
    assert!(offset_of!(ProcUniqidentifierinfo, p_idversion) == 32);
    assert!(offset_of!(ProcUniqidentifierinfo, p_orig_ppidversion) == 36);
    assert!(offset_of!(ProcUniqidentifierinfo, p_reserve2) == 40);
    assert!(offset_of!(ProcUniqidentifierinfo, p_reserve3) == 48);
};

pub(super) fn read_creation_result(
    raw: &ProcUniqidentifierinfo,
    call: CallResult,
    output: &mut Result<crate::ring::ProcessCreation, super::CreationError>,
) {
    *output = match call.returned {
        56 => Ok(crate::ring::ProcessCreation {
            unique_id: raw.p_uniqueid,
            pid_version: raw.p_idversion,
        }),
        0 => Err(match call.errno {
            1 | 13 => super::CreationError::Refused(call.errno),
            3 => super::CreationError::Gone(call.errno),
            22 => super::CreationError::Unsupported(call.errno),
            _ => super::CreationError::Syscall(call.errno),
        }),
        _ => Err(super::CreationError::Size(call.returned)),
    };
}

const PROC_PIDUNIQIDENTIFIERINFO: c_int = 17;

unsafe extern "C" {
    fn __error() -> *mut c_int;
    fn proc_pidinfo(
        pid: c_int,
        flavor: c_int,
        argument: u64,
        buffer: *mut c_void,
        buffer_size: c_int,
    ) -> c_int;
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffer_size: u32) -> c_int;
}

/// The wrapper returns a byte length excluding NUL, independently of rusage conventions.
pub fn read_path(pid: i32, scratch: &mut [u8; 4096]) -> Result<usize, super::PathError> {
    scratch.fill(0xff);
    let call = unsafe {
        *__error() = 0;
        let returned = proc_pidpath(pid, scratch.as_mut_ptr().cast(), 4096);
        CallResult {
            returned,
            errno: *__error(),
        }
    };
    super::path::read_path_result(scratch, call)
}

/// Flavor17's nonzero argument requests exited-unreaped processes as well.
pub fn read_creation(
    pid: i32,
    include_exited: bool,
    output: &mut Result<crate::ring::ProcessCreation, super::CreationError>,
) {
    let mut raw = ProcUniqidentifierinfo::EMPTY;
    let call = unsafe {
        *__error() = 0;
        let returned = proc_pidinfo(
            pid,
            PROC_PIDUNIQIDENTIFIERINFO,
            u64::from(include_exited),
            (&raw mut raw).cast(),
            56,
        );
        CallResult {
            returned,
            errno: *__error(),
        }
    };
    read_creation_result(&raw, call, output);
}
