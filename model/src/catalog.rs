//! The recording's tables, each declared once: its name, the key its rows are kept in, and its
//! columns with their kinds, in the order its files hold them.
//!
//! A group of columns, such as the battery's, is one a table gained after windows without it were
//! sealed. A window holds a group whole or not at all, and one sealed before the group lacks it.
use Kind::{Bool, Bytes, F32, F64, I32, I64, Text, U8, U16, U32, U64};

/// What a column holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    U8,
    U16,
    U32,
    U64,
    I32,
    I64,
    F32,
    F64,
    /// UTF-8 text.
    Text,
    /// Bytes that need not be text, such as a path.
    Bytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: &'static str,
    pub kind: Kind,
}

pub struct Table {
    pub name: &'static str,
    /// The columns the table's rows are ordered by, most significant first.
    pub key: &'static [&'static str],
    /// The columns every window holds.
    pub columns: &'static [Column],
    /// The groups a window may hold, in the order it writes them after `columns`.
    pub groups: &'static [&'static [Column]],
}

const fn column(name: &'static str, kind: Kind) -> Column {
    Column { name, kind }
}

/// Every table a window holds, in the order a window writes them.
pub const TABLES: [&Table; 10] = [
    &TICKS,
    &MEMORY,
    &CPUS,
    &INTERFACES,
    &GPU_STATES,
    &CORE_STATES,
    &FANS,
    &PROCESSES,
    &COALITIONS,
    &EVENTS,
];

/// One row a tick: when it was read, which sources it read, and its machine-wide readings.
pub const TICKS: Table = Table {
    name: "ticks",
    key: &["publication"],
    columns: &[
        column("publication", U64),
        column("run_tick", U64),
        column("wall_ns", U64),
        column("continuous_ns", U64),
        column("awake_ns", U64),
        column("ioreport_read", Bool),
        column("smc_read", Bool),
        column("gpu_energy_nj", U64),
        column("power_watts", F32),
        column("soc_celsius", F32),
        column("processes_refused", U32),
        column("processes_gone", U32),
        column("events_first", U64),
        column("events_end", U64),
        column("dropped_starts", U64),
        column("dropped_execs", U64),
        column("dropped_exits", U64),
        column("gpu_state_count", U32),
        column("core_count", U32),
    ],
    groups: &[&BATTERY, &[PHYSICAL_DISKS], &TEMPERATURES, &SYSTEM],
};

/// One row a tick of the Mac's memory.
pub const MEMORY: Table = Table {
    name: "memory",
    key: &["publication"],
    columns: &[
        column("publication", U64),
        column("total_bytes", U64),
        column("free_bytes", U64),
        column("active_bytes", U64),
        column("inactive_bytes", U64),
        column("speculative_bytes", U64),
        column("wired_bytes", U64),
        column("compressed_bytes", U64),
        column("anonymous_bytes", U64),
        column("file_backed_bytes", U64),
        column("purgeable_bytes", U64),
        column("swap_total_bytes", U64),
        column("swap_used_bytes", U64),
        column("swapped_in_bytes", U64),
        column("swapped_out_bytes", U64),
        column("pageins", U64),
        column("pageouts", U64),
        column("compressions", U64),
        column("decompressions", U64),
        column("pressure", U8),
    ],
    groups: &[],
};

/// One row a CPU a tick, with its clock ticks in each state.
pub const CPUS: Table = Table {
    name: "cpus",
    key: &["cpu", "publication"],
    columns: &[
        column("publication", U64),
        column("cpu", U32),
        column("user_ticks", U32),
        column("system_ticks", U32),
        column("nice_ticks", U32),
        column("idle_ticks", U32),
    ],
    groups: &[],
};

