//! Schema version 1 keeps source units and SQL NULL for unavailable measurements.
//! Event kinds: start=1, exec=2, exit=3. Exit observation: on-exit=0, already-exited=1.
//! Availability: measured=0, refused=1, gone=2, not-applicable=3.
//! Memory pressure: normal=0, warning=1, critical=2. These stored codes are versioned.
use model::catalog::Kind;

pub const SCHEMA_VERSION: u32 = 1;

/// Optional tick columns retain raw battery units without changing schema-1 tables or seals.
/// Every extended row carries the version, including rows with no captured reading.
pub const BATTERY_VERSION: u8 = 1;
pub(crate) const BATTERY_ROWS_MAX: u64 = 61;
pub const BATTERY_FIELDS: [(&str, &str); 12] = [
    ("battery_version", "UTINYINT"),
    ("battery_source_continuous_ns", "UBIGINT"),
    ("battery_charge_current", "UBIGINT"),
    ("battery_charge_maximum", "UBIGINT"),
    ("battery_full_charge_capacity", "UBIGINT"),
    ("battery_design_capacity", "UBIGINT"),
    ("battery_cycle_count", "UBIGINT"),
    ("battery_charging", "BOOLEAN"),
    ("battery_external_power", "BOOLEAN"),
    ("battery_voltage_mv", "UBIGINT"),
    ("battery_current_ma", "BIGINT"),
    ("battery_adapter_watts", "UBIGINT"),
];

/// Optional tick columns for the temperature of the CPU and of the GPU, in degrees Celsius. A
/// window that captured neither carries neither column, and one that captured either carries
/// both, so a reader finds the pair whole or absent. A reading the SMC did not give is NULL.
pub const TEMPERATURE_FIELDS: [(&str, &str); 2] =
    [("cpu_celsius", "FLOAT"), ("gpu_celsius", "FLOAT")];
pub(crate) const TEMPERATURE_ROWS_MAX: u64 = 61;

/// Optional tick columns for the Mac's readings that belong to no process, core or device,
/// and for the machine's facts. A window that captured none carries none of them, and one
/// that captured any carries all, so a reader finds the group whole or absent. A reading the
/// collector did not get is NULL.
pub const SYSTEM_FIELDS: [(&str, &str); 19] = [
    ("load_average_1", "DOUBLE"),
    ("load_average_5", "DOUBLE"),
    ("load_average_15", "DOUBLE"),
    ("open_files", "UBIGINT"),
    ("open_files_limit", "UBIGINT"),
    ("thermal_pressure", "UTINYINT"),
    ("gpu_memory_in_use_bytes", "UBIGINT"),
    ("gpu_memory_allocated_bytes", "UBIGINT"),
    ("boot_wall_ns", "UBIGINT"),
    ("machine_model", "VARCHAR"),
    ("machine_chip", "VARCHAR"),
    ("machine_architecture", "VARCHAR"),
    ("machine_cpu_cores", "UINTEGER"),
    ("machine_gpu_cores", "UINTEGER"),
    ("machine_memory_bytes", "UBIGINT"),
    ("machine_performance_level0_name", "VARCHAR"),
    ("machine_performance_level0_logical_cpus", "UINTEGER"),
    ("machine_performance_level1_name", "VARCHAR"),
    ("machine_performance_level1_logical_cpus", "UINTEGER"),
];
pub(crate) const SYSTEM_ROWS_MAX: u64 = 61;

/// The optional column of `core_states` for the clock of each core's state, in kHz. A window
/// that knew no clock for a state holds NULL there, and one sealed before it lacks it.
pub const CORE_STATE_FREQUENCY_FIELD: (&str, &str) = ("frequency_khz", "UINTEGER");

/// The optional column of `processes` for each process's GPU time, in nanoseconds, NULL for a
/// process that held no GPU client.
pub const PROCESS_GPU_FIELD: (&str, &str) = ("gpu_ns", "UBIGINT");

/// The most columns a tick source has once every optional extension is in it.
pub const TICK_COLUMNS_MAX: usize = 64;
const _: () = assert!(
    19 + BATTERY_FIELDS.len() + 1 + TEMPERATURE_FIELDS.len() + SYSTEM_FIELDS.len()
        <= TICK_COLUMNS_MAX
);

