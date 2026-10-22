use std::fs::OpenOptions;
use std::path::Path;

use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::*;

use crate::schema::{TABLES, Table};
use crate::{Error, Result, Window};

mod bounds;
mod machine;
mod ticks;
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
        if window.ticks.is_empty() {
            return Err(Error::Bound("empty window"));
        }

        let mut tables = Vec::with_capacity(TABLES.len());
        for (table, _columns, order) in TABLES {
            let mut frame = frame(table, window)?.sort(
                order.split(", "),
                SortMultipleOptions {
                    descending: vec![false],
                    nulls_last: vec![true],
                    multithreaded: false,
                    maintain_order: true,
                    limit: None,
                },
            )?;
            let name = table.name();
            let rows = collector::u64_from_usize(frame.height());
            let path = pending.join(format!("{name}.parquet"));

            let file = encode_open(&path)?;

            encode_write(&mut frame, file)?;
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
    pending: &Path,
) -> Result<TableManifest> {
    assert!(crate::schema::DERIVED_TABLES.contains(&name));
    let rows = collector::u64_from_usize(frame.height());
    let path = pending.join(format!("{name}.parquet"));

    let file = encode_open(&path)?;

    encode_write(frame, file)?;
    Ok(TableManifest { name, rows })
}

fn encode_open(path: &Path) -> Result<std::fs::File> {
    if path.to_str().is_none() {
        return Err(Error::Bound("path encoding"));
    }

    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(Error::Bound("existing table"))
        }
        Err(error) => Err(error.into()),
    }
}

/// What a table's bytes gather in before they reach its file, the size of a data page.
pub const WRITE_BUFFER_BYTES: u64 = 1024 * 1024;

fn encode_write(frame: &mut DataFrame, file: impl std::io::Write) -> Result<()> {
    frame.rechunk_mut();
    // Parquet writes each header and page on its own, a system call apiece without a buffer.
    let capacity = usize::try_from(WRITE_BUFFER_BYTES).map_err(|_| Error::Bound("write buffer"))?;
    let mut buffer = std::io::BufWriter::with_capacity(capacity, file);
    let mut writer = ParquetWriter::new(&mut buffer)
        .with_compression(ParquetCompression::Zstd(Some(ZstdLevel::try_new(3)?)))
        .with_statistics(StatisticsOptions {
            min_value: true,
            max_value: true,
            distinct_count: false,
            null_count: true,
            binary_statistics_truncate_length: None,
        })
        .with_row_group_size(Some(16_384))
        .with_data_page_size(Some(1024 * 1024))
        .set_parallel(false)
        .with_key_value_metadata(None)
        .with_context_info(None)
        .batched(frame.schema())?;
    // Polars' eager finish balances groups above the requested size; explicit slices cap them.
    for offset in (0..frame.height()).step_by(16_384) {
        let length = (frame.height() - offset).min(16_384);
        let offset = i64::try_from(offset).map_err(|_| Error::Bound("parquet row offset"))?;
        writer.write_batch(&frame.slice(offset, length))?;
    }
    let bytes = writer.finish()?;
    assert!(bytes > 0);

    // Dropping the buffer would flush its last bytes and discard a failed write.
    buffer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    Ok(())
}

/// Materializes a single admitted window table without a SQL or row-string conversion.
pub fn frame(table: Table, window: &Window) -> Result<DataFrame> {
    bounds::frame(table, window)?;

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

pub(crate) fn usage_columns<'usage, Row>(
    rows: &[Row],
    usage: impl Fn(&Row) -> Option<&'usage collector::processes::Usage>,
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
    let Some((_, columns, _)) = TABLES.iter().find(|entry| entry.0.name() == table.name()) else {
        unreachable!("every table has a schema");
    };

    let mut columns = (*columns).to_owned();
    if matches!(table, Table::Ticks) {
        if window.battery_captured() {
            table_columns_push(&mut columns, &crate::schema::BATTERY_FIELDS);
        }
        if window.disks_captured() {
            columns.push_str(", physical_disks VARCHAR");
        }
        if window.temperatures_captured() {
            table_columns_push(&mut columns, &crate::schema::TEMPERATURE_FIELDS);
        }
        if window.system_captured() {
            table_columns_push(&mut columns, &crate::schema::SYSTEM_FIELDS);
        }
    }

    if window.programs.is_some() {
        if matches!(table, Table::Processes | Table::Events) {
            table_columns_push(&mut columns, &crate::schema::IDENTITY_FIELDS);
        }
    }

    if window.system_captured() {
        let field = match table {
            Table::CoreStates => Some(crate::schema::CORE_STATE_FREQUENCY_FIELD),
            Table::Processes if window.programs.is_some() => Some(crate::schema::PROCESS_GPU_FIELD),
            _ => None,
        };
        if let Some(field) = field {
            table_columns_push(&mut columns, &[field]);
        }
    }

    columns
}

fn table_columns_push(columns: &mut String, fields: &[(&str, &str)]) {
    for (name, kind) in fields {
        columns.push_str(", ");
        columns.push_str(name);
        columns.push(' ');
        columns.push_str(kind);
    }
}