/// One row a network interface a tick, with its traffic counters.
pub const INTERFACES: Table = Table {
    name: "interfaces",
    key: &["interface", "publication"],
    columns: &[
        column("publication", U64),
        column("interface", U16),
        column("name", Text),
        column("received_bytes", U64),
        column("sent_bytes", U64),
        column("received_packets", U64),
        column("sent_packets", U64),
        column("receive_errors", U64),
        column("send_errors", U64),
        column("receive_drops", U64),
    ],
    groups: &[],
};

/// One row a GPU frequency state a tick, with the time the GPU spent in it.
pub const GPU_STATES: Table = Table {
    name: "gpu_states",
    key: &["state", "publication"],
    columns: &[
        column("publication", U64),
        column("state", U32),
        column("active_ns", U64),
    ],
    groups: &[],
};

/// One row a core's frequency state a tick, with the time the core spent in it.
pub const CORE_STATES: Table = Table {
    name: "core_states",
    key: &["core", "state", "publication"],
    columns: &[
        column("publication", U64),
        column("core", U32),
        column("state", U32),
        column("active_ns", U64),
    ],
    groups: &[&[CORE_STATE_FREQUENCY]],
};

/// One row a fan a tick.
pub const FANS: Table = Table {
    name: "fans",
    key: &["fan", "publication"],
    columns: &[
        column("publication", U64),
        column("fan", U32),
        column("rpm", F32),
    ],
    groups: &[],
};

/// One row a live process a tick, with its usage counters.
pub const PROCESSES: Table = Table {
    name: "processes",
    key: &["pid", "awake_ns_at_start", "publication"],
    columns: &[
        column("publication", U64),
        column("pid", I32),
        column("awake_ns_at_start", U64),
        column("user_ns", U64),
        column("system_ns", U64),
        column("energy_nj", U64),
        column("instructions", U64),
        column("super_instructions", U64),
        column("cycles", U64),
        column("idle_wakeups", U64),
        column("interrupt_wakeups", U64),
        column("pageins", U64),
        column("resident_bytes", U64),
        column("footprint_bytes", U64),
        column("disk_read_bytes", U64),
        column("disk_written_bytes", U64),
        column("threads", I32),
        column("faults", I32),
        column("context_switches", I32),
        column("mach_syscalls", I32),
        column("unix_syscalls", I32),
    ],
    groups: &[&IDENTITY, &[PROCESS_GPU]],
};

/// One row a resource coalition a tick, with its totals.
pub const COALITIONS: Table = Table {
    name: "coalitions",
    key: &["coalition", "publication"],
    columns: &[
        column("publication", U64),
        column("coalition", U64),
        column("tasks_started", U64),
        column("tasks_exited", U64),
        column("cpu_ns", U64),
        column("interrupt_wakeups", U64),
        column("idle_wakeups", U64),
        column("disk_read_bytes", U64),
        column("disk_written_bytes", U64),
        column("gpu_ns", U64),
        column("energy_nj", U64),
    ],
    groups: &[],
};

/// One row a process start, program change or exit.
pub const EVENTS: Table = Table {
    name: "events",
    key: &["pid", "awake_ns_at_start", "event"],
    columns: &[
        column("publication", U64),
        column("event", U64),
        column("run_tick", U64),
        column("pid", I32),
        column("kind", U8),
        column("coalition", U64),
        column("availability", U8),
        column("noticed", U8),
        column("awake_ns_at_start", U64),
        column("user_ns", U64),
        column("system_ns", U64),
        column("energy_nj", U64),
        column("instructions", U64),
        column("super_instructions", U64),
        column("cycles", U64),
        column("idle_wakeups", U64),
        column("interrupt_wakeups", U64),
        column("pageins", U64),
        column("resident_bytes", U64),
        column("footprint_bytes", U64),
        column("disk_read_bytes", U64),
        column("disk_written_bytes", U64),
    ],
    groups: &[&IDENTITY],
};
/// The tick columns of the battery's raw readings, beside the version of their encoding.
pub const BATTERY: [Column; 12] = [
    column("battery_version", U8),
    column("battery_source_continuous_ns", U64),
    column("battery_charge_current", U64),
    column("battery_charge_maximum", U64),
    column("battery_full_charge_capacity", U64),
    column("battery_design_capacity", U64),
    column("battery_cycle_count", U64),
    column("battery_charging", Bool),
    column("battery_external_power", Bool),
    column("battery_voltage_mv", U64),
    column("battery_current_ma", I64),
    column("battery_adapter_watts", U64),
];

