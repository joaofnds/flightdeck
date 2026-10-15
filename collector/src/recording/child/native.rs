//! Native child usage and reaping; every byte of kernel output starts zeroed.
use super::{ChildCost, Finished};
use crate::clock::Timebase;
use std::ffi::{c_int, c_void};
use std::io;
use std::mem::offset_of;

pub(super) fn sample(pid: i32, timebase: Timebase) -> io::Result<ChildCost> {
    let mut record = RusageInfoV6::default();
    if unsafe { proc_pid_rusage(pid, 6, std::ptr::from_mut(&mut record).cast()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let user_ns = timebase.nanoseconds(record.user_time)?;
    let system_ns = timebase.nanoseconds(record.system_time)?;
    let cpu_ns = user_ns
        .checked_add(system_ns)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    Ok(ChildCost {
        cpu_ns,
        footprint_bytes: record.phys_footprint,
        footprint_bytes_max: record.lifetime_max_phys_footprint,
    })
}

pub(super) struct Reaped {
    status: i32,
    usage: Rusage,
}

impl Reaped {
    pub(super) fn finished(&self) -> io::Result<Finished> {
        let user_ns = timeval_ns(&self.usage.utime)?;
        let system_ns = timeval_ns(&self.usage.stime)?;
        let cpu_ns = user_ns
            .checked_add(system_ns)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        Ok(Finished {
            cpu_ns,
            status: self.status,
            resident_bytes_max: u64::try_from(self.usage.maxrss)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?,
        })
    }
}

pub(super) fn wait(pid: i32, blocking: bool) -> io::Result<Option<Reaped>> {
    let mut reaped = Reaped {
        status: 0,
        usage: Rusage::default(),
    };
    let options = i32::from(!blocking); // WNOHANG is 1 on Darwin.
    for _interruption in 0..8 {
        let returned =
            unsafe { wait4(pid, &raw mut reaped.status, options, &raw mut reaped.usage) };
        if returned == pid {
            return Ok(Some(reaped));
        }
        if returned == 0 {
            return Ok(None);
        }
        if returned == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(4) {
                continue;
            }
            return Err(error);
        }
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Err(io::Error::from(io::ErrorKind::Interrupted))
}

fn timeval_ns(time: &Timeval) -> io::Result<u64> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    let seconds = u64::try_from(time.tv_sec).map_err(|_| invalid())?;
    let microseconds = u64::try_from(time.tv_usec).map_err(|_| invalid())?;
    if microseconds >= 1_000_000 {
        return Err(invalid());
    }
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|seconds_ns| seconds_ns.checked_add(microseconds * 1_000))
        .ok_or_else(invalid)
}

#[derive(Default)]
#[repr(C)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i32,
    padding: u32,
}

#[derive(Default)]
#[repr(C)]
struct Rusage {
    utime: Timeval,
    stime: Timeval,
    maxrss: i64,
    ixrss: i64,
    idrss: i64,
    isrss: i64,
    minflt: i64,
    majflt: i64,
    nswap: i64,
    inblock: i64,
    oublock: i64,
    msgsnd: i64,
    msgrcv: i64,
    nsignals: i64,
    nvcsw: i64,
    nivcsw: i64,
}

const _: () = assert!(size_of::<Timeval>() == 16);
const _: () = assert!(offset_of!(Timeval, tv_usec) == 8);
const _: () = assert!(size_of::<Rusage>() == 144);
const _: () = assert!(offset_of!(Rusage, stime) == 16);
const _: () = assert!(offset_of!(Rusage, maxrss) == 32);

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
const _: () = assert!(offset_of!(RusageInfoV6, user_time) == 16);
const _: () = assert!(offset_of!(RusageInfoV6, system_time) == 24);
const _: () = assert!(offset_of!(RusageInfoV6, phys_footprint) == 72);
const _: () = assert!(offset_of!(RusageInfoV6, lifetime_max_phys_footprint) == 240);

unsafe extern "C" {
    pub(super) fn kill(pid: c_int, signal: c_int) -> c_int;
    fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
    fn wait4(pid: c_int, status: *mut c_int, options: c_int, usage: *mut Rusage) -> c_int;
}
