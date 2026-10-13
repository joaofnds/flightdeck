use std::ffi::{CStr, c_char, c_int, c_uint};
use std::io;
use std::mem::offset_of;

use crate::kernel::{self, KERN_SUCCESS, KernReturn, MachPort};

// `processor_cpu_load_info.cpu_ticks` holds a core's ticks at the indices the CPU_STATE
// constants name.
const USER_INDEX: usize = cpu_ticks_index(CPU_STATE_USER);
const SYSTEM_INDEX: usize = cpu_ticks_index(CPU_STATE_SYSTEM);
const IDLE_INDEX: usize = cpu_ticks_index(CPU_STATE_IDLE);
const NICE_INDEX: usize = cpu_ticks_index(CPU_STATE_NICE);

/// Converts a `CPU_STATE` constant to the index it names in `cpu_ticks`, at compile time,
/// where `as` would do it with no check.
const fn cpu_ticks_index(state: c_int) -> usize {
    match state {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        _ => panic!("a CPU_STATE constant past the end of cpu_ticks"),
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CpuTicks {
    pub user: u32,
    pub system: u32,
    pub nice: u32,
    pub idle: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Memory {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub active_bytes: u64,
    pub inactive_bytes: u64,
    pub speculative_bytes: u64,
    pub wired_bytes: u64,
    pub compressed_bytes: u64,
    pub anonymous_bytes: u64,
    pub file_backed_bytes: u64,
    pub purgeable_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swapped_in_bytes: u64,
    pub swapped_out_bytes: u64,
    pub pageins: u64,
    pub pageouts: u64,
    pub compressions: u64,
    pub decompressions: u64,
    pub pressure: Pressure,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pressure {
    #[default]
    Normal,
    Warning,
    Critical,
}

impl Pressure {
    fn from_level(level: u32) -> io::Result<Self> {
        match level {
            1 => Ok(Self::Normal),
            2 => Ok(Self::Warning),
            4 => Ok(Self::Critical),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("macOS reported memory pressure level {other}"),
            )),
        }
    }
}

const CPU_STATE_MAX: usize = 4;
const CPU_STATE_USER: c_int = 0;
const CPU_STATE_SYSTEM: c_int = 1;
const CPU_STATE_IDLE: c_int = 2;
const CPU_STATE_NICE: c_int = 3;
const PROCESSOR_CPU_LOAD_INFO: c_int = 2;

/// A core's ticks in each state, as `struct processor_cpu_load_info` declares it.
#[repr(C)]
struct ProcessorCpuLoadInfo {
    cpu_ticks: [u32; CPU_STATE_MAX],
}

const _: () = assert!(size_of::<ProcessorCpuLoadInfo>() == 16);
const _: () = assert!(offset_of!(ProcessorCpuLoadInfo, cpu_ticks) == 0);

/// The machine's page counts, as `struct vm_statistics64` declares them through its first
/// revision. The kernel fills only the integers the call's count asks for, and the struct
/// has grown past this since.
#[repr(C)]
struct VmStatistics64 {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
}

const _: () = assert!(size_of::<VmStatistics64>() == 152);
const _: () = assert!(offset_of!(VmStatistics64, free_count) == 0);
const _: () = assert!(offset_of!(VmStatistics64, active_count) == 4);
const _: () = assert!(offset_of!(VmStatistics64, inactive_count) == 8);
const _: () = assert!(offset_of!(VmStatistics64, wire_count) == 12);
const _: () = assert!(offset_of!(VmStatistics64, pageins) == 32);
const _: () = assert!(offset_of!(VmStatistics64, pageouts) == 40);
const _: () = assert!(offset_of!(VmStatistics64, purgeable_count) == 88);
const _: () = assert!(offset_of!(VmStatistics64, speculative_count) == 92);
const _: () = assert!(offset_of!(VmStatistics64, decompressions) == 96);
const _: () = assert!(offset_of!(VmStatistics64, compressions) == 104);
const _: () = assert!(offset_of!(VmStatistics64, swapins) == 112);
const _: () = assert!(offset_of!(VmStatistics64, swapouts) == 120);
const _: () = assert!(offset_of!(VmStatistics64, compressor_page_count) == 128);
const _: () = assert!(offset_of!(VmStatistics64, external_page_count) == 136);
const _: () = assert!(offset_of!(VmStatistics64, internal_page_count) == 140);

const HOST_VM_INFO64: c_int = 4;
/// The size of `VmStatistics64` in integers, the count `host_statistics64` fills, as the
/// header computes it.
const HOST_VM_INFO64_REV1_COUNT: usize = size_of::<VmStatistics64>() / size_of::<c_int>();

const _: () = assert!(size_of::<VmStatistics64>().is_multiple_of(size_of::<c_int>()));
const _: () = assert!(HOST_VM_INFO64_REV1_COUNT == 38);

/// The machine's swap, as `struct xsw_usage` declares it without the `xsu_` prefix.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XswUsage {
    total: u64,
    avail: u64,
    used: u64,
    pagesize: u32,
    encrypted: c_int,
}

const _: () = assert!(size_of::<XswUsage>() == 32);
const _: () = assert!(offset_of!(XswUsage, total) == 0);
const _: () = assert!(offset_of!(XswUsage, used) == 16);

unsafe extern "C" {
    static vm_page_size: usize;

    fn mach_host_self() -> MachPort;
    fn host_processor_info(
        host: MachPort,
        flavor: c_int,
        processor_count: *mut u32,
        info: *mut *mut c_int,
        info_count: *mut u32,
    ) -> KernReturn;
    fn host_statistics64(
        host: MachPort,
        flavor: c_int,
        info: *mut c_int,
        info_count: *mut u32,
    ) -> KernReturn;
    fn vm_deallocate(task: MachPort, address: usize, size: usize) -> KernReturn;
    fn sysctlnametomib(name: *const c_char, mib: *mut c_int, mib_count: *mut usize) -> c_int;
}

pub struct Machine {
    host: MachPort,
    page_size: u64,
    swap_usage: Sysctl,
    pressure_level: Sysctl,
    pub cpus: Vec<CpuTicks>,
    pub memory: Memory,
}

impl Machine {
    pub fn new() -> io::Result<Self> {
        let total_bytes: u64 = Sysctl::named(c"hw.memsize")?.read()?;
        let page_size = u64::try_from(unsafe { vm_page_size }).map_err(io::Error::other)?;

        Ok(Self {
            host: unsafe { mach_host_self() },
            page_size,
            swap_usage: Sysctl::named(c"vm.swapusage")?,
            pressure_level: Sysctl::named(c"kern.memorystatus_vm_pressure_level")?,
            cpus: Vec::new(),
            memory: Memory {
                total_bytes,
                ..Memory::default()
            },
        })
    }

    pub fn read(&mut self) -> io::Result<()> {
        self.read_cpus()?;
        self.read_memory()
    }

    fn read_cpus(&mut self) -> io::Result<()> {
        let mut count: u32 = 0;
        let mut info: *mut c_int = std::ptr::null_mut();
        let mut size: u32 = 0;
        let result = unsafe {
            host_processor_info(
                self.host,
                PROCESSOR_CPU_LOAD_INFO,
                &raw mut count,
                &raw mut info,
                &raw mut size,
            )
        };
        if result != KERN_SUCCESS {
            return Err(io::Error::other(format!(
                "host_processor_info returned {result}"
            )));
        }

        let loads = unsafe {
            std::slice::from_raw_parts(info.cast::<ProcessorCpuLoadInfo>(), usize_from_u32(count))
        };

        self.cpus.clear();
        self.cpus.extend(loads.iter().map(|load| CpuTicks {
            user: load.cpu_ticks[USER_INDEX],
            system: load.cpu_ticks[SYSTEM_INDEX],
            nice: load.cpu_ticks[NICE_INDEX],
            idle: load.cpu_ticks[IDLE_INDEX],
        }));

        unsafe {
            vm_deallocate(
                kernel::task_self(),
                info.addr(),
                usize_from_u32(size) * size_of::<c_int>(),
            )
        };

        Ok(())
    }

    fn read_memory(&mut self) -> io::Result<()> {
        let mut vm = std::mem::MaybeUninit::<VmStatistics64>::zeroed();
        let mut count = match u32::try_from(HOST_VM_INFO64_REV1_COUNT) {
            Ok(count) => count,
            Err(error) => unreachable!("{error}"),
        };
        let result = unsafe {
            host_statistics64(
                self.host,
                HOST_VM_INFO64,
                vm.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        if result != KERN_SUCCESS {
            return Err(io::Error::other(format!(
                "host_statistics64 returned {result}"
            )));
        }

        let vm = unsafe { vm.assume_init() };
        let swap: XswUsage = self.swap_usage.read()?;
        let pages = |count: u32| u64::from(count) * self.page_size;

        self.memory = Memory {
            total_bytes: self.memory.total_bytes,
            free_bytes: pages(vm.free_count),
            active_bytes: pages(vm.active_count),
            inactive_bytes: pages(vm.inactive_count),
            speculative_bytes: pages(vm.speculative_count),
            wired_bytes: pages(vm.wire_count),
            compressed_bytes: pages(vm.compressor_page_count),
            anonymous_bytes: pages(vm.internal_page_count),
            file_backed_bytes: pages(vm.external_page_count),
            purgeable_bytes: pages(vm.purgeable_count),
            swap_total_bytes: swap.total,
            swap_used_bytes: swap.used,
            swapped_in_bytes: vm.swapins * self.page_size,
            swapped_out_bytes: vm.swapouts * self.page_size,
            pageins: vm.pageins,
            pageouts: vm.pageouts,
            compressions: vm.compressions,
            decompressions: vm.decompressions,
            pressure: Pressure::from_level(self.pressure_level.read()?)?,
        };

        Ok(())
    }
}

struct Sysctl {
    mib: [c_int; 12],
    len: c_uint,
}

impl Sysctl {
    fn named(name: &CStr) -> io::Result<Self> {
        let mut mib = [0; 12];
        let mut len = mib.len();
        if unsafe { sysctlnametomib(name.as_ptr(), mib.as_mut_ptr(), &raw mut len) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            mib,
            len: c_uint::try_from(len).unwrap_or(0),
        })
    }

    fn read<T: Copy + Default>(&self) -> io::Result<T> {
        let mut value = T::default();
        let mut size = size_of::<T>();
        let result = unsafe {
            kernel::sysctl(
                self.mib.as_ptr().cast_mut(),
                self.len,
                (&raw mut value).cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(value)
    }
}

/// Widens a count the kernel returns as a `u32`, which a `usize` holds on every 64-bit Mac.
fn usize_from_u32(value: u32) -> usize {
    const { assert!(size_of::<u32>() <= size_of::<usize>()) };

    match usize::try_from(value) {
        Ok(value) => value,
        Err(error) => unreachable!("{error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_ticks_of_every_logical_cpu() {
        let mut machine = Machine::new().unwrap();

        machine.read().unwrap();

        assert_eq!(
            u32::from_ne_bytes(kernel::sysctl_bytes(c"hw.logicalcpu")),
            u32::try_from(machine.cpus.len()).unwrap()
        );
    }

    #[test]
    fn reads_memory_that_fits_in_the_installed_ram() {
        let mut machine = Machine::new().unwrap();

        machine.read().unwrap();

        let memory = machine.memory;
        let in_ram = memory.free_bytes
            + memory.active_bytes
            + memory.inactive_bytes
            + memory.wired_bytes
            + memory.compressed_bytes;
        assert_eq!(
            u64::from_ne_bytes(kernel::sysctl_bytes(c"hw.memsize")),
            memory.total_bytes
        );
        assert!(
            in_ram <= memory.total_bytes && in_ram > memory.total_bytes / 2,
            "{memory:?}"
        );
        assert!(
            memory.swap_used_bytes <= memory.swap_total_bytes,
            "{memory:?}"
        );
    }

    #[test]
    fn reads_level_one_as_normal_pressure() {
        assert_eq!(Pressure::Normal, Pressure::from_level(1).unwrap());
    }

    #[test]
    fn reads_level_two_as_warning_pressure() {
        assert_eq!(Pressure::Warning, Pressure::from_level(2).unwrap());
    }

    #[test]
    fn reads_level_four_as_critical_pressure() {
        assert_eq!(Pressure::Critical, Pressure::from_level(4).unwrap());
    }

    #[test]
    fn refuses_a_pressure_level_macos_does_not_report() {
        assert!(Pressure::from_level(3).is_err());
    }
}
