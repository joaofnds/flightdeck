use std::path::Path;

use collector::recording::private::PrivateDirectory;

use polars::prelude::*;

use model::catalog;

use crate::schema::{TABLES, Table};
use crate::{Error, Result, Window};

mod bounds;
mod machine;
pub(crate) mod metric_projection;
mod table_writer;
mod ticks;
pub(crate) use table_writer::TableWriter;
#[cfg(test)]
mod write_tests;

/// Native encoding state lives for one table so it cannot accumulate across seals.
pub struct Encoder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableManifest {
    pub name: &'static str,
    pub rows: u64,
}

macro_rules! columns {
    ($rows:ident; $($name:literal => $value:expr),+ $(,)?) => {
        vec![$(
            Column::new($name.into(), $rows.iter().map($value).collect::<Vec<_>>())
        ),+]
    };
}

pub(crate) use columns;

impl Encoder {
    /// Writes typed files into a private directory. The caller synchronizes and publishes them.
    pub fn encode(&self, window: &Window, pending: &Path) -> Result<Vec<TableManifest>> {
        self.encode_into(window, &PrivateDirectory::open(pending)?)
    }

    /// Writes typed files through the directory its caller already holds open.
    pub fn encode_into(
        &self,
        window: &Window,
        pending: &PrivateDirectory,
    ) -> Result<Vec<TableManifest>> {
        if window.ticks.is_empty() {
            return Err(Error::Bound("empty window"));
        }

        let mut tables = Vec::with_capacity(TABLES.len());
        for table in TABLES {
            // A window no worker staged has no index, so its events go in one file here. No reader
            // takes that for a window, and only the tests of this encoding read it.
            if matches!(table, Table::Events) {
                if let Some(index) = &window.event_index {
                    tables.push(TableManifest {
                        name: table.name(),
                        rows: index.rows,
                    });
                    continue;
                }
            }
            let name = table.name();
            let rows = if matches!(table, Table::Processes) {
                encode_processes(window, encode_create(pending, &format!("{name}.parquet"))?)?
            } else {
                let mut frame = frame_in_file_order(table, window)?.sort(
                    table.catalog().key.to_vec(),
                    SortMultipleOptions {
                        descending: vec![false],
                        nulls_last: vec![true],
                        multithreaded: false,
                        maintain_order: true,
                        limit: None,
                    },
                )?;
                encode_write(
                    &mut frame,
                    encode_create(pending, &format!("{name}.parquet"))?,
                )?;
                collector::u64_from_usize(frame.height())
            };
            tables.push(TableManifest { name, rows });
        }

        Ok(tables)
    }
}

/// Writes a table derived from a window's own tables into its pending directory, as the window's
/// own tables are written. The caller synchronizes and publishes it with them.
pub fn encode_derived(
    name: &'static str,
    frame: &mut DataFrame,
    pending: &PrivateDirectory,
) -> Result<TableManifest> {
    assert!(crate::schema::DERIVED_TABLES.contains(&name));
    let rows = collector::u64_from_usize(frame.height());

    let file = encode_create(pending, &format!("{name}.parquet"))?;

    encode_write(frame, file)?;
    Ok(TableManifest { name, rows })
}

/// Writes a derived table of `rows` rows a row group at a time, as `encode_row_groups` does, for
/// a table with a row for each process of each tick.
pub(crate) fn encode_derived_row_groups(
    name: &'static str,
    rows: usize,
    pending: &PrivateDirectory,
    frame: impl FnMut(std::ops::Range<usize>) -> Result<DataFrame>,
) -> Result<TableManifest> {
    assert!(crate::schema::DERIVED_TABLES.contains(&name));
    let file = encode_create(pending, &format!("{name}.parquet"))?;
    encode_row_groups(rows, file, frame)?;

    Ok(TableManifest {
        name,
        rows: collector::u64_from_usize(rows),
    })
}

fn encode_create(pending: &PrivateDirectory, name: &str) -> Result<std::fs::File> {
    match pending.create_file(name) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(Error::Bound("existing table"))
        }
        Err(error) => Err(error.into()),
    }
}

