//! The Mac's readings that belong to no process, core or device: the load average, the open
//! files, the thermal pressure, the GPU's memory and the boot time, read each tick, and the
//! machine's facts, read once when the collector starts, since they do not change while the
//! Mac runs.

use std::ffi::{CStr, c_char, c_int};
use std::io;

use crate::clock_states::ClockTables;
use crate::gpu_clients::{CLIENTS_MAX, GpuClientTime, GpuClients};
use crate::gpu_memory::{GpuMemory, GpuMemoryReading};
use crate::kernel::Sysctl;

/// The most bytes a machine fact's text holds, past the 12 of `Apple M5 Pro` and the 48 an
/// Intel chip's brand string takes.
pub const FACT_BYTES_MAX: usize = 64;

/// The readings of one tick, each none where its read failed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SystemReading {
    /// The 1, 5 and 15 minute averages.
    pub load_average: Option<[f64; 3]>,
    pub open_files: Option<u64>,
    pub open_files_limit: Option<u64>,
    /// The level macOS publishes, 0 nominal through 4 sleeping.
    pub thermal_pressure: Option<u8>,
    pub gpu_memory: Option<GpuMemoryReading>,
    pub boot_wall_ns: Option<u64>,
}

/// The machine's identity and sizes, each none where its read failed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MachineFacts {
    pub model: Option<FactText>,
    pub chip: Option<FactText>,
    pub architecture: Option<FactText>,
    pub cpu_cores: Option<u32>,
    pub gpu_cores: Option<u32>,
    pub memory_bytes: Option<u64>,
    /// The chip's two performance levels, level 0 the fastest, or none on a chip with other
    /// than two.
    pub levels: Option<[PerformanceLevel; 2]>,
}

/// One performance level of the chip's CPUs, as macOS names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerformanceLevel {
    pub name: FactText,
    pub logical_cpus: u32,
}

/// A machine fact's text, UTF-8 of at most `FACT_BYTES_MAX` bytes, held without allocating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FactText {
    bytes: [u8; FACT_BYTES_MAX],
    count: u8,
}

impl Default for FactText {
    fn default() -> Self {
        Self {
            bytes: [0; FACT_BYTES_MAX],
            count: 0,
        }
    }
}

impl FactText {
    /// The text of `bytes`, or none where they are empty, longer than `FACT_BYTES_MAX`, hold
    /// a NUL or are not UTF-8.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }
        if bytes.len() > FACT_BYTES_MAX {
            return None;
        }
        if bytes.contains(&0) {
            return None;
        }
        std::str::from_utf8(bytes).ok()?;

        let mut text = Self::default();
        text.bytes[..bytes.len()].copy_from_slice(bytes);
        text.count = u8::try_from(bytes.len()).ok()?;
        Some(text)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        let bytes = &self.bytes[..usize::from(self.count)];
        match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => unreachable!("a fact's text was checked as UTF-8: {error}"),
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.count)]
    }
}

/// The handles each tick's reads use, opened once, and what the last tick read.
pub struct System {
    open_files: Option<Sysctl>,
    open_files_limit: Option<Sysctl>,
    boot_time: Option<Sysctl>,
    thermal_pressure: Option<ThermalPressure>,
    gpu_memory: Option<GpuMemory>,
    gpu_clients: Option<GpuClients>,
    clock: Option<ClockTables>,
    pub machine: MachineFacts,
    pub reading: SystemReading,
    /// Each process's GPU time the last tick read, by pid, rising.
    pub gpu_times: Vec<GpuClientTime>,
}

impl System {
    /// Opens every source this Mac has and reads the machine's facts. A source that fails to
    /// open is reported and read as none, so a Mac without one records every other.
    #[must_use]
    pub fn open() -> Self {
        Self {
            open_files: open_source("kern.num_files", Sysctl::named(c"kern.num_files")),
            open_files_limit: open_source("kern.maxfiles", Sysctl::named(c"kern.maxfiles")),
            boot_time: open_source("kern.boottime", Sysctl::named(c"kern.boottime")),
            thermal_pressure: open_source("thermal pressure", ThermalPressure::register()),
            gpu_memory: open_source("GPU memory", GpuMemory::open()),
            gpu_clients: open_source("GPU clients", GpuClients::new()),
            clock: open_source("clock tables", crate::clock_states::read()),
            machine: machine_facts(),
            reading: SystemReading::default(),
            gpu_times: Vec::with_capacity(CLIENTS_MAX),
        }
    }

    /// Reads the tick's system readings. A read that fails is reported and left none.
    pub fn read(&mut self) {
        self.reading = SystemReading {
            load_average: reported("load average", load_average()),
            open_files: self
                .open_files
                .as_ref()
                .and_then(|open_files| reported("kern.num_files", read_count(open_files))),
            open_files_limit: self
                .open_files_limit
                .as_ref()
                .and_then(|limit| reported("kern.maxfiles", read_count(limit))),
            thermal_pressure: self
                .thermal_pressure
                .as_ref()
                .and_then(|pressure| reported("thermal pressure", pressure.level())),
            gpu_memory: self.gpu_memory.as_ref().and_then(GpuMemory::read),
            boot_wall_ns: self
                .boot_time
                .as_ref()
                .and_then(|boot_time| reported("kern.boottime", read_boot_wall_ns(boot_time))),
        };
    }

