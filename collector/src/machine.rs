use std::ffi::c_int;
use std::io;
use std::mem::offset_of;

use crate::kernel::{self, KernReturn, MachPort, Sysctl, kern_return_check};
use crate::{u64_from_usize, usize_from_u32};

pub struct Machine {
    host: MachPort,
    page_size: PageSize,
    swap_usage: Sysctl,
    pressure_level: Sysctl,
    cpus: Vec<CPUTicks>,
    pub memory: Memory,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CPUTicks {
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

/// The bytes a page of memory holds, above 0.
#[derive(Debug, Clone, Copy)]
struct PageSize {
    bytes: u64,
}

impl PageSize {
    /// Takes `size`, the bytes `vm_page_size` gives, or an error where it is 0.
    fn new(size: usize) -> io::Result<Self> {
        let bytes = u64_from_usize(size);
        if bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "vm_page_size gave a page of 0 bytes",
            ));
        }

        Ok(Self { bytes })
    }

    /// Returns the bytes `pages` pages hold, or an error where a u64 cannot hold them.
    fn bytes_of(self, pages: u64) -> io::Result<u64> {
        assert!(self.bytes > 0);

        let Some(bytes) = pages.checked_mul(self.bytes) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{pages} pages of {} bytes pass what a u64 holds",
                    self.bytes
                ),
            ));
        };

        Ok(bytes)
    }
}

impl Machine {
    pub fn new(cpu_limit: usize) -> io::Result<Self> {
        assert!(cpu_limit > 0);

        let total_bytes: u64 = Sysctl::named(c"hw.memsize")?.read()?;
        let page_size = PageSize::new(unsafe { vm_page_size })?;

        Ok(Self {
            host: unsafe { mach_host_self() },
            page_size,
            swap_usage: Sysctl::named(c"vm.swapusage")?,
            pressure_level: Sysctl::named(c"kern.memorystatus_vm_pressure_level")?,
            cpus: Vec::with_capacity(cpu_limit),
            memory: Memory {
                total_bytes,
                ..Memory::default()
            },
        })
    }

    pub fn read(&mut self) -> io::Result<()> {
        assert!(self.cpus.capacity() > 0);

        self.read_cpus()?;
        assert!(!self.cpus.is_empty());

        self.read_memory()
    }

    /// The ticks of each logical CPU the last read found. The list is private, since a read
    /// fills it within the room `new` gave it and never grows it.
    #[must_use]
    pub fn cpus(&self) -> &[CPUTicks] {
        assert!(self.cpus.capacity() > 0);

        &self.cpus
    }

    fn read_cpus(&mut self) -> io::Result<()> {
        let mut counts = ReplyCounts {
            cpu_count: 0,
            integer_count: 0,
        };
        let mut address: *mut c_int = std::ptr::null_mut();
        let result = unsafe {
            host_processor_info(
                self.host,
                PROCESSOR_CPU_LOAD_INFO,
                &raw mut counts.cpu_count,
                &raw mut address,
                &raw mut counts.integer_count,
            )
        };
        kern_return_check(result, "host_processor_info")?;
        if address.is_null() {
            return Err(io::Error::other(
                "host_processor_info replied with no loads",
            ));
        }

        let info = MachMemory {
            address,
            size: usize_from_u32(counts.integer_count) * size_of::<c_int>(),
        };

        let capacity = self.cpus.capacity();
        assert!(capacity > 0);
        let cpu_count = read_cpus_count_check(counts, capacity)?;
        let loads = unsafe {
            std::slice::from_raw_parts(info.address.cast::<ProcessorCPULoadInfo>(), cpu_count)
        };
        self.cpus.clear();
        self.cpus.extend(loads.iter().map(|load| CPUTicks {
            user: load.cpu_ticks[USER_INDEX],
            system: load.cpu_ticks[SYSTEM_INDEX],
            nice: load.cpu_ticks[NICE_INDEX],
            idle: load.cpu_ticks[IDLE_INDEX],
        }));

        Ok(())
    }

