use std::ffi::{c_int, c_void};
use std::io;
use std::mem::offset_of;

use crate::clock::Timebase;
use crate::kernel::{self, EPERM, ESRCH};
use crate::push_without_growing;

#[derive(Debug, Clone, Copy)]
pub struct Process {
    pub pid: i32,
    pub usage: Usage,
    pub task: Task,
}

#[derive(Debug, Clone, Copy)]
pub struct Usage {
    pub awake_ns_at_start: u64,
    pub user_ns: u64,
    pub system_ns: u64,
    pub energy_nj: u64,
    pub instructions: u64,
    pub super_instructions: u64,
    pub cycles: u64,
    pub idle_wakeups: u64,
    pub interrupt_wakeups: u64,
    pub pageins: u64,
    pub resident_bytes: u64,
    pub footprint_bytes: u64,
    pub disk_read_bytes: u64,
    pub disk_written_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Task {
    pub threads: i32,
    pub faults: i32,
    pub context_switches: i32,
    pub mach_syscalls: i32,
    pub unix_syscalls: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    Refused,
    Gone,
}

pub struct Sweep {
    pub read: Vec<Process>,
    pub refused: u32,
    pub gone: u32,
}

impl Sweep {
    /// A sweep with room taken once for every process a listing holds, at most
    /// `process_limit`.
    #[must_use]
    pub fn new(process_limit: usize) -> Self {
        assert!(process_limit > 0);

        let sweep = Self {
            read: Vec::with_capacity(process_limit),
            refused: 0,
            gone: 0,
        };
        assert!(sweep.read.capacity() >= process_limit);
        sweep
    }

