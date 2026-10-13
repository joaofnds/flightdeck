use std::ffi::{c_int, c_void};
use std::io;
use std::mem::offset_of;

use crate::clock::Timebase;
use crate::kernel::{EPERM, ESRCH, Sysctl};
use crate::push_without_growing;

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

/// Room for `struct proc_bsdshortinfo`, which `is_alive` asks for and never reads.
#[repr(C)]
struct ProcBsdshortinfo([u32; 16]);

const _: () = assert!(size_of::<ProcBsdshortinfo>() == 64);
const _: () = assert!(align_of::<ProcBsdshortinfo>() == 4);

const PROC_PIDTASKINFO: c_int = 4;
const PROC_PIDT_SHORTBSDINFO: c_int = 13;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    Refused,
    Gone,
}

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

/// The highest pid macOS gives a process, `PID_MAX` in XNU's `bsd/sys/proc_internal.h`, which
/// the SDK does not declare. Pids count up to it and then wrap to low pids again.
pub const PID_MAX: i32 = 99_999;

/// Returns how many entries a list takes to hold one for every process macOS runs at once,
/// which `kern.maxproc` gives.
pub fn process_limit() -> io::Result<usize> {
    let maxproc: c_int = Sysctl::named(c"kern.maxproc")?.read()?;
    process_limit_of(maxproc)
}

/// The process limit a `kern.maxproc` of `maxproc` gives, which allows at least this process.
fn process_limit_of(maxproc: c_int) -> io::Result<usize> {
    let refused = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("kern.maxproc of {maxproc} allows no process"),
        )
    };

    let Ok(limit) = usize::try_from(maxproc) else {
        return Err(refused());
    };
    if limit > 0 {
        assert_eq!(Ok(maxproc), c_int::try_from(limit));
        Ok(limit)
    } else {
        Err(refused())
    }
}

/// Returns an empty pid list with room for every process `process_limit` allows and one more,
/// so a listing that fills it holds more processes than the limit allows.
#[must_use]
pub fn pid_list(process_limit: usize) -> Vec<i32> {
    Vec::with_capacity(process_limit + 1)
}

/// Lists the pid of every process into `into`, up to the capacity it has, which it never
/// grows. A listing that fills `into` may have been cut short, so it is an error.
pub fn list_pids(into: &mut Vec<i32>) -> io::Result<()> {
    let capacity = into.capacity();
    let buffer_size = c_int::try_from(capacity * size_of::<i32>()).map_err(io::Error::other)?;

    into.resize(capacity, 0);
    let count = unsafe { proc_listallpids(into.as_mut_ptr().cast(), buffer_size) };
    let listed = match usize::try_from(count) {
        Ok(count) if count < capacity => {
            into.truncate(count);
            Ok(())
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::QuotaExceeded,
            format!("the pids listed filled the room for {capacity}"),
        )),
        Err(_) => Err(io::Error::last_os_error()),
    };
    if listed.is_err() {
        into.clear();
    }

    assert_eq!(capacity, into.capacity());
    listed
}

