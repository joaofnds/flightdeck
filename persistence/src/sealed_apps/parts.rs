//! Every App's part of each tick of a sealed window: its row, its processes, and the facts of
//! the tick every page shows, so the viewer shows a saved second's App without replaying its
//! minute.
use std::path::Path;

use polars::prelude::{Column, DataFrame};

use super::{TickApps, WindowApps};
use crate::app_tables::{
    AMOUNTS, MEASURES, PARTS_COLUMNS, PARTS_FIXED_COLUMNS, PARTS_VERSION, ROW_APP, ROW_HEADER,
    ROW_PROCESS, amount_column_names, kind_code, measure_column_names, program_code, state_code,
    unavailable_code,
};
use crate::encoder::columns;
use crate::schema::APP_PARTS;
use crate::window::WINDOW_TICKS_MAX;
use crate::{Error, Result, TableManifest};
use model::apps::{Amount, ProcessTotals, Subtotal, Totals, Unavailable};
use model::programs::{AppKey, Programs};

/// What the engine may decode of a window's table, below what it admits of a file. A row
/// decodes to at most 8 bytes a column, 16 more for each of its two columns of bytes and what
/// parquet adds to each, and the bytes of its App's key and its executable's name, so long
/// keys may refuse a window before its rows reach their bound.
pub(super) const DECODED_BYTES_MAX: u64 = 384 * 1024 * 1024;
const ROW_DECODED_BYTES: usize = PARTS_COLUMNS * 8 + 2 * 24;

const _: () = {
    assert!(PARTS_COLUMNS <= crate::engine::PARQUET_COLUMNS_MAX);
    assert!(DECODED_BYTES_MAX < crate::engine::PARQUET_EXPANDED_BYTES_MAX);
};

/// Writes every App's part of the window's ticks into its pending directory, in the table the
/// window seals with.
pub(super) fn encode(
    window: &WindowApps,
    programs: &Programs,
    pending: &Path,
) -> Result<TableManifest> {
    let mut frame = encode_frame(window, programs)?;

    crate::encode_derived(APP_PARTS, &mut frame, pending)
}

/// One row of the table.
#[derive(Clone, Copy)]
pub(super) enum PartRow<'window> {
    Header {
        tick: &'window TickApps,
    },
    App {
        tick: &'window TickApps,
        index: u32,
        totals: &'window Totals,
    },
    Process {
        tick: &'window TickApps,
        app_index: u32,
        totals: &'window Totals,
        index: u32,
        process: &'window ProcessTotals,
        /// The entry of the process's program in the run's index.
        entry: u32,
    },
}