    fn read_memory(&mut self) -> io::Result<()> {
        assert!(self.page_size.bytes > 0);

        let mut statistics = std::mem::MaybeUninit::<VMStatistics64>::zeroed();
        let mut count = match u32::try_from(HOST_VM_INFO64_REV1_COUNT) {
            Ok(count) => count,
            Err(error) => unreachable!("{error}"),
        };
        let result = unsafe {
            host_statistics64(
                self.host,
                HOST_VM_INFO64,
                statistics.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        kern_return_check(result, "host_statistics64")?;
        read_memory_count_check(count)?;

        let statistics = unsafe { statistics.assume_init() };
        let swap: XswUsage = self.swap_usage.read()?;

        self.memory = Memory {
            total_bytes: self.memory.total_bytes,
            free_bytes: self.page_size.bytes_of(statistics.free_count.into())?,
            active_bytes: self.page_size.bytes_of(statistics.active_count.into())?,
            inactive_bytes: self.page_size.bytes_of(statistics.inactive_count.into())?,
            speculative_bytes: self
                .page_size
                .bytes_of(statistics.speculative_count.into())?,
            wired_bytes: self.page_size.bytes_of(statistics.wire_count.into())?,
            compressed_bytes: self
                .page_size
                .bytes_of(statistics.compressor_page_count.into())?,
            anonymous_bytes: self
                .page_size
                .bytes_of(statistics.internal_page_count.into())?,
            file_backed_bytes: self
                .page_size
                .bytes_of(statistics.external_page_count.into())?,
            purgeable_bytes: self.page_size.bytes_of(statistics.purgeable_count.into())?,
            swap_total_bytes: swap.total,
            swap_used_bytes: swap.used,
            swapped_in_bytes: self.page_size.bytes_of(statistics.swapins)?,
            swapped_out_bytes: self.page_size.bytes_of(statistics.swapouts)?,
            pageins: statistics.pageins,
            pageouts: statistics.pageouts,
            compressions: statistics.compressions,
            decompressions: statistics.decompressions,
            pressure: Pressure::from_level(self.pressure_level.read()?)?,
        };

        // Each count of pages converts to bytes in whole pages.
        assert!(self.memory.free_bytes.is_multiple_of(self.page_size.bytes));
        assert!(
            self.memory
                .swapped_out_bytes
                .is_multiple_of(self.page_size.bytes)
        );
        Ok(())
    }
}

/// The counts `host_processor_info` replies with: the CPUs whose loads it gives, and the
/// integers those loads take.
#[derive(Clone, Copy)]
struct ReplyCounts {
    cpu_count: u32,
    integer_count: u32,
}

/// Checks that `counts` give at least one CPU, and no more than `capacity`, in the
/// `CPU_STATE_MAX` integers of each CPU's load, and returns the CPU count.
fn read_cpus_count_check(counts: ReplyCounts, capacity: usize) -> io::Result<usize> {
    let cpu_count = usize_from_u32(counts.cpu_count);
    let integer_count = usize_from_u32(counts.integer_count);
    if cpu_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "host_processor_info replied with no CPUs",
        ));
    }
    if integer_count != cpu_count * CPU_STATE_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "host_processor_info replied with {integer_count} integers for {cpu_count} CPUs"
            ),
        ));
    }
    if cpu_count > capacity {
        return Err(io::Error::new(
            io::ErrorKind::QuotaExceeded,
            format!("macOS reported {cpu_count} CPUs past the limit of {capacity}"),
        ));
    }

    Ok(cpu_count)
}

/// Checks that `host_statistics64` wrote back `count` integers, all of `VMStatistics64`,
/// since one it left unwritten would read as a count of 0.
fn read_memory_count_check(count: u32) -> io::Result<()> {
    if usize_from_u32(count) != HOST_VM_INFO64_REV1_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("host_statistics64 wrote {count} integers of {HOST_VM_INFO64_REV1_COUNT}"),
        ));
    }

    Ok(())
}

/// Memory the kernel gave this task in a reply, `size` bytes at `address`, which it frees
/// when it drops.
struct MachMemory {
    address: *mut c_int,
    size: usize,
}

impl MachMemory {
    /// Frees the memory, which `drop` does, and returns the status `vm_deallocate` gave.
    fn drop_deallocate(&self) -> KernReturn {
        // A reply with no address makes no memory to free.
        assert!(!self.address.is_null());
        assert!(self.size.is_multiple_of(size_of::<c_int>()));

        unsafe { vm_deallocate(kernel::task_self(), self.address.addr(), self.size) }
    }
}