    pub fn run(&mut self, timebase: Timebase, pids: &[i32]) -> io::Result<()> {
        assert!(pids.len() <= self.read.capacity());
        self.read.clear();
        self.refused = 0;
        self.gone = 0;

        for &pid in pids {
            match read(timebase, pid)? {
                Ok(process) => {
                    assert_eq!(pid, process.pid);
                    push_without_growing(&mut self.read, process);
                }
                Err(Unreadable::Refused) => self.refused += 1,
                Err(Unreadable::Gone) => self.gone += 1,
            }
        }

        // Each pid is read or counted once.
        assert!(self.read.len() <= pids.len());
        let unread = pids.len() - self.read.len();
        assert_eq!(Ok(unread), usize::try_from(self.refused + self.gone));
        Ok(())
    }
}

/// The highest pid macOS gives a process, `PID_MAX` in XNU's `bsd/sys/proc_internal.h`, which
/// the SDK does not declare. Pids count up to it and then wrap to low pids again.
pub const PID_MAX: i32 = 99_999;

#[repr(C)]
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
    cpu_time_qos: [u64; 7],
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
const _: () = assert!(offset_of!(RusageInfoV6, pkg_idle_wkups) == 32);
const _: () = assert!(offset_of!(RusageInfoV6, interrupt_wkups) == 40);
const _: () = assert!(offset_of!(RusageInfoV6, pageins) == 48);
const _: () = assert!(offset_of!(RusageInfoV6, resident_size) == 64);
const _: () = assert!(offset_of!(RusageInfoV6, phys_footprint) == 72);
const _: () = assert!(offset_of!(RusageInfoV6, proc_start_abstime) == 80);
const _: () = assert!(offset_of!(RusageInfoV6, diskio_bytesread) == 144);
const _: () = assert!(offset_of!(RusageInfoV6, diskio_byteswritten) == 152);
const _: () = assert!(offset_of!(RusageInfoV6, instructions) == 248);
const _: () = assert!(offset_of!(RusageInfoV6, cycles) == 256);
const _: () = assert!(offset_of!(RusageInfoV6, pinstructions) == 320);
const _: () = assert!(offset_of!(RusageInfoV6, energy_nj) == 336);

const RUSAGE_INFO_V6: c_int = 6;

/// Info `proc_pidinfo` gives about a process, which it gives under one flavor.
pub(crate) trait PidInfo {
    const FLAVOR: c_int;
}

/// A process's task, as `struct proc_taskinfo` declares it.
#[repr(C)]
struct ProcTaskinfo {
    virtual_size: u64,
    resident_size: u64,
    total_user: u64,
    total_system: u64,
    threads_user: u64,
    threads_system: u64,
    policy: i32,
    faults: i32,
    pageins: i32,
    cow_faults: i32,
    messages_sent: i32,
    messages_received: i32,
    syscalls_mach: i32,
    syscalls_unix: i32,
    csw: i32,
    threadnum: i32,
    numrunning: i32,
    priority: i32,
}

const _: () = assert!(size_of::<ProcTaskinfo>() == 96);
const _: () = assert!(offset_of!(ProcTaskinfo, faults) == 52);
const _: () = assert!(offset_of!(ProcTaskinfo, syscalls_mach) == 72);
const _: () = assert!(offset_of!(ProcTaskinfo, syscalls_unix) == 76);
const _: () = assert!(offset_of!(ProcTaskinfo, csw) == 80);
const _: () = assert!(offset_of!(ProcTaskinfo, threadnum) == 84);

impl PidInfo for ProcTaskinfo {
    const FLAVOR: c_int = 4; // PROC_PIDTASKINFO
}

/// Room for `struct proc_bsdshortinfo`, which `is_alive` asks for and never reads.
#[repr(C)]
struct ProcBSDShortInfo([u32; 16]);

const _: () = assert!(size_of::<ProcBSDShortInfo>() == 64);
const _: () = assert!(align_of::<ProcBSDShortInfo>() == 4);

impl PidInfo for ProcBSDShortInfo {
    const FLAVOR: c_int = 13; // PROC_PIDT_SHORTBSDINFO
}

unsafe extern "C" {
    fn proc_listallpids(buffer: *mut c_void, buffer_size: c_int) -> c_int;
    fn proc_pidinfo(
        pid: c_int,
        flavor: c_int,
        argument: u64,
        buffer: *mut c_void,
        buffer_size: c_int,
    ) -> c_int;
    fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
}

/// Returns how many entries a list takes to hold one for every process macOS runs at once,
/// which `kern.maxproc` gives.
pub fn process_limit() -> io::Result<usize> {
    let process_limit = kernel::read_limit(c"kern.maxproc")?;

    assert!(process_limit > 0);
    Ok(process_limit)
}

/// Returns an empty pid list with room for every process `process_limit` allows and one more,
/// so a listing that fills it holds more processes than the limit allows.
#[must_use]
pub fn pid_list(process_limit: usize) -> Vec<i32> {
    assert!(process_limit > 0);

    let pids = Vec::with_capacity(process_limit + 1);
    assert!(pids.capacity() > process_limit);
    pids
}

/// Lists the pid of every process into `into`, up to the capacity it has, which it never
/// grows. A listing that fills `into` may have been cut short, so it is an error, and so is a
/// pid macOS gives no process.
pub fn list_pids(into: &mut Vec<i32>) -> io::Result<()> {
    let capacity = into.capacity();
    assert!(capacity > 0);
    let buffer_size = c_int::try_from(capacity * size_of::<i32>()).map_err(io::Error::other)?;

    into.resize(capacity, 0);
    let count = unsafe { proc_listallpids(into.as_mut_ptr().cast(), buffer_size) };
    let listed = match usize::try_from(count) {
        Ok(count) if count < capacity => {
            into.truncate(count);
            list_pids_check(into)
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::QuotaExceeded,
            format!("the pids listed filled the room for {capacity}"),
        )),
        Err(_) => Err(io::Error::last_os_error()),
    };
    if listed.is_ok() {
        assert!(!into.is_empty());
    } else {
        into.clear();
    }

    assert_eq!(capacity, into.capacity());
    assert!(into.len() < capacity);
    listed
}

/// Checks that `pids` holds a pid, as a listing holds this process's at least, and that each
/// is one macOS gives a process, from 0 to `PID_MAX`.
fn list_pids_check(pids: &[i32]) -> io::Result<()> {
    if pids.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the listing holds no pid, not even this process's",
        ));
    }

    assert!(!pids.is_empty());
    pids.iter().try_for_each(|&pid| {
        if (0..=PID_MAX).contains(&pid) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the listing holds pid {pid}, outside 0 to {PID_MAX}"),
            ))
        }
    })
}