impl<'window> PartRow<'window> {
    fn tick(&self) -> &'window TickApps {
        match self {
            Self::Header { tick } | Self::App { tick, .. } | Self::Process { tick, .. } => tick,
        }
    }

    /// The row's place in the table's order, one integer the rows sort by alone. From its most
    /// significant bits: the row's kind, the position of its App's key among the window's App
    /// keys, a process row's pid and the version of its pid, and `observation`, the place of
    /// the row's tick among the window's in their order.
    fn order(&self, app_position: Option<u32>, observation: u8) -> u128 {
        let pid = self.described_process().map(|process| process.pid);
        let pid_version = self
            .described_process()
            .and_then(|process| process.creation)
            .map(|creation| creation.pid_version);

        let mut order = u128::from(self.kind());
        order = order << ORDER_FIELD_BITS | order_field(app_position);
        order = order << ORDER_FIELD_BITS | order_field(pid.map(order_signed));
        order = order << ORDER_FIELD_BITS | order_field(pid_version.map(order_signed));
        order << u8::BITS | u128::from(observation)
    }

    pub(super) fn kind(&self) -> u8 {
        match self {
            Self::Header { .. } => ROW_HEADER,
            Self::App { .. } => ROW_APP,
            Self::Process { .. } => ROW_PROCESS,
        }
    }

    /// The tick of a row of its facts.
    fn described_tick(&self) -> Option<&'window TickApps> {
        match self {
            Self::Header { tick } => Some(tick),
            Self::App { .. } | Self::Process { .. } => None,
        }
    }

    /// The App of an App row or a process row, and its place among its tick's Apps.
    fn app(&self) -> Option<(u32, &'window Totals)> {
        match self {
            Self::Header { .. } => None,
            Self::App { index, totals, .. } => Some((*index, totals)),
            Self::Process {
                app_index, totals, ..
            } => Some((*app_index, totals)),
        }
    }

    /// The App of an App row, which a process row of the App does not describe.
    fn described_app(&self) -> Option<&'window Totals> {
        match self {
            Self::App { totals, .. } => Some(totals),
            Self::Header { .. } | Self::Process { .. } => None,
        }
    }

    /// The process of a process row, its place among its tick's processes, and its program's
    /// entry.
    pub(super) fn process(&self) -> Option<(u32, &'window ProcessTotals, u32)> {
        match self {
            Self::Process {
                index,
                process,
                entry,
                ..
            } => Some((*index, process, *entry)),
            Self::Header { .. } | Self::App { .. } => None,
        }
    }

    /// The process of a process row.
    fn described_process(&self) -> Option<&'window ProcessTotals> {
        self.process().map(|(_index, process, _entry)| process)
    }
}

/// The bits of a field of a row's place in the table's order, one more than its value's to put
/// an absent field first.
const ORDER_FIELD_BITS: u32 = u32::BITS + 1;

fn order_field(value: Option<u32>) -> u128 {
    value.map_or(0, |value| 1 << u32::BITS | u128::from(value))
}

/// A signed field of a row's order as an unsigned one, in the same order.
fn order_signed(value: i32) -> u32 {
    value.abs_diff(i32::MIN)
}

const _: () = {
    // The row's kind, its App, its pid and pid version, and its tick.
    assert!(u8::BITS + 3 * ORDER_FIELD_BITS + u8::BITS <= u128::BITS);
    assert!(WINDOW_TICKS_MAX <= 1 << 8);
};

fn encode_frame(window: &WindowApps, programs: &Programs) -> Result<DataFrame> {
    let rows = encode_rows(window, programs)?;
    if encode_decoded_bytes(&rows, programs) > DECODED_BYTES_MAX {
        return Err(Error::Bound("sealed parts bytes"));
    }

    let mut columns = encode_header_columns(&rows);
    columns.extend(encode_member_columns(&rows, programs));
    columns.extend(encode_measure_columns(&rows));
    columns.extend(encode_amount_columns(&rows));
    let names = column_names();
    assert!(
        columns
            .iter()
            .map(|column| column.name().as_str())
            .eq(names.iter().map(String::as_str))
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}

/// A row for each tick's facts, one for each of its Apps, and one for each process of an App.
/// A process outside every App belongs to no App's part.
pub(super) fn encode_rows<'window>(
    window: &'window WindowApps,
    programs: &Programs,
) -> Result<Vec<PartRow<'window>>> {
    if window.parts_full() {
        return Err(Error::Bound("sealed parts rows"));
    }
    assert!(window.ticks.len() <= WINDOW_TICKS_MAX);

    // A row changes little from the row of the same App or process a second before, so keeping
    // them side by side halves the table. The reader orders the rows it reads by publication.
    let positions = encode_positions(window, programs);
    let count = window.ticks.len() + window.totals.len() + window.processes.len();
    let mut rows = Vec::with_capacity(count);
    let mut orders = Vec::with_capacity(count);
    for (observation, tick) in (0_u8..).zip(&window.ticks) {
        // The rows of one App or process keep their ticks' order only from ticks that come in
        // it.
        if let Some(before) = observation.checked_sub(1) {
            let before = &window.ticks[usize::from(before)];
            assert!(before.observation.publication < tick.observation.publication);
        }

        let push = Push {
            window,
            programs,
            positions: &positions,
            tick,
            observation,
        };
        push.rows(&mut rows, &mut orders);
    }
    assert!(rows.len() <= count);
    assert_eq!(orders.len(), rows.len());
    orders.sort_unstable_by_key(|(order, _row)| *order);

    Ok(orders.iter().map(|(_order, row)| rows[*row]).collect())
}