impl Drop for MachMemory {
    /// Frees the memory, and reports on stderr a free that failed, since a drop has no caller
    /// to return the error to. The report builds no `io::Error`, whose message allocates,
    /// since every machine read drops one and the run goes on after it.
    fn drop(&mut self) {
        assert!(!self.address.is_null());

        let returned = self.drop_deallocate();
        if returned != kernel::KERN_SUCCESS {
            eprintln!("collector: vm_deallocate returned {returned:#x}");
        }
    }
}

/// Returns how many entries a list takes to hold one for every logical CPU the machine runs,
/// which `hw.logicalcpu_max` gives.
pub fn cpu_limit() -> io::Result<usize> {
    kernel::read_limit(c"hw.logicalcpu_max")
}

// `processor_cpu_load_info.cpu_ticks` holds a core's ticks at the indices the CPU_STATE
// constants name.
const USER_INDEX: usize = cpu_ticks_index(CPU_STATE_USER);
const SYSTEM_INDEX: usize = cpu_ticks_index(CPU_STATE_SYSTEM);
const IDLE_INDEX: usize = cpu_ticks_index(CPU_STATE_IDLE);
const NICE_INDEX: usize = cpu_ticks_index(CPU_STATE_NICE);

/// Converts a `CPU_STATE` constant to the index it names in `cpu_ticks`, at compile time,
/// where `as` would do it with no check.
const fn cpu_ticks_index(state: c_int) -> usize {
    let index = match state {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        _ => panic!("a CPU_STATE constant past the end of cpu_ticks"),
    };

    assert!(index < CPU_STATE_MAX);
    index
}

const CPU_STATE_MAX: usize = 4;
const CPU_STATE_USER: c_int = 0;
const CPU_STATE_SYSTEM: c_int = 1;
const CPU_STATE_IDLE: c_int = 2;
const CPU_STATE_NICE: c_int = 3;
const PROCESSOR_CPU_LOAD_INFO: c_int = 2;

/// A core's ticks in each state, as `struct processor_cpu_load_info` declares it.
#[repr(C)]
struct ProcessorCPULoadInfo {
    cpu_ticks: [u32; CPU_STATE_MAX],
}

const _: () = assert!(size_of::<ProcessorCPULoadInfo>() == 16);
const _: () = assert!(offset_of!(ProcessorCPULoadInfo, cpu_ticks) == 0);

/// The machine's page counts, as `struct vm_statistics64` declares them through its first
/// revision. The kernel fills only the integers the call's count asks for, and the struct
/// has grown past this since.
#[repr(C)]
struct VMStatistics64 {
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

const _: () = assert!(size_of::<VMStatistics64>() == 152);
const _: () = assert!(offset_of!(VMStatistics64, free_count) == 0);
const _: () = assert!(offset_of!(VMStatistics64, active_count) == 4);
const _: () = assert!(offset_of!(VMStatistics64, inactive_count) == 8);
const _: () = assert!(offset_of!(VMStatistics64, wire_count) == 12);
const _: () = assert!(offset_of!(VMStatistics64, pageins) == 32);
const _: () = assert!(offset_of!(VMStatistics64, pageouts) == 40);
const _: () = assert!(offset_of!(VMStatistics64, purgeable_count) == 88);
const _: () = assert!(offset_of!(VMStatistics64, speculative_count) == 92);
const _: () = assert!(offset_of!(VMStatistics64, decompressions) == 96);
const _: () = assert!(offset_of!(VMStatistics64, compressions) == 104);
const _: () = assert!(offset_of!(VMStatistics64, swapins) == 112);
const _: () = assert!(offset_of!(VMStatistics64, swapouts) == 120);
const _: () = assert!(offset_of!(VMStatistics64, compressor_page_count) == 128);
const _: () = assert!(offset_of!(VMStatistics64, external_page_count) == 136);
const _: () = assert!(offset_of!(VMStatistics64, internal_page_count) == 140);

const HOST_VM_INFO64: c_int = 4;
/// The size of `VMStatistics64` in integers, the count `host_statistics64` fills, as the
/// header computes it.
const HOST_VM_INFO64_REV1_COUNT: usize = size_of::<VMStatistics64>() / size_of::<c_int>();

const _: () = assert!(size_of::<VMStatistics64>().is_multiple_of(size_of::<c_int>()));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_ticks_of_every_logical_cpu() {
        // Goal: a read gives the ticks of every logical CPU the machine runs now.
        // Method: compare the CPUs read with hw.logicalcpu read as raw bytes.
        let mut machine = Machine::new(cpu_limit().unwrap()).unwrap();

        machine.read().unwrap();

        assert_eq!(
            u32::from_ne_bytes(kernel::sysctl_bytes(c"hw.logicalcpu")),
            u32::try_from(machine.cpus.len()).unwrap()
        );
    }