/// Writes the processes table to `file` a row group at a time, in the order its file keeps, and
/// returns its rows. The table has a row for each process of each tick, so a frame of the whole
/// window, and the sorted copy a frame's sort makes, would take more than the persister may use
/// on a Mac that runs thousands of processes.
pub(crate) fn encode_processes(window: &Window, file: impl std::io::Write) -> Result<u64> {
    let rows = crate::identity_encoder::ProcessRows::in_file_order(window);
    encode_row_groups(rows.len(), file, |group| {
        let frame = rows.frame(window, group)?;
        frame_declared(Table::Processes, window, &frame);

        Ok(frame)
    })?;

    Ok(collector::u64_from_usize(rows.len()))
}

/// Writes a table of `rows` rows to `file` a row group at a time, each from the frame that
/// `frame` builds of the rows it is given, which are already in the order the file keeps. Each
/// frame is freed before the next is built, so the table takes one row group's memory however
/// many rows it has.
pub(crate) fn encode_row_groups(
    rows: usize,
    file: impl std::io::Write,
    mut frame: impl FnMut(std::ops::Range<usize>) -> Result<DataFrame>,
) -> Result<()> {
    let first = encode_row_groups_frame(0..rows.min(ROW_GROUP_ROWS), &mut frame)?;
    let mut writer = TableWriter::new(&first, file)?;
    encode_row_groups_write(&mut writer, first)?;
    for offset in (ROW_GROUP_ROWS..rows).step_by(ROW_GROUP_ROWS) {
        let group = offset..rows.min(offset + ROW_GROUP_ROWS);
        encode_row_groups_write(&mut writer, encode_row_groups_frame(group, &mut frame)?)?;
    }

    writer.finish()
}

fn encode_row_groups_frame(
    group: std::ops::Range<usize>,
    frame: &mut impl FnMut(std::ops::Range<usize>) -> Result<DataFrame>,
) -> Result<DataFrame> {
    crate::work::record(|counts| counts.frames += 1);
    let rows = group.len();
    let frame = frame(group)?;
    assert_eq!(frame.height(), rows);

    Ok(frame)
}

/// Writes a row group and frees its frame.
fn encode_row_groups_write<W: std::io::Write>(
    writer: &mut TableWriter<W>,
    mut frame: DataFrame,
) -> Result<()> {
    writer.write(&mut frame)
}

/// The rows of a row group, which the last row group of a table may fall short of.
pub(crate) const ROW_GROUP_ROWS: usize = 16_384;
/// What a data page holds before the writer starts the next.
const DATA_PAGE_BYTES: u64 = 1024 * 1024;
/// What a table's bytes gather in before they reach its file, the size of a data page.
pub const WRITE_BUFFER_BYTES: u64 = DATA_PAGE_BYTES;

pub(crate) fn encode_write(frame: &mut DataFrame, file: impl std::io::Write) -> Result<()> {
    let mut writer = TableWriter::new(frame, file)?;
    writer.write(frame)?;
    writer.finish()
}

/// Writes one batch of a window's events, however many of them the window holds.
pub(crate) fn encode_write_event_batch(
    frame: &mut DataFrame,
    file: impl std::io::Write,
) -> Result<()> {
    let mut writer = TableWriter::event_batch(frame, file)?;
    writer.write(frame)?;
    writer.finish()
}

/// What `frame` may take of memory for `table`, or why it refuses to build that frame.
pub fn frame_bytes(table: Table, window: &Window) -> Result<u64> {
    Ok(collector::u64_from_usize(bounds::frame_bytes(
        table, window,
    )?))
}

/// Materializes a single admitted window table without a SQL or row-string conversion.
pub fn frame(table: Table, window: &Window) -> Result<DataFrame> {
    crate::work::record(|counts| counts.frames += 1);
    bounds::frame(table, window)?;

    let frame = frame_built(table, window)?;
    frame_declared(table, window, &frame);
    Ok(frame)
}

fn frame_built(table: Table, window: &Window) -> Result<DataFrame> {
    match table {
        Table::Ticks => ticks::frame(window),
        Table::Memory => machine::memory(window),
        Table::Cpus => machine::cpus(window),
        Table::Interfaces => machine::interfaces(window),
        Table::GpuStates => machine::gpu(window),
        Table::CoreStates => machine::cores(window),
        Table::Fans => machine::fans(window),
        Table::Processes => crate::identity_encoder::processes(window),
        Table::Coalitions => machine::coalitions(window),
        Table::Events => crate::identity_encoder::events(window),
    }
}

