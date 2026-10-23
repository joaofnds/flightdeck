//! The format of the tables a window derives of its Apps as it seals: the ranks of each tick's
//! Apps, and every App's part of each tick. The persister writes them and the viewer reads
//! them, so each column and code is declared here once.
use model::apps::{ProcessState, ProgramUnavailable, Unavailable};
use model::programs::AppKey;
use model::ranks::{METRICS, Metric};

use crate::schema::identity_availability;

/// The version of the ranks table's columns. A reader takes no ranks from a table of another
/// version and leaves its window to the replay, so a later build may change the columns and
/// an older one still reads the recording.
pub const RANKS_VERSION: u8 = 2;
/// The columns of the ranks table, in their order: those of the tick, those of one ranking,
/// and those of the App at a position in it.
pub const RANKS_COLUMNS: [&str; 20] = [
    "version",
    "publication",
    "run_tick",
    "wall_ns",
    "continuous_ns",
    "awake_ns",
    "app_count",
    "membership_complete",
    "ungrouped_members",
    "metric",
    "unavailable",
    "overflowed",
    "partial",
    "position",
    "app_kind",
    "app_key",
    "amount",
    "rate",
    "measured_members",
    "unavailable_members",
];

/// The version of the parts table's columns. A reader takes no Apps from a table of another
/// version and leaves its minute to the App rule, so a later build may change the columns and
/// an older one still reads the recording.
pub const PARTS_VERSION: u8 = 3;
/// The rows a minute keeps. About 1,100 processes in a few hundred Apps a second make some
/// 85,000 rows a minute, a sixth of this.
pub const PARTS_ROWS_MAX: u32 = 1 << 19;
/// A row holds the facts of a tick, one of its Apps, or one of an App's processes.
pub const ROW_HEADER: u8 = 0;
pub const ROW_APP: u8 = 1;
pub const ROW_PROCESS: u8 = 2;
/// The columns of the parts table before the measures and amounts, in their order: those of
/// every row, those only a row of a tick's facts fills, those of an App and of a process row,
/// those only an App row fills, and those only a process row fills.
pub const PARTS_FIXED_COLUMNS: [&str; 32] = [
    "version",
    "publication",
    "row_kind",
    "run_tick",
    "wall_ns",
    "continuous_ns",
    "awake_ns",
    "previous_publication",
    "previous_continuous_ns",
    "previous_awake_ns",
    "elapsed_ns",
    "membership_complete",
    "ungrouped_members",
    "live_ungrouped_members",
    "usage_refused",
    "usage_gone",
    "app_count",
    "process_count",
    "app_index",
    "app_kind",
    "app_key",
    "members",
    "live_members",
    "app_membership_complete",
    "process_index",
    "pid",
    "unique_id",
    "pid_version",
    "executable_name",
    "program_unavailable",
    "app_attribution",
    "state",
];
/// An App row's measures, each in a column for every field of `MEASURE_FIELDS`, after the
/// fixed columns.
pub const MEASURES: [&str; 8] = [
    "cpu_ns",
    "energy_nj",
    "disk_read_bytes",
    "disk_written_bytes",
    "disk_io_bytes",
    "resident_bytes",
    "footprint_bytes",
    "gpu_ns",
];
pub const MEASURE_FIELDS: [&str; 4] = [
    "known",
    "measured_members",
    "unavailable_members",
    "overflowed",
];
/// A process row's amounts, each in a column of its value and one of why it has none, after
/// the measures.
pub const AMOUNTS: [&str; 11] = [
    "cpu_ns",
    "energy_nj",
    "disk_read_bytes",
    "disk_written_bytes",
    "disk_io_bytes",
    "resident_bytes",
    "footprint_bytes",
    "idle_wakeups",
    "pageins",
    "threads",
    "gpu_ns",
];
pub const PARTS_COLUMNS: usize =
    PARTS_FIXED_COLUMNS.len() + MEASURES.len() * MEASURE_FIELDS.len() + AMOUNTS.len() * 2;