    #[test]
    fn reads_the_cpu_limit_from_hw_logicalcpu_max() {
        // Goal: the CPU limit is hw.logicalcpu_max, the most logical CPUs the machine runs.
        // Method: compare the limit with hw.logicalcpu_max read as raw bytes.
        let cpus = u32::from_ne_bytes(kernel::sysctl_bytes(c"hw.logicalcpu_max"));

        assert_eq!(usize::try_from(cpus).unwrap(), cpu_limit().unwrap());
    }

    #[test]
    fn takes_the_bytes_of_pages_at_their_size() {
        // Goal: a count of pages takes as many bytes as the pages hold together.
        // Method: take the bytes of 3 pages at the 16,384 bytes this Mac's vm_page_size gave.
        let page_size = PageSize::new(16_384).unwrap();

        assert_eq!(49_152, page_size.bytes_of(3).unwrap());
    }

    #[test]
    fn refuses_a_page_size_of_0() {
        // Goal: a page size of 0 is an error, since every count of pages would read as no bytes.
        // Method: take a page size of 0.
        let page_size = PageSize::new(0);

        assert_eq!(io::ErrorKind::InvalidData, page_size.unwrap_err().kind());
    }

    #[test]
    fn refuses_pages_past_the_bytes_a_u64_holds() {
        // Goal: a count of pages whose bytes pass what a u64 holds is an error, never a
        // multiplication that traps.
        // Method: take the bytes of u64::MAX pages.
        let page_size = PageSize::new(16_384).unwrap();

        let bytes = page_size.bytes_of(u64::MAX);

        assert_eq!(io::ErrorKind::InvalidData, bytes.unwrap_err().kind());
    }

    #[test]
    #[should_panic(expected = "cpu_limit > 0")]
    fn stops_on_a_machine_with_room_for_no_cpu() {
        // Goal: a machine built with room for no CPU stops, since cpu_limit never gives 0.
        // Method: build one with a limit of 0.
        Machine::new(0).unwrap();
    }

    #[test]
    fn refuses_more_cpus_than_its_limit_without_growing() {
        // Goal: macOS reporting more CPUs than the limit is an error, and the list keeps its room.
        // Method: read this Mac, which has more than one CPU, into room for one.
        let mut machine = Machine::new(1).unwrap();
        let capacity = machine.cpus.capacity();

        let read = machine.read();

        assert_eq!(io::ErrorKind::QuotaExceeded, read.unwrap_err().kind());
        assert_eq!(capacity, machine.cpus.capacity());
    }

    #[test]
    fn takes_a_reply_of_four_integers_a_cpu() {
        // Goal: a reply holding the four ticks of each CPU it counts gives that many CPUs.
        // Method: check the counts this Mac's 18 CPUs replied with, in room for 18.
        let counts = ReplyCounts {
            cpu_count: 18,
            integer_count: 72,
        };

        assert_eq!(18, read_cpus_count_check(counts, 18).unwrap());
    }

