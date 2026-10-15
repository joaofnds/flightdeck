//! The SDK's `rusage_info_v6` ABI, pinned to the fields this observer reads.
use std::ffi::{c_int, c_void};
use std::io;
use std::mem::offset_of;

use super::Footprint;

pub(super) fn immediate_exit(status: c_int) -> ! {
    // No C/C++ exit handler may race the engine executing on the main thread.
    unsafe { _exit(status) }
}

pub(super) fn producer_owner_changed(pid: i32) -> io::Result<bool> {
    let mut record = ProcBsdShortInfo::default();
    let size = c_int::try_from(size_of::<ProcBsdShortInfo>()).map_err(io::Error::other)?;
    let count = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDT_SHORTBSDINFO,
            0,
            std::ptr::from_mut(&mut record).cast(),
            size,
        )
    };
    if count != size {
        return Err(io::Error::last_os_error());
    }
    if record.pid != u32::try_from(pid).map_err(io::Error::other)? {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    // The original collector does not change credentials. Another real owner proves reuse.
    Ok(record.ruid != unsafe { getuid() })
}

#[repr(C)]
#[derive(Default)]
struct ProcBsdShortInfo {
    pid: u32,
    ppid: u32,
    pgid: u32,
    status: u32,
    comm: [u8; 16],
    flags: u32,
    uid: u32,
    gid: u32,
    ruid: u32,
    rgid: u32,
    svuid: u32,
    svgid: u32,
    rfu: u32,
}

const PROC_PIDT_SHORTBSDINFO: c_int = 13;
const _: () = assert!(size_of::<ProcBsdShortInfo>() == 64);
const _: () = assert!(offset_of!(ProcBsdShortInfo, ruid) == 44);

pub(super) fn read() -> io::Result<Footprint> {
    let mut record = RusageInfoV6::default();
    let status = unsafe {
        proc_pid_rusage(
            getpid(),
            RUSAGE_INFO_V6,
            std::ptr::from_mut(&mut record).cast(),
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Footprint {
        current_bytes: record.phys_footprint,
        lifetime_bytes_max: record.lifetime_max_phys_footprint,
        resident_bytes: record.resident_size,
    })
}

#[repr(C)]
#[derive(Default)]
struct RusageInfoV6 {
    uuid: [u8; 16],
    user_time: u64,
    system_time: u64,
    pkg_idle_wkups: u64,
    interrupt_wkups: u64,
    pageins: u64,
    wired_size: u64,
    resident_size: u64,
    phys_footprint: u64,
    proc_start_abstime: u64,
    proc_exit_abstime: u64,
    child_user_time: u64,
    child_system_time: u64,
    child_pkg_idle_wkups: u64,
    child_interrupt_wkups: u64,
    child_pageins: u64,
    child_elapsed_abstime: u64,
    diskio_bytesread: u64,
    diskio_byteswritten: u64,
    cpu_time_qos_default: u64,
    cpu_time_qos_maintenance: u64,
    cpu_time_qos_background: u64,
    cpu_time_qos_utility: u64,
    cpu_time_qos_legacy: u64,
    cpu_time_qos_user_initiated: u64,
    cpu_time_qos_user_interactive: u64,
    billed_system_time: u64,
    serviced_system_time: u64,
    logical_writes: u64,
    lifetime_max_phys_footprint: u64,
    instructions: u64,
    cycles: u64,
    billed_energy: u64,
    serviced_energy: u64,
    interval_max_phys_footprint: u64,
    runnable_time: u64,
    flags: u64,
    user_ptime: u64,
    system_ptime: u64,
    pinstructions: u64,
    pcycles: u64,
    energy_nj: u64,
    penergy_nj: u64,
    secure_time_in_system: u64,
    secure_ptime_in_system: u64,
    neural_footprint: u64,
    lifetime_max_neural_footprint: u64,
    interval_max_neural_footprint: u64,
    conclave_footprint: u64,
    page_wait_time_mach: u64,
    page_cache_hits: u64,
    reserved: [u64; 6],
}

const _: () = assert!(size_of::<RusageInfoV6>() == 464);
const _: () = assert!(offset_of!(RusageInfoV6, resident_size) == 64);
const _: () = assert!(offset_of!(RusageInfoV6, phys_footprint) == 72);
const _: () = assert!(offset_of!(RusageInfoV6, lifetime_max_phys_footprint) == 240);
const RUSAGE_INFO_V6: c_int = 6;

unsafe extern "C" {
    fn _exit(status: c_int) -> !;
    fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
    fn getpid() -> c_int;
    fn getuid() -> u32;
    fn proc_pidinfo(
        pid: c_int,
        flavor: c_int,
        arg: u64,
        buffer: *mut c_void,
        buffer_size: c_int,
    ) -> c_int;
}