/// The tick columns of the CPU's and the GPU's temperature, in degrees Celsius.
pub const TEMPERATURES: [Column; 2] = [column("cpu_celsius", F32), column("gpu_celsius", F32)];

/// The tick columns of the Mac's readings that belong to no process, core or device, and of the
/// machine's facts.
pub const SYSTEM: [Column; 19] = [
    column("load_average_1", F64),
    column("load_average_5", F64),
    column("load_average_15", F64),
    column("open_files", U64),
    column("open_files_limit", U64),
    column("thermal_pressure", U8),
    column("gpu_memory_in_use_bytes", U64),
    column("gpu_memory_allocated_bytes", U64),
    column("boot_wall_ns", U64),
    column("machine_model", Text),
    column("machine_chip", Text),
    column("machine_architecture", Text),
    column("machine_cpu_cores", U32),
    column("machine_gpu_cores", U32),
    column("machine_memory_bytes", U64),
    column("machine_performance_level0_name", Text),
    column("machine_performance_level0_logical_cpus", U32),
    column("machine_performance_level1_name", Text),
    column("machine_performance_level1_logical_cpus", U32),
];

/// The process and event columns of each process's identity and of how its usage was read.
pub const IDENTITY: [Column; 7] = [
    column("identity_version", U8),
    column("identity_availability", U8),
    column("usage_availability", U8),
    column("process_unique_id", U64),
    column("pid_version", I32),
    column("executable_name", Bytes),
    column("app_path", Bytes),
];
/// The tick column of the Mac's physical disks, as text.
pub const PHYSICAL_DISKS: Column = column("physical_disks", Text);

/// The core state column of the clock of each state, in kHz.
pub const CORE_STATE_FREQUENCY: Column = column("frequency_khz", U32);

/// The process column of each process's GPU time, in nanoseconds.
pub const PROCESS_GPU: Column = column("gpu_ns", U64);

const _: () = {
    let mut table = 0;
    while table < TABLES.len() {
        assert!(names_unique(TABLES[table]));
        assert!(key_declared(TABLES[table]));
        table += 1;
    }
};

/// Whether no two of a table's columns, its groups' among them, share a name.
const fn names_unique(table: &Table) -> bool {
    let mut first = 0;
    while first < column_count(table) {
        let mut second = first + 1;
        while second < column_count(table) {
            let (left, right) = (column_at(table, first), column_at(table, second));
            if text_equal(left.name, right.name) {
                return false;
            }
            second += 1;
        }
        first += 1;
    }
    true
}

/// Whether each column of a table's key is one of the columns every window holds.
const fn key_declared(table: &Table) -> bool {
    let mut key = 0;
    while key < table.key.len() {
        let mut found = false;
        let mut index = 0;
        while index < table.columns.len() {
            found = found || text_equal(table.key[key], table.columns[index].name);
            index += 1;
        }
        if !found {
            return false;
        }
        key += 1;
    }
    true
}

const fn column_count(table: &Table) -> usize {
    let mut count = table.columns.len();
    let mut group = 0;
    while group < table.groups.len() {
        count += table.groups[group].len();
        group += 1;
    }
    count
}

const fn column_at(table: &Table, index: usize) -> Column {
    if index < table.columns.len() {
        return table.columns[index];
    }
    let mut rest = index - table.columns.len();
    let mut group = 0;
    while rest >= table.groups[group].len() {
        rest -= table.groups[group].len();
        group += 1;
    }
    table.groups[group][rest]
}

const fn text_equal(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}