/// Returns the pid of this process.
pub fn own_pid() -> io::Result<i32> {
    let pid = own_pid_check(std::process::id())?;

    assert!(pid >= 0);
    assert!(pid <= PID_MAX);
    Ok(pid)
}

/// Checks that `pid`, which macOS reports as this process's, is one macOS gives a process.
fn own_pid_check(pid: u32) -> io::Result<i32> {
    if pid <= PID_MAX.cast_unsigned() {
        let checked = pid.cast_signed();
        assert!(checked >= 0);
        assert!(checked <= PID_MAX);
        Ok(checked)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("this process has pid {pid}, past {PID_MAX}"),
        ))
    }
}

pub fn read(timebase: Timebase, pid: i32) -> io::Result<Result<Process, Unreadable>> {
    assert!(pid >= 0);
    assert!(pid <= PID_MAX);

    let usage = match read_usage(timebase, pid)? {
        Ok(usage) => usage,
        Err(unreadable) => return Ok(Err(unreadable)),
    };
    let task = match unsafe { pid_info::<ProcTaskinfo>(pid) }? {
        Ok(task) => task,
        Err(unreadable) => return Ok(Err(unreadable)),
    };

    Ok(Ok(Process {
        pid,
        usage,
        task: Task {
            threads: task.threadnum,
            faults: task.faults,
            context_switches: task.csw,
            mach_syscalls: task.syscalls_mach,
            unix_syscalls: task.syscalls_unix,
        },
    }))
}

pub fn read_usage(timebase: Timebase, pid: i32) -> io::Result<Result<Usage, Unreadable>> {
    assert!(pid >= 0);
    assert!(pid <= PID_MAX);

    let mut usage = std::mem::MaybeUninit::<RusageInfoV6>::zeroed();
    let returned = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V6, usage.as_mut_ptr().cast()) };
    if let Err(unreadable) = read_usage_returned(returned, io::Error::last_os_error())? {
        return Ok(Err(unreadable));
    }

    assert_eq!(0, returned);
    let usage = unsafe { usage.assume_init_ref() };
    Ok(Ok(Usage {
        awake_ns_at_start: timebase.nanoseconds(usage.proc_start_abstime)?,
        user_ns: timebase.nanoseconds(usage.user_time)?,
        system_ns: timebase.nanoseconds(usage.system_time)?,
        energy_nj: usage.energy_nj,
        instructions: usage.instructions,
        super_instructions: usage.pinstructions,
        cycles: usage.cycles,
        idle_wakeups: usage.pkg_idle_wkups,
        interrupt_wakeups: usage.interrupt_wkups,
        pageins: usage.pageins,
        resident_bytes: usage.resident_size,
        footprint_bytes: usage.phys_footprint,
        disk_read_bytes: usage.diskio_bytesread,
        disk_written_bytes: usage.diskio_byteswritten,
    }))
}

/// Reads what a `proc_pid_rusage` `returned`, and the `error` it set where it failed, which it
/// does by returning -1.
fn read_usage_returned(returned: c_int, error: io::Error) -> io::Result<Result<(), Unreadable>> {
    assert!(error.raw_os_error().is_some());

    if returned == 0 {
        Ok(Ok(()))
    } else {
        if returned == -1 {
            unreadable_of(error).map(Err)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("proc_pid_rusage returned {returned}, neither 0 nor -1"),
            ))
        }
    }
}

/// Returns whether a process has `pid`.
pub fn is_alive(pid: i32) -> io::Result<bool> {
    assert!(pid >= 0);
    assert!(pid <= PID_MAX);

    let info = unsafe { pid_info::<ProcBSDShortInfo>(pid) }?;
    Ok(is_alive_of(&info))
}

/// Returns whether the process a read of `info` was about is alive. One that refuses to be
/// read is, since macOS reports a pid no process has as gone, never as refused.
fn is_alive_of<T>(info: &Result<T, Unreadable>) -> bool {
    match info {
        Ok(_) | Err(Unreadable::Refused) => true,
        Err(Unreadable::Gone) => false,
    }
}