    /// Reads each process's GPU time. A read that fails is reported and leaves no times.
    pub fn read_gpu(&mut self) {
        let Some(clients) = &self.gpu_clients else {
            self.gpu_times.clear();
            return;
        };

        let capacity = self.gpu_times.capacity();
        if let Err(error) = clients.read(&mut self.gpu_times) {
            crate::diagnostics::error(format_args!(
                "collector: GPU clients read failed: kind={:?} errno={:?}",
                error.kind(),
                error.raw_os_error()
            ));
            self.gpu_times.clear();
        }
        assert_eq!(self.gpu_times.capacity(), capacity);
    }

    /// The power manager's clock tables, read when the source opened, or none where
    /// unreadable.
    #[must_use]
    pub fn clock(&self) -> Option<&ClockTables> {
        self.clock.as_ref()
    }
}

/// Fills `gpu_ns` with the GPU time of each pid of `pids`, rising, from `times`, rising by
/// pid, with none for a pid that has no GPU clients or more time than the ring carries.
pub fn gpu_ns_by_pid(
    pids: impl Iterator<Item = i32>,
    times: &[GpuClientTime],
    gpu_ns: &mut Vec<Option<u64>>,
) {
    gpu_ns.clear();
    let capacity = gpu_ns.capacity();
    let mut time = 0;
    let mut previous = None;
    for pid in pids {
        if let Some(previous) = previous {
            assert!(previous < pid);
        }
        previous = Some(pid);

        while time < times.len() {
            if times[time].pid >= pid {
                break;
            }
            time += 1;
        }
        let found = times.get(time).filter(|found| found.pid == pid);
        assert!(gpu_ns.len() < capacity);
        gpu_ns.push(found.and_then(gpu_ns_by_pid_carried));
    }
}

/// The time of `found`, or none where it passes what the ring carries, which only a client
/// count no driver keeps, or a sum of them, reaches.
fn gpu_ns_by_pid_carried(found: &GpuClientTime) -> Option<u64> {
    if found.gpu_ns > crate::ring::GPU_NS_MAX {
        crate::diagnostics::error(format_args!(
            "collector: GPU time of pid {} read {} ns, past what the ring carries",
            found.pid, found.gpu_ns
        ));
        return None;
    }

    Some(found.gpu_ns)
}

fn open_source<T>(name: &str, opened: io::Result<T>) -> Option<T> {
    match opened {
        Ok(source) => Some(source),
        Err(error) => {
            crate::diagnostics::error(format_args!(
                "collector: recording no {name} readings, since it failed to open: {error}"
            ));
            None
        }
    }
}

/// The reading `read` gave, or none, with its error reported.
fn reported<T>(name: &str, read: io::Result<T>) -> Option<T> {
    match read {
        Ok(value) => Some(value),
        Err(error) => {
            crate::diagnostics::error(format_args!(
                "collector: {name} read failed: kind={:?} errno={:?}",
                error.kind(),
                error.raw_os_error()
            ));
            None
        }
    }
}

fn load_average() -> io::Result<[f64; 3]> {
    let mut load = [0.0_f64; 3];
    let count = unsafe { getloadavg(load.as_mut_ptr(), 3) };
    if count != 3 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    for average in load {
        if !average.is_finite() {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        if average < 0.0 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
    }

    Ok(load)
}

fn read_count(sysctl: &Sysctl) -> io::Result<u64> {
    let count: c_int = sysctl.read()?;
    u64::try_from(count).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}

/// The wall time macOS booted at, in nanoseconds since 1970.
fn read_boot_wall_ns(boot_time: &Sysctl) -> io::Result<u64> {
    read_boot_wall_ns_of(boot_time.read()?)
}

/// The nanoseconds since 1970 of the boot time `value`, or an error where it is before 1970,
/// holds a second or more of microseconds, or passes what a u64 counts.
fn read_boot_wall_ns_of(value: Timeval) -> io::Result<u64> {
    let seconds =
        u64::try_from(value.tv_sec).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let microseconds =
        u64::try_from(value.tv_usec).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if microseconds >= 1_000_000 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }

    seconds
        .checked_mul(1_000_000_000)
        .and_then(|nanoseconds| nanoseconds.checked_add(microseconds * 1000))
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
}

/// A time in seconds and microseconds, as `struct timeval` declares it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i32,
}

const _: () = assert!(size_of::<Timeval>() == 16);
const _: () = assert!(std::mem::offset_of!(Timeval, tv_sec) == 0);
const _: () = assert!(std::mem::offset_of!(Timeval, tv_usec) == 8);