    #[test]
    fn refuses_a_reply_of_fewer_integers_than_four_a_cpu() {
        // Goal: a reply too short for the CPUs it counts is an error, so no read passes its end.
        // Method: check 18 CPUs in 71 integers, one short.
        let counts = ReplyCounts {
            cpu_count: 18,
            integer_count: 71,
        };

        let checked = read_cpus_count_check(counts, 18);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_reply_of_more_integers_than_four_a_cpu() {
        // Goal: a reply whose integers are not the four ticks of each CPU is an error, since
        // the CPUs' ticks would then not start where a read looks for them.
        // Method: check 18 CPUs in 90 integers, five a CPU.
        let counts = ReplyCounts {
            cpu_count: 18,
            integer_count: 90,
        };

        let checked = read_cpus_count_check(counts, 18);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_reply_of_no_cpus() {
        // Goal: a reply that counts no CPUs is an error, never a machine with no CPUs.
        // Method: check a reply of no CPUs in no integers.
        let counts = ReplyCounts {
            cpu_count: 0,
            integer_count: 0,
        };

        let checked = read_cpus_count_check(counts, 18);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn reports_a_free_the_kernel_refuses() {
        // Goal: a free the kernel refuses returns the status the drop reports, never success.
        // Method: free two pages that wrap past the top of the address space, which
        // vm_deallocate refused with KERN_INVALID_ARGUMENT on macOS 27.0.1, from memory held
        // apart from its drop so the free runs once.
        let memory = std::mem::ManuallyDrop::new(MachMemory {
            address: std::ptr::without_provenance_mut(0xffff_ffff_ffff_f000),
            size: 0x2000,
        });

        let freed = memory.drop_deallocate();

        assert_ne!(kernel::KERN_SUCCESS, freed);
    }

    #[test]
    fn takes_statistics_of_the_first_revision_whole() {
        // Goal: a reply that fills every integer of the first revision is read.
        // Method: check the 38 integers host_statistics64 wrote back on macOS 27.0.1.
        assert!(read_memory_count_check(38).is_ok());
    }

    #[test]
    fn refuses_statistics_shorter_than_the_first_revision() {
        // Goal: a reply that leaves some page counts unwritten is an error, never zeros
        // read as counts.
        // Method: check a reply of 37 integers, one short.
        let checked = read_memory_count_check(37);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn reads_the_installed_ram_from_hw_memsize() {
        // Goal: the machine's RAM is what hw.memsize gives.
        // Method: compare it with hw.memsize read as raw bytes.
        let machine = Machine::new(cpu_limit().unwrap()).unwrap();

        assert_eq!(
            u64::from_ne_bytes(kernel::sysctl_bytes(c"hw.memsize")),
            machine.memory.total_bytes
        );
    }

    #[test]
    fn reads_pages_in_ram_that_fit_in_the_installed_ram() {
        // Goal: the pages read as in RAM fit in the RAM installed, and fill most of it, so each
        // page count reads in the unit the others do.
        // Method: read this Mac, and add up its free, active, inactive, wired and compressed
        // pages.
        let mut machine = Machine::new(cpu_limit().unwrap()).unwrap();

        machine.read().unwrap();

        let memory = machine.memory;
        let in_ram = memory.free_bytes
            + memory.active_bytes
            + memory.inactive_bytes
            + memory.wired_bytes
            + memory.compressed_bytes;
        assert!(in_ram <= memory.total_bytes, "{memory:?}");
        assert!(in_ram > memory.total_bytes / 2, "{memory:?}");
    }

    #[test]
    fn reads_no_more_swap_used_than_there_is() {
        // Goal: the swap read as used fits in the swap there is.
        // Method: read this Mac's swap.
        let mut machine = Machine::new(cpu_limit().unwrap()).unwrap();

        machine.read().unwrap();

        let memory = machine.memory;
        assert!(
            memory.swap_used_bytes <= memory.swap_total_bytes,
            "{memory:?}"
        );
    }

    #[test]
    fn reads_level_one_as_normal_pressure() {
        // Goal: macOS's pressure level 1 reads as normal pressure.
        // Method: read level 1.
        assert_eq!(Pressure::Normal, Pressure::from_level(1).unwrap());
    }

    #[test]
    fn reads_level_two_as_warning_pressure() {
        // Goal: macOS's pressure level 2 reads as warning pressure.
        // Method: read level 2.
        assert_eq!(Pressure::Warning, Pressure::from_level(2).unwrap());
    }

    #[test]
    fn reads_level_four_as_critical_pressure() {
        // Goal: macOS's pressure level 4 reads as critical pressure.
        // Method: read level 4.
        assert_eq!(Pressure::Critical, Pressure::from_level(4).unwrap());
    }

    #[test]
    fn refuses_a_pressure_level_macos_does_not_report() {
        // Goal: a pressure level macOS does not report is an error, never a guessed pressure.
        // Method: read level 3, between warning and critical.
        let read = Pressure::from_level(3);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }
}