/// # Safety
///
/// Every bit pattern must be a valid `T`, as it is for a C struct of integers, since
/// `T` holds whatever bytes the kernel wrote.
pub(crate) unsafe fn pid_info<T: PidInfo>(pid: i32) -> io::Result<Result<T, Unreadable>> {
    const { assert!(T::FLAVOR > 0) };
    assert!(pid >= 0);
    assert!(pid <= PID_MAX);

    let mut info = std::mem::MaybeUninit::<T>::uninit();
    let size = pid_info_size::<T>();
    assert!(size > 0);
    let written = unsafe { proc_pidinfo(pid, T::FLAVOR, 0, info.as_mut_ptr().cast(), size) };
    if let Err(unreadable) = pid_info_written::<T>(written, io::Error::last_os_error())? {
        return Ok(Err(unreadable));
    }

    assert_eq!(size, written);
    Ok(Ok(unsafe { info.assume_init() }))
}

/// The size of `T` as `proc_pidinfo` takes it. A type too large for that fails the build.
fn pid_info_size<T>() -> c_int {
    const { assert!(size_of::<T>() > 0) };
    const { assert!(size_of::<T>() < 1 << 31) };

    match c_int::try_from(size_of::<T>()) {
        Ok(size) => size,
        Err(error) => unreachable!("{error}"),
    }
}

/// Reads what a `proc_pidinfo` asked for a `T` returned: `written`, the count of bytes it
/// wrote, and the `error` it set where it failed, which it does by writing none. A count
/// besides none and the size of `T` is info of another size than `T`.
fn pid_info_written<T>(written: c_int, error: io::Error) -> io::Result<Result<(), Unreadable>> {
    let size = pid_info_size::<T>();
    assert!(size > 0);
    assert!(error.raw_os_error().is_some());

    if written == size {
        Ok(Ok(()))
    } else {
        if written == 0 {
            unreadable_of(error).map(Err)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("proc_pidinfo wrote {written} bytes of the {size} asked for"),
            ))
        }
    }
}

/// Reads the `error` a call about one process failed with: EPERM as refused, ESRCH as gone,
/// and any other errno as the error it is.
fn unreadable_of(error: io::Error) -> io::Result<Unreadable> {
    assert!(error.raw_os_error().is_some());

    match error.raw_os_error() {
        Some(EPERM) => Ok(Unreadable::Refused),
        Some(ESRCH) => Ok(Unreadable::Gone),
        _ => Err(error),
    }
}

