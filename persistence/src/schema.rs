//! Schema version 1 keeps source units and SQL NULL for unavailable measurements.
//! Event kinds: start=1, exec=2, exit=3. Exit observation: on-exit=0, already-exited=1.
//! Availability: measured=0, refused=1, gone=2, not-applicable=3.
//! Memory pressure: normal=0, warning=1, critical=2. These stored codes are versioned.
pub const SCHEMA_VERSION: u32 = 1;

pub mod event_kind {
    pub const START: u8 = 1;
    pub const EXEC: u8 = 2;
    pub const EXIT: u8 = 3;
}

pub mod noticed {
    pub const ON_EXIT: u8 = 0;
    pub const ALREADY_EXITED: u8 = 1;
}

pub mod availability {
    pub const MEASURED: u8 = 0;
    pub const REFUSED: u8 = 1;
    pub const GONE: u8 = 2;
    pub const NOT_APPLICABLE: u8 = 3;
}

pub mod pressure {
    pub const NORMAL: u8 = 0;
    pub const WARNING: u8 = 1;
    pub const CRITICAL: u8 = 2;
}

#[derive(Clone, Copy)]
pub(crate) enum Table {
    Ticks,
    Memory,
    Cpus,
    Interfaces,
    GpuStates,
    CoreStates,
    Fans,
    Processes,
    Coalitions,
    Events,
}

impl Table {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Ticks => "ticks",
            Self::Memory => "memory",
            Self::Cpus => "cpus",
            Self::Interfaces => "interfaces",
            Self::GpuStates => "gpu_states",
            Self::CoreStates => "core_states",
            Self::Fans => "fans",
            Self::Processes => "processes",
            Self::Coalitions => "coalitions",
            Self::Events => "events",
        }
    }
}

pub(crate) const TABLES: [(Table, &str, &str); 10] = [
    (
        Table::Ticks,
        "publication UBIGINT, run_tick UBIGINT, wall_ns UBIGINT, \
        continuous_ns UBIGINT, awake_ns UBIGINT, ioreport_read BOOLEAN, smc_read BOOLEAN, \
        gpu_energy_nj UBIGINT, power_watts FLOAT, soc_celsius FLOAT, \
        processes_refused UINTEGER, processes_gone UINTEGER, events_first UBIGINT, \
        events_end UBIGINT, dropped_starts UBIGINT, dropped_execs UBIGINT, dropped_exits UBIGINT, \
        gpu_state_count UINTEGER, core_count UINTEGER",
        "publication",
    ),
    (
        Table::Memory,
        "publication UBIGINT, total_bytes UBIGINT, free_bytes UBIGINT, \
        active_bytes UBIGINT, inactive_bytes UBIGINT, speculative_bytes UBIGINT, \
        wired_bytes UBIGINT, compressed_bytes UBIGINT, anonymous_bytes UBIGINT, \
        file_backed_bytes UBIGINT, purgeable_bytes UBIGINT, swap_total_bytes UBIGINT, \
        swap_used_bytes UBIGINT, swapped_in_bytes UBIGINT, swapped_out_bytes UBIGINT, \
        pageins UBIGINT, pageouts UBIGINT, compressions UBIGINT, decompressions UBIGINT, \
        pressure UTINYINT",
        "publication",
    ),
    (
        Table::Cpus,
        "publication UBIGINT, cpu UINTEGER, user_ticks UINTEGER, system_ticks UINTEGER, \
        nice_ticks UINTEGER, idle_ticks UINTEGER",
        "cpu, publication",
    ),
    (
        Table::Interfaces,
        "publication UBIGINT, interface USMALLINT, name VARCHAR, \
        received_bytes UBIGINT, sent_bytes UBIGINT, received_packets UBIGINT, \
        sent_packets UBIGINT, receive_errors UBIGINT, send_errors UBIGINT, \
        receive_drops UBIGINT",
        "interface, publication",
    ),
    (
        Table::GpuStates,
        "publication UBIGINT, state UINTEGER, active_ns UBIGINT",
        "state, publication",
    ),
    (
        Table::CoreStates,
        "publication UBIGINT, core UINTEGER, state UINTEGER, active_ns UBIGINT",
        "core, state, publication",
    ),
    (
        Table::Fans,
        "publication UBIGINT, fan UINTEGER, rpm FLOAT",
        "fan, publication",
    ),
    (
        Table::Processes,
        "publication UBIGINT, pid INTEGER, awake_ns_at_start UBIGINT, \
        user_ns UBIGINT, system_ns UBIGINT, energy_nj UBIGINT, instructions UBIGINT, \
        super_instructions UBIGINT, cycles UBIGINT, idle_wakeups UBIGINT, \
        interrupt_wakeups UBIGINT, pageins UBIGINT, resident_bytes UBIGINT, \
        footprint_bytes UBIGINT, disk_read_bytes UBIGINT, disk_written_bytes UBIGINT, \
        threads INTEGER, faults INTEGER, context_switches INTEGER, mach_syscalls INTEGER, \
        unix_syscalls INTEGER",
        "pid, awake_ns_at_start, publication",
    ),
    (
        Table::Coalitions,
        "publication UBIGINT, coalition UBIGINT, tasks_started UBIGINT, \
        tasks_exited UBIGINT, cpu_ns UBIGINT, interrupt_wakeups UBIGINT, idle_wakeups UBIGINT, \
        disk_read_bytes UBIGINT, disk_written_bytes UBIGINT, gpu_ns UBIGINT, energy_nj UBIGINT",
        "coalition, publication",
    ),
    (
        Table::Events,
        "publication UBIGINT, event UBIGINT, run_tick UBIGINT, pid INTEGER, \
        kind UTINYINT, coalition UBIGINT, availability UTINYINT, noticed UTINYINT, \
        awake_ns_at_start UBIGINT, user_ns UBIGINT, system_ns UBIGINT, energy_nj UBIGINT, \
        instructions UBIGINT, super_instructions UBIGINT, cycles UBIGINT, idle_wakeups UBIGINT, \
        interrupt_wakeups UBIGINT, pageins UBIGINT, resident_bytes UBIGINT, \
        footprint_bytes UBIGINT, disk_read_bytes UBIGINT, disk_written_bytes UBIGINT",
        "pid, awake_ns_at_start, event",
    ),
];