/// A table keeps each kind, reason, state and metric by these codes, so a build that changed
/// one would misread the tables earlier builds wrote at the same version.
const _: () = {
    assert!(kind_code(AppKey::Bundle(&[])) == 0);
    assert!(kind_code(AppKey::Executable(&[])) == 1);
    assert!(unavailable_code(Unavailable::Refused) == 0);
    assert!(unavailable_code(Unavailable::Gone) == 1);
    assert!(unavailable_code(Unavailable::Unknown) == 2);
    assert!(unavailable_code(Unavailable::Interval) == 3);
    assert!(unavailable_code(Unavailable::Creation) == 4);
    assert!(unavailable_code(Unavailable::Baseline) == 5);
    assert!(unavailable_code(Unavailable::CounterReset) == 6);
    assert!(unavailable_code(Unavailable::Overflow) == 7);
    assert!(unavailable_code(Unavailable::Exec) == 8);
    assert!(unavailable_code(Unavailable::Program) == 9);
    assert!(unavailable_code(Unavailable::ExitUnverified) == 10);
    assert!(state_code(ProcessState::Live) == 0);
    assert!(state_code(ProcessState::ExitedVerified) == 1);
    assert!(state_code(ProcessState::ExitedUnverified) == 2);
    // A row names its ranking by its place among the rankings of a tick.
    assert!(METRICS.len() == 5);
    assert!(metric_code(METRICS[0]) == 0);
    assert!(metric_code(METRICS[1]) == 1);
    assert!(metric_code(METRICS[2]) == 2);
    assert!(metric_code(METRICS[3]) == 3);
    assert!(metric_code(METRICS[4]) == 4);
};

/// The names of the columns of `measure`, one for each of `MEASURE_FIELDS`.
#[must_use]
pub fn measure_column_names(measure: &str) -> [String; 4] {
    MEASURE_FIELDS.map(|field| format!("app_{measure}_{field}"))
}

/// The names of the columns of `amount`, its value and why it has none.
#[must_use]
pub fn amount_column_names(amount: &str) -> [String; 2] {
    [
        format!("process_{amount}"),
        format!("process_{amount}_unavailable"),
    ]
}

#[must_use]
pub const fn kind_code(key: AppKey<'_>) -> u8 {
    match key {
        AppKey::Bundle(_) => 0,
        AppKey::Executable(_) => 1,
    }
}

#[must_use]
pub const fn unavailable_code(reason: Unavailable) -> u8 {
    match reason {
        Unavailable::Refused => 0,
        Unavailable::Gone => 1,
        Unavailable::Unknown => 2,
        Unavailable::Interval => 3,
        Unavailable::Creation => 4,
        Unavailable::Baseline => 5,
        Unavailable::CounterReset => 6,
        Unavailable::Overflow => 7,
        Unavailable::Exec => 8,
        Unavailable::Program => 9,
        Unavailable::ExitUnverified => 10,
    }
}

/// Why the collector did not read a program, by the code the processes table gives it.
#[must_use]
pub const fn program_code(reason: ProgramUnavailable) -> u8 {
    match reason {
        ProgramUnavailable::Refused => identity_availability::REFUSED,
        ProgramUnavailable::Gone => identity_availability::GONE,
        ProgramUnavailable::Unsupported => identity_availability::UNSUPPORTED,
        ProgramUnavailable::Changed => identity_availability::CHANGED,
        ProgramUnavailable::Invalid => identity_availability::INVALID,
        ProgramUnavailable::Exhausted => identity_availability::EXHAUSTED,
    }
}

#[must_use]
pub const fn state_code(state: ProcessState) -> u8 {
    match state {
        ProcessState::Live => 0,
        ProcessState::ExitedVerified => 1,
        ProcessState::ExitedUnverified => 2,
    }
}

#[must_use]
pub const fn metric_code(metric: Metric) -> u8 {
    match metric {
        Metric::Cpu => 0,
        Metric::Resident => 1,
        Metric::Power => 2,
        Metric::Disk => 3,
        Metric::Gpu => 4,
    }
}