#[cfg(test)]
pub(crate) fn child_that_has_exited() -> std::process::Child {
    use std::ffi::c_uint;

    /// Room for `siginfo_t`, which `waitid` fills and the tests never read.
    #[repr(C)]
    struct SigInfo([u64; 13]);

    const _: () = assert!(size_of::<SigInfo>() == 104);
    const _: () = assert!(align_of::<SigInfo>() == 8);

    const P_PID: c_uint = 1;
    const WEXITED: c_int = 4;
    const WNOWAIT: c_int = 32;

    unsafe extern "C" {
        fn waitid(kind: c_uint, id: u32, info: *mut SigInfo, options: c_int) -> c_int;
    }

    let child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let mut info = std::mem::MaybeUninit::<SigInfo>::zeroed();
    let waited = unsafe { waitid(P_PID, child.id(), info.as_mut_ptr(), WEXITED | WNOWAIT) };
    assert_eq!(0, waited, "{}", io::Error::last_os_error());

    child
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cost::spin_on_this_thread_for;
    use crate::kernel::EINVAL;

    /// A time in seconds and microseconds, as `struct timeval` declares it.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Timeval {
        tv_sec: i64,
        tv_usec: i32,
    }

    const _: () = assert!(size_of::<Timeval>() == 16);
    const _: () = assert!(offset_of!(Timeval, tv_sec) == 0);
    const _: () = assert!(offset_of!(Timeval, tv_usec) == 8);

    /// What a process has used, as `struct rusage` declares it.
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

    const _: () = assert!(size_of::<Rusage>() == 144);
    const _: () = assert!(offset_of!(Rusage, utime) == 0);
    const _: () = assert!(offset_of!(Rusage, stime) == 16);

    const RUSAGE_SELF: c_int = 0;

    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn getrusage(who: c_int, usage: *mut Rusage) -> c_int;
    }

    fn own_cpu_ns() -> u64 {
        let mut usage = std::mem::MaybeUninit::<Rusage>::uninit();
        let measured = unsafe { getrusage(RUSAGE_SELF, usage.as_mut_ptr()) };
        assert_eq!(0, measured, "{}", io::Error::last_os_error());

        let usage = unsafe { usage.assume_init() };
        let micros = |t: Timeval| {
            u64::try_from(t.tv_sec).unwrap() * 1_000_000 + u64::try_from(t.tv_usec).unwrap()
        };

        (micros(usage.utime) + micros(usage.stime)) * 1_000
    }

    #[test]
    fn reports_a_listing_that_fills_its_buffer_without_growing_it() {
        // Goal: a pid listing longer than its buffer is an error, and the buffer keeps the
        // capacity it was given, as Tiger Style's limit on everything asks.
        // Method: list into a buffer with room for one pid, fewer than any Mac runs.
        let mut pids = Vec::with_capacity(1);
        let capacity = pids.capacity();

        let listed = list_pids(&mut pids);

        assert_eq!(io::ErrorKind::QuotaExceeded, listed.unwrap_err().kind());
        assert_eq!(capacity, pids.capacity());
    }

    #[test]
    fn refuses_a_listed_pid_past_pid_max() {
        // Goal: a listing holding a pid past the highest macOS gives is an error, since every
        // function given a listed pid asserts it is one macOS gives.
        // Method: check a listing of 1 and PID_MAX + 1.
        let checked = list_pids_check(&[1, PID_MAX + 1]);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_negative_listed_pid() {
        // Goal: a listing holding a negative pid is an error, since macOS gives no process one.
        // Method: check a listing of 1 and -1.
        let checked = list_pids_check(&[1, -1]);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_an_empty_listing() {
        // Goal: a listing that holds no pid is an error, since it holds this process's at least.
        // Method: check a listing of no pid.
        let checked = list_pids_check(&[]);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn takes_a_listing_of_the_lowest_and_highest_pids() {
        // Goal: 0, the kernel's pid, and PID_MAX are pids macOS gives, so a listing holds them.
        // Method: check a listing of 0 and PID_MAX.
        let checked = list_pids_check(&[0, PID_MAX]);

        assert!(checked.is_ok(), "{checked:?}");
    }

    #[test]
    fn takes_an_own_pid_of_pid_max() {
        // Goal: PID_MAX is a pid macOS gives, so this process may have it.
        // Method: check an own pid of PID_MAX.
        let checked = own_pid_check(PID_MAX.cast_unsigned());

        assert_eq!(PID_MAX, checked.unwrap());
    }

    #[test]
    fn refuses_an_own_pid_past_pid_max() {
        // Goal: an own pid past the highest macOS gives is an error, since every function given
        // a pid asserts it is one macOS gives.
        // Method: check an own pid of PID_MAX + 1.
        let checked = own_pid_check(100_000);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_an_own_pid_past_the_range_of_an_i32() {
        // Goal: an own pid too large for an i32 is an error, rather than a negative pid.
        // Method: check an own pid of u32::MAX, which cast_signed reads as -1.
        let checked = own_pid_check(u32::MAX);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn lists_this_process_among_every_pid() {
        // Goal: a listing holds every process, this one included.
        // Method: list every pid into a list sized from the process limit, and look for this
        // process's pid.
        let mut pids = pid_list(process_limit().unwrap());

        list_pids(&mut pids).unwrap();

        assert!(pids.contains(&std::process::id().cast_signed()));
    }

    #[test]
    fn reads_this_process_with_the_cpu_time_getrusage_reports() {
        // Goal: the CPU time read for a process is the time it spent, in nanoseconds, as getrusage
        // reports it for this process.
        // Method: spin 50 ms, then read this process between two getrusage readings, and take
        // the read within 2 ms of them.
        spin_on_this_thread_for(Duration::from_millis(50));
        let pid = std::process::id().cast_signed();

        let before = own_cpu_ns();
        let process = read(Timebase::read().unwrap(), pid).unwrap().unwrap();
        let after = own_cpu_ns();

        let cpu_ns = process.usage.user_ns + process.usage.system_ns;
        assert!(
            before - 2_000_000 <= cpu_ns,
            "read {cpu_ns} ns, getrusage {before} to {after}"
        );
        assert!(
            cpu_ns <= after + 2_000_000,
            "read {cpu_ns} ns, getrusage {before} to {after}"
        );
    }

    #[test]
    fn reads_this_process_spending_energy_while_it_runs() {
        // Goal: a process's energy grows while it runs.
        // Method: read this process's usage before and after spinning 50 ms.
        let pid = std::process::id().cast_signed();
        let before = read_usage(Timebase::read().unwrap(), pid).unwrap().unwrap();

        spin_on_this_thread_for(Duration::from_millis(50));

        let after = read_usage(Timebase::read().unwrap(), pid).unwrap().unwrap();
        assert!(
            after.energy_nj > before.energy_nj,
            "{} nJ then {} nJ",
            before.energy_nj,
            after.energy_nj
        );
    }

    #[test]
    fn reads_when_a_process_started_in_awake_time() {
        // Goal: a process's start reads in awake time, the clock the collector keeps.
        // Method: spawn a child, read its start, and take it within a second of the awake time
        // mach_absolute_time reads just after.
        let timebase = Timebase::read().unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("1")
            .spawn()
            .unwrap();

        let usage = read_usage(timebase, child.id().cast_signed())
            .unwrap()
            .unwrap();
        let awake_ns = timebase
            .nanoseconds(unsafe { mach_absolute_time() })
            .unwrap();
        child.kill().unwrap();
        child.wait().unwrap();

        let age = Duration::from_nanos(awake_ns - usage.awake_ns_at_start);
        assert!(age < Duration::from_secs(1), "started {age:?} ago");
    }

    #[test]
    fn reads_this_process_in_a_sweep() {
        // Goal: a sweep reads each process it is given.
        // Method: sweep this process's pid alone.
        let pid = std::process::id().cast_signed();
        let mut sweep = Sweep::new(1);

        sweep.run(Timebase::read().unwrap(), &[pid]).unwrap();

        let read: Vec<i32> = sweep.read.iter().map(|process| process.pid).collect();
        assert_eq!(vec![pid], read);
    }

    #[test]
    fn reads_or_counts_every_listed_pid() {
        // Goal: a sweep of every listed pid reads each one or counts it as refused or gone.
        // Method: sweep a listing of every pid, and compare the readings and counts with it.
        let mut pids = pid_list(process_limit().unwrap());
        list_pids(&mut pids).unwrap();
        let mut sweep = Sweep::new(process_limit().unwrap());

        sweep.run(Timebase::read().unwrap(), &pids).unwrap();

        assert_eq!(
            pids.len(),
            sweep.read.len() + usize::try_from(sweep.refused + sweep.gone).unwrap()
        );
    }

    #[test]
    fn reads_the_process_limit_from_kern_maxproc() {
        // Goal: the process limit is kern.maxproc, the most processes macOS runs at once.
        // Method: compare the limit with kern.maxproc read as raw bytes.
        let maxproc = i32::from_ne_bytes(kernel::sysctl_bytes(c"kern.maxproc"));

        assert_eq!(usize::try_from(maxproc).unwrap(), process_limit().unwrap());
    }

    #[test]
    fn takes_room_for_every_process_the_limit_allows() {
        // Goal: a sweep takes room at startup for every process the limit allows, so no listing
        // makes a tick grow its readings.
        // Method: build a sweep for this Mac's process limit and compare its room with the limit.
        let limit = process_limit().unwrap();

        let sweep = Sweep::new(limit);

        assert!(sweep.read.capacity() >= limit);
    }

    #[test]
    fn refuses_launchd_to_a_user_without_root() {
        // Goal: a process this user may not read, such as one another user owns, reads as refused.
        // Method: read launchd, pid 1, which runs as root, as a user without root.
        assert_eq!(
            Unreadable::Refused,
            read(Timebase::read().unwrap(), 1).unwrap().unwrap_err()
        );
    }

    #[test]
    fn reads_a_reaped_child_as_gone() {
        // Goal: a pid whose process exited and was reaped reads as gone.
        // Method: spawn a child, wait for it, and read its pid.
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();

        let read = read(Timebase::read().unwrap(), child.id().cast_signed());

        assert_eq!(Unreadable::Gone, read.unwrap().unwrap_err());
    }

    #[test]
    fn reports_an_errno_besides_refused_and_gone_as_an_error() {
        // Goal: an errno other than EPERM and ESRCH is an error rather than a refusal, so a change
        // in macOS shows as one instead of as every process refusing.
        // Method: ask for an info flavor macOS does not have, which it refuses with EINVAL.
        #[repr(C)]
        struct UnknownInfo([u32; 24]);
        impl PidInfo for UnknownInfo {
            const FLAVOR: c_int = 9_999;
        }
        let pid = std::process::id().cast_signed();

        let info = unsafe { pid_info::<UnknownInfo>(pid) };

        assert_eq!(
            Some(EINVAL),
            info.err().and_then(|error| error.raw_os_error())
        );
    }

    #[test]
    fn refuses_info_shorter_than_asked_for() {
        // Goal: info shorter than its type is an error rather than a reading, since the rest of
        // the type would hold no bytes the kernel wrote, and no errno says why.
        // Method: read a call that wrote 40 of the 96 bytes of a task info and set no errno.
        let written = pid_info_written::<ProcTaskinfo>(40, io::Error::from_raw_os_error(0));

        assert_eq!(io::ErrorKind::InvalidData, written.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_usage_read_that_returned_neither_success_nor_failure() {
        // Goal: proc_pid_rusage returns 0 or -1, so any other value is an error rather than a
        // reading taken from whatever errno an earlier call left, such as ESRCH read as gone.
        // Method: read a call that returned 1 with ESRCH left in errno.
        let returned = read_usage_returned(1, io::Error::from_raw_os_error(ESRCH));

        assert_eq!(io::ErrorKind::InvalidData, returned.unwrap_err().kind());
    }

    #[test]
    fn reports_a_usage_read_failing_with_another_errno() {
        // Goal: a usage read that fails with an errno other than EPERM and ESRCH is that error,
        // rather than a refusal.
        // Method: read a call that returned -1 with EINVAL in errno, which proc_pid_rusage sets
        // for a flavor it does not have.
        let returned = read_usage_returned(-1, io::Error::from_raw_os_error(EINVAL));

        assert_eq!(Some(EINVAL), returned.unwrap_err().raw_os_error());
    }

    #[test]
    fn finds_this_process_alive() {
        // Goal: a running process is alive.
        // Method: ask about this process.
        assert!(is_alive(std::process::id().cast_signed()).unwrap());
    }

    #[test]
    fn finds_a_process_it_cannot_read_alive() {
        // Goal: a process whose task this user may not read is still found alive.
        // Method: ask about launchd, whose task info a user without root is refused, though not
        // the short BSD info that is_alive reads.
        assert!(is_alive(1).unwrap());
    }

    #[test]
    fn finds_a_process_that_refuses_to_be_read_alive() {
        // Goal: a process that refuses to be read is alive, since macOS reports a pid no process
        // has as gone, never as refused.
        // Method: take a refused read, since no process this user can name refuses the short BSD
        // info that is_alive reads.
        assert!(is_alive_of(&Err::<(), _>(Unreadable::Refused)));
    }

    #[test]
    fn finds_an_exited_child_not_yet_reaped_not_alive() {
        // Goal: a process that has exited is not alive, even while its parent has yet to reap it.
        // Method: ask about a child that has exited, which waitid with WNOWAIT leaves unreaped.
        let mut child = child_that_has_exited();

        let alive = is_alive(child.id().cast_signed()).unwrap();
        child.wait().unwrap();

        assert!(!alive);
    }
}