/// Optional process/event columns; the original schema-1 tables and seal remain unchanged.
/// A complete extension uses this version on every row. Absent columns mean legacy data.
pub const IDENTITY_VERSION: u8 = 1;
/// Admission and validation share this limit for each extended process/event table.
pub const IDENTITY_ROWS_MAX: u64 = 1_048_576;
pub const IDENTITY_PATH_BYTES_MAX: usize = collector::ring::PROGRAM_PATH_BYTES_MAX;
pub const IDENTITY_FIELDS: [(&str, &str); 7] = [
    ("identity_version", "UTINYINT"),
    ("identity_availability", "UTINYINT"),
    ("usage_availability", "UTINYINT"),
    ("process_unique_id", "UBIGINT"),
    ("pid_version", "INTEGER"),
    ("executable_name", "BLOB"),
    ("app_path", "BLOB"),
];

pub mod identity_availability {
    pub const MEASURED: u8 = 0;
    pub const REFUSED: u8 = 1;
    pub const GONE: u8 = 2;
    pub const UNSUPPORTED: u8 = 3;
    pub const CHANGED: u8 = 4;
    pub const INVALID: u8 = 5;
    pub const EXHAUSTED: u8 = 6;
}

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

/// Tables a window derives from its own tables when it seals, which a reader can rebuild from
/// them. A window holds each at most once, and a window sealed before one existed lacks it.
pub const DERIVED_TABLES: [&str; 2] = [APP_RANKS, APP_PARTS];
/// The Apps that used the most of each resource in each of the window's observations.
pub const APP_RANKS: &str = "app_ranks";
/// Every App's row and processes in each of the window's observations.
pub const APP_PARTS: &str = "app_parts";

#[derive(Clone, Copy)]
pub enum Table {
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
    /// The original schema columns, excluding optional extensions.
    pub fn columns(self) -> impl Iterator<Item = &'static str> {
        let Some((_, columns, _)) = TABLES.iter().find(|entry| entry.0.name() == self.name())
        else {
            unreachable!("every table has a schema");
        };

        columns.split(',').map(|column| {
            let Some((name, _)) = column.trim().split_once(' ') else {
                unreachable!("every schema column has a type");
            };

            name
        })
    }

    #[must_use]
    pub fn name(self) -> &'static str {
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

pub const TABLES: [(Table, &str, &str); 10] = [
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

/// The name the recording's SQL gives the storage of `kind`.
#[must_use]
pub const fn storage_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Bool => "BOOLEAN",
        Kind::U8 => "UTINYINT",
        Kind::U16 => "USMALLINT",
        Kind::U32 => "UINTEGER",
        Kind::U64 => "UBIGINT",
        Kind::I32 => "INTEGER",
        Kind::I64 => "BIGINT",
        Kind::F32 => "FLOAT",
        Kind::F64 => "DOUBLE",
        Kind::Text => "VARCHAR",
        Kind::Bytes => "BLOB",
    }
}

#[cfg(test)]
mod tests {
    use model::catalog::{self, Column};

    use super::*;

    #[test]
    fn the_catalog_declares_each_table_as_the_schema_does() {
        // Goal: the catalog names every table, key, column and kind the schema declares.
        // Method: compare each catalog table with the schema's definition of the same table.
        for (declared, (table, definition, order)) in catalog::TABLES.iter().zip(TABLES) {
            let expected: Vec<(&str, &str)> = definition
                .split(',')
                .map(|column| column.trim().split_once(' ').unwrap())
                .collect();

            assert_eq!(declared.name, table.name());
            assert_eq!(declared.key.join(", "), order);
            assert_eq!(fields(declared.columns), expected);
        }
    }

    #[test]
    fn the_catalog_declares_each_group_as_the_schema_does() {
        // Goal: the catalog's groups hold the columns and kinds of the schema's extensions.
        // Method: compare each group with the schema's fields of the same extension.
        let groups: [(&[Column], Fields); 7] = [
            (&catalog::BATTERY, &BATTERY_FIELDS),
            (&[catalog::PHYSICAL_DISKS], &[("physical_disks", "VARCHAR")]),
            (&catalog::TEMPERATURES, &TEMPERATURE_FIELDS),
            (&catalog::SYSTEM, &SYSTEM_FIELDS),
            (&catalog::IDENTITY, &IDENTITY_FIELDS),
            (
                &[catalog::CORE_STATE_FREQUENCY],
                &[CORE_STATE_FREQUENCY_FIELD],
            ),
            (&[catalog::PROCESS_GPU], &[PROCESS_GPU_FIELD]),
        ];

        for (group, expected) in groups {
            assert_eq!(fields(group), expected);
        }
    }

    /// A schema's columns, each named with the name of its storage.
    type Fields = &'static [(&'static str, &'static str)];

    fn fields(columns: &[Column]) -> Vec<(&'static str, &'static str)> {
        columns
            .iter()
            .map(|column| (column.name, storage_name(column.kind)))
            .collect()
    }
}
