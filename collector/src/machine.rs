use std::ffi::CStr;
use std::io;

use crate::mach;

// `processor_cpu_load_info.cpu_ticks` holds a core's ticks at the indices libc's CPU_STATE
// constants name.
const USER_INDEX: usize = cpu_ticks_index(libc::CPU_STATE_USER);
const SYSTEM_INDEX: usize = cpu_ticks_index(libc::CPU_STATE_SYSTEM);
const IDLE_INDEX: usize = cpu_ticks_index(libc::CPU_STATE_IDLE);
const NICE_INDEX: usize = cpu_ticks_index(libc::CPU_STATE_NICE);

/// Converts a `CPU_STATE` constant to the index it names in `cpu_ticks`, at compile time,
/// where `as` would do it with no check.
const fn cpu_ticks_index(state: libc::c_int) -> usize {
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

unsafe extern "C" {
    fn mach_host_self() -> libc::mach_port_t;
}

pub struct Machine {
    host: libc::mach_port_t,
    page_size: u64,
    swap_usage: Sysctl,
    pressure_level: Sysctl,
    pub cpus: Vec<CpuTicks>,
    pub memory: Memory,
}

impl Machine {
    pub fn new() -> io::Result<Self> {
        let total_bytes: u64 = Sysctl::named(c"hw.memsize")?.read()?;
        let page_size = u64::try_from(unsafe { libc::vm_page_size }).map_err(io::Error::other)?;

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
        let mut count: libc::natural_t = 0;
        let mut info: libc::processor_info_array_t = std::ptr::null_mut();
        let mut size: libc::mach_msg_type_number_t = 0;
        let result = unsafe {
            libc::host_processor_info(
                self.host,
                libc::PROCESSOR_CPU_LOAD_INFO,
                &raw mut count,
                &raw mut info,
                &raw mut size,
            )
        };
        if result != libc::KERN_SUCCESS {
            return Err(io::Error::other(format!(
                "host_processor_info returned {result}"
            )));
        }

        let loads = unsafe {
            std::slice::from_raw_parts(
                info.cast::<libc::processor_cpu_load_info>(),
                usize_from_u32(count),
            )
        };

        self.cpus.clear();
        self.cpus.extend(loads.iter().map(|load| CpuTicks {
            user: load.cpu_ticks[USER_INDEX],
            system: load.cpu_ticks[SYSTEM_INDEX],
            nice: load.cpu_ticks[NICE_INDEX],
            idle: load.cpu_ticks[IDLE_INDEX],
        }));

        unsafe {
            libc::vm_deallocate(
                mach::task_self(),
                info.addr(),
                usize_from_u32(size) * size_of::<libc::integer_t>(),
            )
        };

        Ok(())
    }

    fn read_memory(&mut self) -> io::Result<()> {
        let mut vm = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        let result = unsafe {
            libc::host_statistics64(
                self.host,
                libc::HOST_VM_INFO64,
                vm.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        if result != libc::KERN_SUCCESS {
            return Err(io::Error::other(format!(
                "host_statistics64 returned {result}"
            )));
        }

        let vm = unsafe { vm.assume_init() };
        let swap: libc::xsw_usage = self.swap_usage.read()?;
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
            swap_total_bytes: swap.xsu_total,
            swap_used_bytes: swap.xsu_used,
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
    mib: [libc::c_int; 12],
    len: libc::c_uint,
}

impl Sysctl {
    fn named(name: &CStr) -> io::Result<Self> {
        let mut mib = [0; 12];
        let mut len = mib.len();
        if unsafe { libc::sysctlnametomib(name.as_ptr(), mib.as_mut_ptr(), &raw mut len) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            mib,
            len: libc::c_uint::try_from(len).unwrap_or(0),
        })
    }

    fn read<T: Copy + Default>(&self) -> io::Result<T> {
        let mut value = T::default();
        let mut size = size_of::<T>();
        let result = unsafe {
            libc::sysctl(
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

    fn sysctl_u64(name: &CStr) -> u64 {
        let mut value = 0u64;
        let mut size = size_of::<u64>();
        unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&raw mut value).cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        value
    }

    #[test]
    fn reads_the_ticks_of_every_logical_cpu() {
        let mut machine = Machine::new().unwrap();

        machine.read().unwrap();

        assert_eq!(
            sysctl_u64(c"hw.logicalcpu"),
            u64::try_from(machine.cpus.len()).unwrap()
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
        assert_eq!(sysctl_u64(c"hw.memsize"), memory.total_bytes);
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