pub fn read(pid: i32, timebase: Timebase) -> io::Result<Result<Process, Unreadable>> {
    let usage = match read_usage(pid, timebase)? {
        Ok(usage) => usage,
        Err(unreadable) => return Ok(Err(unreadable)),
    };
    let task = match unsafe { pid_info::<ProcTaskinfo>(pid, PROC_PIDTASKINFO) }? {
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

pub fn read_usage(pid: i32, timebase: Timebase) -> io::Result<Result<Usage, Unreadable>> {
    let mut usage = std::mem::MaybeUninit::<RusageInfoV6>::uninit();
    let returned = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V6, usage.as_mut_ptr().cast()) };
    if let Err(unreadable) = read_usage_returned(returned, io::Error::last_os_error())? {
        return Ok(Err(unreadable));
    }

    assert_eq!(0, returned);
    let usage = unsafe { usage.assume_init_ref() };
    Ok(Ok(Usage {
        awake_ns_at_start: timebase.nanos(usage.proc_start_abstime),
        user_ns: timebase.nanos(usage.user_time),
        system_ns: timebase.nanos(usage.system_time),
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
    if returned == 0 {
        Ok(Ok(()))
    } else {
        if returned == -1 {
            unreadable_of(error).map(Err)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("proc_pid_rusage returned {returned}, neither success nor failure"),
            ))
        }
    }
}

pub fn is_alive(pid: i32) -> io::Result<bool> {
    let info = unsafe { pid_info::<ProcBsdshortinfo>(pid, PROC_PIDT_SHORTBSDINFO) }?;
    Ok(info.is_ok())
}

/// # Safety
///
/// Every bit pattern must be a valid `T`, as it is for a C struct of integers, since
/// `T` holds whatever bytes the kernel wrote.
pub(crate) unsafe fn pid_info<T>(pid: i32, flavor: c_int) -> io::Result<Result<T, Unreadable>> {
    let mut info = std::mem::MaybeUninit::<T>::uninit();
    let size = c_int::try_from(size_of::<T>()).unwrap_or(0);
    let written = unsafe { proc_pidinfo(pid, flavor, 0, info.as_mut_ptr().cast(), size) };
    if let Err(unreadable) = pid_info_written(written, size, io::Error::last_os_error())? {
        return Ok(Err(unreadable));
    }

    assert_eq!(size, written);
    Ok(Ok(unsafe { info.assume_init() }))
}

/// Reads what a `proc_pidinfo` asked for `size` bytes returned: the count of bytes it `written`,
/// and the `error` it set where it failed, which it does by writing none. Any other count is
/// info of another size than its type.
fn pid_info_written(
    written: c_int,
    size: c_int,
    error: io::Error,
) -> io::Result<Result<(), Unreadable>> {
    assert!(size > 0);

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

/// Reads the `error` a call about one process failed with: EPERM as refused, ESRCH as gone,
/// and any other errno as the error it is.
fn unreadable_of(error: io::Error) -> io::Result<Unreadable> {
    match error.raw_os_error() {
        Some(EPERM) => Ok(Unreadable::Refused),
        Some(ESRCH) => Ok(Unreadable::Gone),
        _ => Err(error),
    }
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
        Self {
            read: Vec::with_capacity(process_limit),
            refused: 0,
            gone: 0,
        }
    }

    pub fn run(&mut self, pids: &[i32], timebase: Timebase) -> io::Result<()> {
        assert!(pids.len() <= self.read.capacity());
        self.read.clear();
        self.refused = 0;
        self.gone = 0;

        for &pid in pids {
            match read(pid, timebase)? {
                Ok(process) => push_without_growing(&mut self.read, process),
                Err(Unreadable::Refused) => self.refused += 1,
                Err(Unreadable::Gone) => self.gone += 1,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cost::spin_on_this_thread_for;

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
    fn lists_this_process_among_every_pid() {
        let mut pids = pid_list(process_limit().unwrap());

        list_pids(&mut pids).unwrap();

        assert!(pids.contains(&std::process::id().cast_signed()));
    }

    #[test]
    fn reads_this_process_with_the_cpu_time_getrusage_reports() {
        spin_on_this_thread_for(Duration::from_millis(50));
        let pid = std::process::id().cast_signed();

        let before = own_cpu_ns();
        let process = read(pid, Timebase::read()).unwrap().unwrap();
        let after = own_cpu_ns();

        let cpu_ns = process.usage.user_ns + process.usage.system_ns;
        assert!(
            before - 2_000_000 <= cpu_ns && cpu_ns <= after + 2_000_000,
            "read {cpu_ns} ns, getrusage {before} to {after}"
        );
    }

    #[test]
    fn reads_this_process_spending_energy_while_it_runs() {
        let pid = std::process::id().cast_signed();
        let before = read_usage(pid, Timebase::read()).unwrap().unwrap();

        spin_on_this_thread_for(Duration::from_millis(50));

        let after = read_usage(pid, Timebase::read()).unwrap().unwrap();
        assert!(
            after.energy_nj > before.energy_nj,
            "{} nJ then {} nJ",
            before.energy_nj,
            after.energy_nj
        );
    }

    #[test]
    fn reads_when_a_process_started_in_awake_time() {
        let timebase = Timebase::read();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("1")
            .spawn()
            .unwrap();

        let usage = read_usage(child.id().cast_signed(), timebase)
            .unwrap()
            .unwrap();
        let awake_ns = timebase.nanos(unsafe { mach_absolute_time() });
        child.kill().unwrap();
        child.wait().unwrap();

        let age = Duration::from_nanos(awake_ns - usage.awake_ns_at_start);
        assert!(age < Duration::from_secs(1), "started {age:?} ago");
    }

    #[test]
    fn reads_this_process_in_a_sweep() {
        let pid = std::process::id().cast_signed();
        let mut sweep = Sweep::new(1);

        sweep.run(&[pid], Timebase::read()).unwrap();

        let read: Vec<i32> = sweep.read.iter().map(|process| process.pid).collect();
        assert_eq!(vec![pid], read);
    }

    #[test]
    fn reads_or_counts_every_listed_pid() {
        let mut pids = pid_list(process_limit().unwrap());
        list_pids(&mut pids).unwrap();
        let mut sweep = Sweep::new(process_limit().unwrap());

        sweep.run(&pids, Timebase::read()).unwrap();

        assert_eq!(
            pids.len(),
            sweep.read.len() + usize::try_from(sweep.refused + sweep.gone).unwrap()
        );
    }

    #[test]
    fn reads_the_process_limit_from_kern_maxproc() {
        // Goal: the process limit is kern.maxproc, the most processes macOS runs at once.
        // Method: compare the limit with kern.maxproc read as raw bytes.
        let maxproc = i32::from_ne_bytes(crate::kernel::sysctl_bytes(c"kern.maxproc"));

        assert_eq!(usize::try_from(maxproc).unwrap(), process_limit().unwrap());
    }

    #[test]
    fn refuses_a_kern_maxproc_of_zero() {
        // Goal: a kern.maxproc that allows no process is an error, since this process runs.
        // Method: take the process limit of a kern.maxproc of 0.
        let limit = process_limit_of(0);

        assert_eq!(io::ErrorKind::InvalidData, limit.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_negative_kern_maxproc() {
        // Goal: a negative kern.maxproc is an error, since no count of processes is negative.
        // Method: take the process limit of a kern.maxproc of -1.
        let limit = process_limit_of(-1);

        assert_eq!(io::ErrorKind::InvalidData, limit.unwrap_err().kind());
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
        assert_eq!(
            Unreadable::Refused,
            read(1, Timebase::read()).unwrap().unwrap_err()
        );
    }

    #[test]
    fn reads_a_reaped_child_as_gone() {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();

        let read = read(child.id().cast_signed(), Timebase::read());

        assert_eq!(Unreadable::Gone, read.unwrap().unwrap_err());
    }

    #[test]
    fn reports_an_errno_besides_refused_and_gone_as_an_error() {
        // Goal: an errno other than EPERM and ESRCH is an error rather than a refusal, so a change
        // in macOS shows as one instead of as every process refusing.
        // Method: ask for an info flavor macOS does not have, which it refuses with EINVAL.
        const EINVAL: i32 = 22;
        let pid = std::process::id().cast_signed();

        let info = unsafe { pid_info::<ProcTaskinfo>(pid, 9_999) };

        assert_eq!(
            Some(EINVAL),
            info.err().and_then(|error| error.raw_os_error())
        );
    }

    #[test]
    fn refuses_info_shorter_than_asked_for() {
        // Goal: info shorter than its type is an error rather than a reading, since the rest of
        // the type would hold no bytes the kernel wrote, and no errno says why.
        // Method: read a call that wrote 40 of 96 bytes and set no errno.
        let written = pid_info_written(40, 96, io::Error::from_raw_os_error(0));

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
    fn finds_this_process_alive() {
        assert!(is_alive(std::process::id().cast_signed()).unwrap());
    }

    #[test]
    fn finds_a_process_it_cannot_read_alive() {
        assert!(is_alive(1).unwrap());
    }

    #[test]
    fn finds_an_exited_child_not_yet_reaped_not_alive() {
        let mut child = child_that_has_exited();

        let alive = is_alive(child.id().cast_signed()).unwrap();
        child.wait().unwrap();

        assert!(!alive);
    }
}