/// Reads the machine's facts, each reported and left none where its read fails.
fn machine_facts() -> MachineFacts {
    let levels = match crate::machine::performance_levels() {
        Ok(Some(levels)) => machine_facts_levels(levels.logical_cpus),
        Ok(None) => None,
        Err(error) => {
            crate::diagnostics::error(format_args!(
                "collector: performance levels read failed: {error}"
            ));
            None
        }
    };
    let gpu_cores = match crate::machine::gpu_cores() {
        Ok(Some(cores)) => {
            if let Ok(cores) = u32::try_from(cores) {
                Some(cores)
            } else {
                crate::diagnostics::error(format_args!(
                    "collector: GPU cores read {cores}, past a count"
                ));
                None
            }
        }
        Ok(None) => None,
        Err(error) => {
            crate::diagnostics::error(format_args!("collector: GPU cores read failed: {error}"));
            None
        }
    };

    MachineFacts {
        model: machine_facts_text(c"hw.product"),
        chip: machine_facts_text(c"machdep.cpu.brand_string"),
        architecture: machine_facts_text(c"hw.machine"),
        cpu_cores: machine_facts_count(c"hw.physicalcpu_max"),
        gpu_cores,
        memory_bytes: reported(
            "hw.memsize",
            Sysctl::named(c"hw.memsize").and_then(|memory| memory.read::<u64>()),
        ),
        levels,
    }
}

fn machine_facts_levels(logical_cpus: [u32; 2]) -> Option<[PerformanceLevel; 2]> {
    let [fastest, slower] = logical_cpus;
    Some([
        PerformanceLevel {
            name: machine_facts_text(c"hw.perflevel0.name")?,
            logical_cpus: fastest,
        },
        PerformanceLevel {
            name: machine_facts_text(c"hw.perflevel1.name")?,
            logical_cpus: slower,
        },
    ])
}

fn machine_facts_count(name: &CStr) -> Option<u32> {
    let count: c_int = reported(
        "machine count",
        Sysctl::named(name).and_then(|sysctl| sysctl.read()),
    )?;
    match u32::try_from(count) {
        Ok(count) if count > 0 => Some(count),
        _ => {
            crate::diagnostics::error(format_args!("collector: {name:?} read {count}, no count"));
            None
        }
    }
}

/// The text the kernel state `name` holds, or none, reported, where it is unreadable or no
/// fact's text.
fn machine_facts_text(name: &CStr) -> Option<FactText> {
    let mut bytes = [0_u8; FACT_BYTES_MAX + 1];
    let mut count = bytes.len();
    let status = unsafe {
        sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &raw mut count,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        crate::diagnostics::error(format_args!(
            "collector: {name:?} read failed: {}",
            io::Error::last_os_error()
        ));
        return None;
    }
    // The kernel counts the final NUL, which the text leaves out.
    let text = bytes.get(..count.checked_sub(1)?)?;
    let fact = FactText::new(text);
    if fact.is_none() {
        crate::diagnostics::error(format_args!(
            "collector: {name:?} holds no machine fact's text"
        ));
    }
    fact
}

/// A registration for macOS's thermal pressure notification, whose state is the level.
struct ThermalPressure {
    token: c_int,
}

const NOTIFY_STATUS_OK: u32 = 0;

impl ThermalPressure {
    fn register() -> io::Result<Self> {
        let mut token: c_int = 0;
        let name = c"com.apple.system.thermalpressurelevel";
        let status = unsafe { notify_register_check(name.as_ptr(), &raw mut token) };
        if status != NOTIFY_STATUS_OK {
            return Err(io::Error::other(format!(
                "notify_register_check returned {status}"
            )));
        }

        Ok(Self { token })
    }

    fn level(&self) -> io::Result<u8> {
        let mut state = 0_u64;
        let status = unsafe { notify_get_state(self.token, &raw mut state) };
        if status != NOTIFY_STATUS_OK {
            crate::diagnostics::error(format_args!(
                "collector: notify_get_state returned {status}"
            ));
            return Err(io::Error::from(io::ErrorKind::Other));
        }

        u8::try_from(state).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
    }
}

impl Drop for ThermalPressure {
    fn drop(&mut self) {
        let status = unsafe { notify_cancel(self.token) };
        if status != NOTIFY_STATUS_OK {
            crate::diagnostics::error(format_args!("collector: notify_cancel returned {status}"));
        }
    }
}

unsafe extern "C" {
    fn getloadavg(load: *mut f64, count: c_int) -> c_int;
    fn sysctlbyname(
        name: *const c_char,
        old: *mut std::ffi::c_void,
        old_size: *mut usize,
        new: *mut std::ffi::c_void,
        new_size: usize,
    ) -> c_int;
    fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
    fn notify_get_state(token: c_int, state64: *mut u64) -> u32;
    fn notify_cancel(token: c_int) -> u32;
}

#[cfg(test)]
mod tests;