/// The ranks among the run's Apps of every App of the window, in order and each once, so an
/// App's place among them is its key's place among the window's.
fn encode_positions(window: &WindowApps, programs: &Programs) -> Vec<u32> {
    let mut positions: Vec<u32> = window
        .totals
        .iter()
        .map(|totals| programs.rank(totals.app))
        .collect();
    positions.sort_unstable();
    positions.dedup();

    positions
}

/// What puts the rows of one tick of the window beside their places in the table's order.
struct Push<'push, 'window> {
    window: &'window WindowApps,
    programs: &'push Programs,
    positions: &'push [u32],
    tick: &'window TickApps,
    /// The tick's place among the window's.
    observation: u8,
}

impl<'window> Push<'_, 'window> {
    /// Pushes every row of the tick, and its place in the table's order beside the place of the
    /// row in `rows`.
    fn rows(&self, rows: &mut Vec<PartRow<'window>>, orders: &mut Vec<(u128, usize)>) {
        let tick = self.tick;
        let totals = &self.window.totals[tick.totals.clone()];
        let processes = &self.window.processes[tick.processes.clone()];

        let header = PartRow::Header { tick };
        orders.push((header.order(None, self.observation), rows.len()));
        rows.push(header);
        for (index, app) in (0_u32..).zip(totals) {
            let row = PartRow::App {
                tick,
                index,
                totals: app,
            };
            orders.push((
                row.order(Some(self.position(app)), self.observation),
                rows.len(),
            ));
            rows.push(row);
        }
        for (index, process) in (0_u32..).zip(processes) {
            if let Ok(entry) = process.program {
                let app_index = self.app_index(totals, entry);
                let app = &totals[model::usize_from_u32(app_index)];
                let row = PartRow::Process {
                    tick,
                    app_index,
                    totals: app,
                    index,
                    process,
                    entry,
                };
                orders.push((
                    row.order(Some(self.position(app)), self.observation),
                    rows.len(),
                ));
                rows.push(row);
            }
        }
    }

    /// The place of the key of `app` among the window's App keys.
    fn position(&self, app: &Totals) -> u32 {
        let rank = self.programs.rank(app.app);
        match self.positions.binary_search(&rank) {
            Ok(position) => model::u32_from_length(position),
            Err(_) => unreachable!("the window's positions hold every App of its ticks"),
        }
    }

    /// The place among the tick's Apps, which come in key order, of the App of the program
    /// `entry`.
    fn app_index(&self, totals: &[Totals], entry: u32) -> u32 {
        let rank = self.programs.rank(self.programs.app(entry));
        match totals.binary_search_by_key(&rank, |app| self.programs.rank(app.app)) {
            Ok(index) => model::u32_from_length(index),
            Err(_) => unreachable!("a process of a program is a member of its App"),
        }
    }
}

