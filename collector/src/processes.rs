use std::ffi::{c_int, c_void};
use std::io;
use std::mem::offset_of;

use crate::clock::Timebase;
use crate::kernel::{ESRCH, Sysctl};

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

/// Reads the most processes the system runs at once, `kern.maxproc`, which sizes every list
/// that holds an entry for each process.
pub fn process_limit() -> io::Result<usize> {
    let limit: c_int = Sysctl::named(c"kern.maxproc")?.read()?;
    usize::try_from(limit).map_err(io::Error::other)
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
    let room = c_int::try_from(capacity * size_of::<i32>()).map_err(io::Error::other)?;

    into.resize(capacity, 0);
    let count = unsafe { proc_listallpids(into.as_mut_ptr().cast(), room) };
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

pub fn read(pid: i32, timebase: Timebase) -> Result<Process, Unreadable> {
    let usage = read_usage(pid, timebase)?;
    let task: ProcTaskinfo = unsafe { pid_info(pid, PROC_PIDTASKINFO)? };

    Ok(Process {
        pid,
        usage,
        task: Task {
            threads: task.threadnum,
            faults: task.faults,
            context_switches: task.csw,
            mach_syscalls: task.syscalls_mach,
            unix_syscalls: task.syscalls_unix,
        },
    })
}

pub fn read_usage(pid: i32, timebase: Timebase) -> Result<Usage, Unreadable> {
    let usage = rusage(pid)?;

    Ok(Usage {
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
    })
}

fn rusage(pid: i32) -> Result<RusageInfoV6, Unreadable> {
    let mut usage = std::mem::MaybeUninit::<RusageInfoV6>::uninit();
    if unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V6, usage.as_mut_ptr().cast()) } != 0 {
        return Err(unreadable_from_errno());
    }

    Ok(unsafe { usage.assume_init() })
}

#[must_use]
pub fn is_alive(pid: i32) -> bool {
    unsafe { pid_info::<ProcBsdshortinfo>(pid, PROC_PIDT_SHORTBSDINFO) }.is_ok()
}

/// # Safety
///
/// Every bit pattern must be a valid `T`, as it is for a C struct of integers, since
/// `T` holds whatever bytes the kernel wrote.
pub(crate) unsafe fn pid_info<T>(pid: i32, flavor: c_int) -> Result<T, Unreadable> {
    let mut info = std::mem::MaybeUninit::<T>::uninit();
    let size = c_int::try_from(size_of::<T>()).unwrap_or(0);
    let written = unsafe { proc_pidinfo(pid, flavor, 0, info.as_mut_ptr().cast(), size) };
    if written != size {
        return Err(unreadable_from_errno());
    }

    Ok(unsafe { info.assume_init() })
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

fn unreadable_from_errno() -> Unreadable {
    if io::Error::last_os_error().raw_os_error() == Some(ESRCH) {
        Unreadable::Gone
    } else {
        Unreadable::Refused
    }
}

#[derive(Default)]
pub struct Sweep {
    pub read: Vec<Process>,
    pub refused: u32,
    pub gone: u32,
}

impl Sweep {
    pub fn run(&mut self, pids: &[i32], timebase: Timebase) {
        self.read.clear();
        self.refused = 0;
        self.gone = 0;

        for &pid in pids {
            match read(pid, timebase) {
                Ok(process) => self.read.push(process),
                Err(Unreadable::Refused) => self.refused += 1,
                Err(Unreadable::Gone) => self.gone += 1,
            }
        }
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

        let listed = list_pids(&mut pids);

        assert_eq!(io::ErrorKind::QuotaExceeded, listed.unwrap_err().kind());
        assert_eq!(1, pids.capacity());
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
        let process = read(pid, Timebase::read()).unwrap();
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
        let before = read_usage(pid, Timebase::read()).unwrap();

        spin_on_this_thread_for(Duration::from_millis(50));

        let after = read_usage(pid, Timebase::read()).unwrap();
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

        let usage = read_usage(child.id().cast_signed(), timebase).unwrap();
        let awake_ns = timebase.nanos(unsafe { mach_absolute_time() });
        child.kill().unwrap();
        child.wait().unwrap();

        let age = Duration::from_nanos(awake_ns - usage.awake_ns_at_start);
        assert!(age < Duration::from_secs(1), "started {age:?} ago");
    }

    #[test]
    fn reads_this_process_in_a_sweep() {
        let pid = std::process::id().cast_signed();
        let mut sweep = Sweep::default();

        sweep.run(&[pid], Timebase::read());

        let read: Vec<i32> = sweep.read.iter().map(|process| process.pid).collect();
        assert_eq!(vec![pid], read);
    }

    #[test]
    fn reads_or_counts_every_listed_pid() {
        let mut pids = pid_list(process_limit().unwrap());
        list_pids(&mut pids).unwrap();
        let mut sweep = Sweep::default();

        sweep.run(&pids, Timebase::read());

        assert_eq!(
            pids.len(),
            sweep.read.len() + usize::try_from(sweep.refused + sweep.gone).unwrap()
        );
    }

    #[test]
    fn refuses_launchd_to_a_user_without_root() {
        assert_eq!(Unreadable::Refused, read(1, Timebase::read()).unwrap_err());
    }

    #[test]
    fn reads_a_reaped_child_as_gone() {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();

        let read = read(child.id().cast_signed(), Timebase::read());

        assert_eq!(Unreadable::Gone, read.unwrap_err());
    }

    #[test]
    fn finds_this_process_alive() {
        assert!(is_alive(std::process::id().cast_signed()));
    }

    #[test]
    fn finds_a_process_it_cannot_read_alive() {
        assert!(is_alive(1));
    }

    #[test]
    fn finds_an_exited_child_not_yet_reaped_not_alive() {
        let mut child = child_that_has_exited();

        let alive = is_alive(child.id().cast_signed());
        child.wait().unwrap();

        assert!(!alive);
    }
}