/// The table's frame, with the events already in the order their file keeps, so the sort before
/// writing finds them sorted.
fn frame_in_file_order(table: Table, window: &Window) -> Result<DataFrame> {
    let frame = match table {
        Table::Events => {
            bounds::frame(table, window)?;
            crate::identity_encoder::events_in_file_order(window)?
        }
        _ => return frame(table, window),
    };
    frame_declared(table, window, &frame);
    Ok(frame)
}

/// Asserts that `frame` holds the columns the catalog declares for the table of the window, in
/// their order and of their kinds, so a column the encoder adds, renames or moves fails here
/// rather than in a reader of the files.
fn frame_declared(table: Table, window: &Window, frame: &DataFrame) {
    let declared = declared_columns(table, window);
    assert_eq!(frame.width(), declared.len());
    for (column, declared) in frame.columns().iter().zip(&declared) {
        assert_eq!(column.name().as_str(), declared.name);
        let stored = crate::query::storage_type(column.dtype());
        assert!(stored.is_ok_and(|name| name == crate::schema::storage_name(declared.kind)));
    }
}

pub(crate) fn usage_columns<'usage, Row>(
    rows: &'usage [Row],
    usage: impl Fn(&'usage Row) -> Option<&'usage collector::processes::Usage>,
) -> Vec<Column> {
    columns!(rows;
        "awake_ns_at_start" => |row| usage(row).map(|value| value.awake_ns_at_start),
        "user_ns" => |row| usage(row).map(|value| value.user_ns),
        "system_ns" => |row| usage(row).map(|value| value.system_ns),
        "energy_nj" => |row| usage(row).map(|value| value.energy_nj),
        "instructions" => |row| usage(row).map(|value| value.instructions),
        "super_instructions" => |row| usage(row).map(|value| value.super_instructions),
        "cycles" => |row| usage(row).map(|value| value.cycles),
        "idle_wakeups" => |row| usage(row).map(|value| value.idle_wakeups),
        "interrupt_wakeups" => |row| usage(row).map(|value| value.interrupt_wakeups),
        "pageins" => |row| usage(row).map(|value| value.pageins),
        "resident_bytes" => |row| usage(row).map(|value| value.resident_bytes),
        "footprint_bytes" => |row| usage(row).map(|value| value.footprint_bytes),
        "disk_read_bytes" => |row| usage(row).map(|value| value.disk_read_bytes),
        "disk_written_bytes" => |row| usage(row).map(|value| value.disk_written_bytes),
    )
}

/// Declares as SQL columns the saved schema of this table for the window's extensions.
#[must_use]
pub fn table_columns(table: Table, window: &Window) -> String {
    crate::schema::declaration(&declared_columns(table, window))
}

/// The columns the catalog declares for the table of a window: the table's own, then each of
/// its groups the window holds.
#[must_use]
pub fn declared_columns(table: Table, window: &Window) -> Vec<catalog::Column> {
    let system = window.system_captured();
    let identities = window.programs.is_some();
    let groups: &[(bool, &[catalog::Column])] = match table {
        Table::Ticks => &[
            (window.battery_captured(), &catalog::BATTERY),
            (window.disks_captured(), &[catalog::PHYSICAL_DISKS]),
            (window.temperatures_captured(), &catalog::TEMPERATURES),
            (system, &catalog::SYSTEM),
        ],
        Table::CoreStates => &[(system, &[catalog::CORE_STATE_FREQUENCY])],
        Table::Processes => &[
            (identities, &catalog::IDENTITY),
            (system && identities, &[catalog::PROCESS_GPU]),
        ],
        Table::Events => &[
            (identities, &catalog::IDENTITY),
            (true, &catalog::EVENT_OBSERVATION),
        ],
        _ => &[],
    };
    let declared = table.catalog();
    assert_eq!(groups.len(), declared.groups.len());

    let mut columns = declared.columns.to_vec();
    for ((held, group), declared_group) in groups.iter().zip(declared.groups) {
        assert_eq!(group, declared_group);
        if *held {
            columns.extend_from_slice(group);
        }
    }
    columns
}
