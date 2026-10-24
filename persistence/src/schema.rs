//! Schema version 1 keeps source units and SQL NULL for unavailable measurements.
//! Event kinds: start=1, exec=2, exit=3. Exit observation: on-exit=0, already-exited=1.
//! Availability: measured=0, refused=1, gone=2, not-applicable=3.
//! Memory pressure: normal=0, warning=1, critical=2. These stored codes are versioned.
use model::catalog::{self, Column, Kind};

pub const SCHEMA_VERSION: u32 = 1;

/// Optional tick columns retain raw battery units without changing schema-1 tables or seals.
/// Every extended row carries the version, including rows with no captured reading.
pub const BATTERY_VERSION: u8 = 1;
pub(crate) const BATTERY_ROWS_MAX: u64 = 61;
pub const BATTERY_FIELDS: [(&str, &str); 12] = fields(catalog::BATTERY);

/// Optional tick columns for the temperature of the CPU and of the GPU, in degrees Celsius. A
/// window that captured neither carries neither column, and one that captured either carries
/// both, so a reader finds the pair whole or absent. A reading the SMC did not give is NULL.
pub const TEMPERATURE_FIELDS: [(&str, &str); 2] = fields(catalog::TEMPERATURES);
pub(crate) const TEMPERATURE_ROWS_MAX: u64 = 61;

/// Optional tick columns for the Mac's readings that belong to no process, core or device,
/// and for the machine's facts. A window that captured none carries none of them, and one
/// that captured any carries all, so a reader finds the group whole or absent. A reading the
/// collector did not get is NULL.
pub const SYSTEM_FIELDS: [(&str, &str); 19] = fields(catalog::SYSTEM);
pub(crate) const SYSTEM_ROWS_MAX: u64 = 61;

/// The optional column of `core_states` for the clock of each core's state, in kHz. A window
/// that knew no clock for a state holds NULL there, and one sealed before it lacks it.
pub const CORE_STATE_FREQUENCY_FIELD: (&str, &str) = field(catalog::CORE_STATE_FREQUENCY);

/// The optional column of `processes` for each process's GPU time, in nanoseconds, NULL for a
/// process that held no GPU client.
pub const PROCESS_GPU_FIELD: (&str, &str) = field(catalog::PROCESS_GPU);

/// The most columns a tick source has once every optional extension is in it.
pub const TICK_COLUMNS_MAX: usize = 64;
const _: () = assert!(catalog::TICKS.column_count() <= TICK_COLUMNS_MAX);

/// Optional process/event columns; the original schema-1 tables and seal remain unchanged.
/// A complete extension uses this version on every row. Absent columns mean legacy data.
pub const IDENTITY_VERSION: u8 = 1;
/// Admission and validation share this limit for each extended process/event table.
pub const IDENTITY_ROWS_MAX: u64 = 1_048_576;
pub const IDENTITY_PATH_BYTES_MAX: usize = collector::ring::PROGRAM_PATH_BYTES_MAX;
pub const IDENTITY_FIELDS: [(&str, &str); 7] = fields(catalog::IDENTITY);

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
    /// The table as the catalog declares it.
    #[must_use]
    pub fn catalog(self) -> &'static catalog::Table {
        match self {
            Self::Ticks => &catalog::TICKS,
            Self::Memory => &catalog::MEMORY,
            Self::Cpus => &catalog::CPUS,
            Self::Interfaces => &catalog::INTERFACES,
            Self::GpuStates => &catalog::GPU_STATES,
            Self::CoreStates => &catalog::CORE_STATES,
            Self::Fans => &catalog::FANS,
            Self::Processes => &catalog::PROCESSES,
            Self::Coalitions => &catalog::COALITIONS,
            Self::Events => &catalog::EVENTS,
        }
    }

    /// The columns every window holds, before any of the table's groups.
    pub fn columns(self) -> impl Iterator<Item = &'static str> {
        self.catalog().columns.iter().map(|column| column.name)
    }

    /// The columns every window holds, declared as SQL columns.
    #[must_use]
    pub fn declaration(self) -> String {
        declaration(self.catalog().columns)
    }

    /// The columns the table's rows are ordered by, as an SQL ordering.
    #[must_use]
    pub fn order(self) -> String {
        self.catalog().key.join(", ")
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        self.catalog().name
    }
}

/// Every table a window holds, in the order of the catalog's.
pub const TABLES: [Table; 10] = [
    Table::Ticks,
    Table::Memory,
    Table::Cpus,
    Table::Interfaces,
    Table::GpuStates,
    Table::CoreStates,
    Table::Fans,
    Table::Processes,
    Table::Coalitions,
    Table::Events,
];

/// `columns` declared as SQL columns, each its name and the name of its storage.
#[must_use]
pub fn declaration(columns: &[Column]) -> String {
    let declared: Vec<String> = columns
        .iter()
        .map(|column| format!("{} {}", column.name, storage_name(column.kind)))
        .collect();
    declared.join(", ")
}

const fn fields<const COUNT: usize>(
    columns: [Column; COUNT],
) -> [(&'static str, &'static str); COUNT] {
    let mut fields = [("", ""); COUNT];
    let mut index = 0;
    while index < COUNT {
        fields[index] = field(columns[index]);
        index += 1;
    }
    fields
}

const fn field(column: Column) -> (&'static str, &'static str) {
    (column.name, storage_name(column.kind))
}

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