/// What the engine may decode of the rows, by the rule `DECODED_BYTES_MAX` gives.
fn encode_decoded_bytes(rows: &[PartRow<'_>], programs: &Programs) -> u64 {
    rows.iter()
        .map(|row| {
            let key = row
                .app()
                .map_or(0, |(_index, app)| programs.key(app.app).bytes().len());
            let name = row.process().map_or(0, |(_index, _process, entry)| {
                programs.program(entry).executable_name.len()
            });
            collector::u64_from_usize(ROW_DECODED_BYTES + key + name)
        })
        .sum()
}

fn encode_header_columns(rows: &[PartRow<'_>]) -> Vec<Column> {
    let previous = |row: &PartRow<'_>| row.described_tick().and_then(|tick| tick.previous);

    columns!(rows;
        "version" => |_| PARTS_VERSION,
        "publication" => |row| row.tick().observation.publication,
        "row_kind" => PartRow::kind,
        "run_tick" => |row| row.described_tick().map(|tick| tick.run_tick),
        "wall_ns" => |row| row.described_tick().and_then(|tick| tick.observation.wall_ns),
        "continuous_ns" => |row| {
            row.described_tick().and_then(|tick| tick.observation.continuous_ns)
        },
        "awake_ns" => |row| row.described_tick().and_then(|tick| tick.observation.awake_ns),
        "previous_publication" => |row| previous(row).map(|observation| observation.publication),
        "previous_continuous_ns" => |row| {
            previous(row).and_then(|observation| observation.continuous_ns)
        },
        "previous_awake_ns" => |row| previous(row).and_then(|observation| observation.awake_ns),
        "elapsed_ns" => |row| row.described_tick().and_then(|tick| tick.elapsed_ns),
        // The App rule holds no tick's membership complete, which readers of this version
        // still take.
        "membership_complete" => |row| row.described_tick().map(|_| false),
        "ungrouped_members" => |row| row.described_tick().map(|tick| tick.ungrouped_members),
        "live_ungrouped_members" => |row| {
            row.described_tick().map(|tick| tick.live_ungrouped_members)
        },
        "usage_refused" => |row| row.described_tick().map(|tick| tick.usage_refused),
        "usage_gone" => |row| row.described_tick().map(|tick| tick.usage_gone),
        "app_count" => |row| row.described_tick().map(|tick| u64::from(tick.app_count)),
        "process_count" => |row| row.described_tick().map(|tick| u64::from(tick.process_count)),
    )
}

fn encode_member_columns(rows: &[PartRow<'_>], programs: &Programs) -> Vec<Column> {
    let key = |row: &PartRow<'_>| row.app().map(|(_index, app)| programs.key(app.app));
    let creation = |row: &PartRow<'_>| row.described_process().and_then(|process| process.creation);

    columns!(rows;
        "app_index" => |row| row.app().map(|(index, _app)| index),
        "app_kind" => |row| key(row).map(kind_code),
        "app_key" => |row| key(row).map(AppKey::bytes),
        "members" => |row| row.described_app().map(|app| app.members),
        "live_members" => |row| row.described_app().map(|app| app.live_members),
        "app_membership_complete" => |row| row.described_app().map(|_| false),
        "process_index" => |row| row.process().map(|(index, _process, _entry)| index),
        "pid" => |row| row.described_process().map(|process| process.pid),
        "unique_id" => |row| creation(row).map(|creation| creation.unique_id),
        "pid_version" => |row| creation(row).map(|creation| creation.pid_version),
        "executable_name" => |row| {
            row.process()
                .map(|(_index, _process, entry)| programs.program(entry).executable_name)
        },
        "program_unavailable" => |row| {
            row.described_process()
                .and_then(|process| process.program.err())
                .map(program_code)
        },
        "app_attribution" => |row| {
            row.described_process()
                .and_then(|process| process.app_attribution)
                .map(unavailable_code)
        },
        "state" => |row| row.described_process().map(|process| state_code(process.state)),
    )
}

fn encode_measure_columns(rows: &[PartRow<'_>]) -> Vec<Column> {
    let mut columns = Vec::with_capacity(MEASURES.len() * 4);
    for measure in MEASURES {
        let value = measure_value(measure);
        let values: Vec<Option<&Subtotal>> = rows
            .iter()
            .map(|row| row.described_app().map(value))
            .collect();
        let [known, measured, unavailable, overflowed] = measure_column_names(measure);
        columns.push(Column::new(
            known.into(),
            values
                .iter()
                .map(|subtotal| subtotal.map(|subtotal| subtotal.known))
                .collect::<Vec<_>>(),
        ));
        columns.push(Column::new(
            measured.into(),
            values
                .iter()
                .map(|subtotal| subtotal.map(|subtotal| subtotal.measured_members))
                .collect::<Vec<_>>(),
        ));
        columns.push(Column::new(
            unavailable.into(),
            values
                .iter()
                .map(|subtotal| subtotal.map(|subtotal| subtotal.unavailable_members))
                .collect::<Vec<_>>(),
        ));
        columns.push(Column::new(
            overflowed.into(),
            values
                .iter()
                .map(|subtotal| subtotal.map(|subtotal| subtotal.overflowed))
                .collect::<Vec<_>>(),
        ));
    }

    columns
}

fn encode_amount_columns(rows: &[PartRow<'_>]) -> Vec<Column> {
    let mut columns = Vec::with_capacity(AMOUNTS.len() * 2);
    for amount in AMOUNTS {
        let value = amount_value(amount);
        let values: Vec<Option<Amount>> = rows
            .iter()
            .map(|row| row.described_process().map(value))
            .collect();
        let [known, unavailable] = amount_column_names(amount);
        columns.push(Column::new(
            known.into(),
            values
                .iter()
                .map(|amount| match amount {
                    Some(Amount::Known(value)) => Some(*value),
                    Some(Amount::Unavailable(_)) | None => None,
                })
                .collect::<Vec<_>>(),
        ));
        columns.push(Column::new(
            unavailable.into(),
            values
                .iter()
                .map(|amount| match amount {
                    Some(Amount::Unavailable(reason)) => Some(unavailable_code(*reason)),
                    Some(Amount::Known(_)) | None => None,
                })
                .collect::<Vec<_>>(),
        ));
    }

    columns
}

/// Where an App row takes the measure `name` from.
fn measure_value(name: &str) -> fn(&Totals) -> &Subtotal {
    match name {
        "cpu_ns" => |app| &app.cpu_ns,
        "energy_nj" => |app| &app.energy_nj,
        "disk_read_bytes" => |app| &app.disk_read_bytes,
        "disk_written_bytes" => |app| &app.disk_written_bytes,
        "disk_io_bytes" => |app| &app.disk_io_bytes,
        "resident_bytes" => |app| &app.resident_bytes,
        "footprint_bytes" => |app| &app.footprint_bytes,
        "gpu_ns" => |app| &app.gpu_ns,
        _ => unreachable!("the table names only the measures an App totals"),
    }
}

/// Where a process row takes the amount `name` from. A gauge the process has no reading of is
/// unknown.
fn amount_value(name: &str) -> fn(&ProcessTotals) -> Amount {
    match name {
        "cpu_ns" => |process| process.deltas.cpu_ns,
        "energy_nj" => |process| process.deltas.energy_nj,
        "disk_read_bytes" => |process| process.deltas.disk_read_bytes,
        "disk_written_bytes" => |process| process.deltas.disk_written_bytes,
        "disk_io_bytes" => |process| process.deltas.disk_io_bytes,
        "resident_bytes" => |process| gauge(process.resident_bytes_last),
        "footprint_bytes" => |process| gauge(process.footprint_bytes_last),
        "idle_wakeups" => |process| process.deltas.idle_wakeups,
        "pageins" => |process| process.deltas.pageins,
        "threads" => |process| gauge(process.threads),
        "gpu_ns" => |process| process.deltas.gpu_ns,
        _ => unreachable!("the table names only the amounts a process holds"),
    }
}

fn gauge(value: Option<u64>) -> Amount {
    value.map_or(Amount::Unavailable(Unavailable::Unknown), Amount::Known)
}

/// Every column of the table in its order.
fn column_names() -> Vec<String> {
    let mut names = Vec::with_capacity(PARTS_COLUMNS);
    names.extend(PARTS_FIXED_COLUMNS.map(String::from));
    for measure in MEASURES {
        names.extend(measure_column_names(measure));
    }
    for amount in AMOUNTS {
        names.extend(amount_column_names(amount));
    }

    names
}
