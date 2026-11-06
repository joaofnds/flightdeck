//! Bounded query sources for saved and staged recording tables.

use std::path::Path;
use std::sync::Arc;

use crate::canonical::ReadWork;
use crate::canonical::catalog::Catalog;
use crate::engine::{Engine, Row};
use crate::resources::ByteBudget;
use crate::schema::{CORE_STATE_FREQUENCY_FIELD, PROCESS_GPU_FIELD, Table};
use polars::prelude::DataType;

use crate::{Manifest, Result};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Second {
    pub processes: Vec<(i32, u64)>,
    pub unavailable_time_windows: u64,
}

#[derive(Clone, Copy)]
pub enum TableSource<'a> {
    Parquet(&'a Path),
    Staged,
}

/// Validates optional identity, battery, disk, temperature, system, core clock and GPU time
/// columns before exposing them to queries.
/// Legacy process/event sources gain typed unavailable metadata; counters remain unchanged.
/// Process validation admits at most 1,048,576 rows. Events validate at most 1,966,080
/// rows in bounded batches; consumers retain at most 1,048,576 associated event rows.
/// Battery and system tick validation each scan at most 62 rows, rejecting more than 61.
/// A saved source is validated once while the engine retains its registration, so saved
/// files must not change after they are first read.
pub fn table_source(connection: &Engine, table: Table, source: TableSource<'_>) -> Result<String> {
    match source {
        TableSource::Parquet(path) => {
            connection.parquet_relation(path, table.name(), |connection, source| {
                table_source_normalize(connection, table, source)
            })
        }
        TableSource::Staged => table_source_normalize(connection, table, table.name()),
    }
}

fn table_source_normalize(connection: &Engine, table: Table, source: &str) -> Result<String> {
    if matches!(table, Table::Processes) {
        let source = crate::identity::normalize(connection, table, source)?;
        return crate::optional_column::normalize(
            connection,
            &source,
            PROCESS_GPU_FIELD,
            crate::identity::COLUMNS_MAX,
        );
    }

    if matches!(table, Table::Events) {
        let source = crate::identity::normalize(connection, table, source)?;
        return crate::event_observation::normalize(connection, &source);
    }

    if matches!(table, Table::Ticks) {
        let source = crate::battery::normalize(connection, source)?;
        let source = crate::disks::normalize(connection, &source)?;
        let source = crate::temperatures::normalize(connection, &source)?;
        return crate::system::normalize(connection, &source);
    }

    if matches!(table, Table::CoreStates) {
        return crate::optional_column::normalize(
            connection,
            source,
            CORE_STATE_FREQUENCY_FIELD,
            CORE_STATE_COLUMNS_MAX,
        );
    }

    Ok(source.to_owned())
}

/// The four original columns of `core_states` and its clock.
const CORE_STATE_COLUMNS_MAX: usize = 5;

/// Errors preserve incomplete enumeration rather than returning a complete empty answer.
pub fn processes_at_second(root: &Path, second: u64) -> Result<Second> {
    const CATALOG_BYTES_MAX: usize = 1 << 28;
    validate_root(root)?;
    collector::recording::private::verify_directory(root)?;
    let budget = Arc::new(ByteBudget::new(CATALOG_BYTES_MAX));
    let catalog = Catalog::open(root, &budget)?;
    let mut work = ReadWork::default();
    let mut result = Second::default();
    for manifest in catalog.windows_at_second(second, &mut work)? {
        result
            .processes
            .extend(query_window(root, &manifest, second)?);
    }
    result.unavailable_time_windows =
        collector::u64_from_usize(catalog.windows_without_clock(&mut work)?.len());
    result.processes.sort_unstable();
    Ok(result)
}

/// Refuses paths the engine could interpret as patterns or change during UTF-8 conversion.
pub fn validate_root(path: &Path) -> Result<()> {
    let text = path
        .to_str()
        .ok_or(crate::Error::Bound("query path encoding"))?;
    if text.bytes().any(|byte| matches!(byte, b'*' | b'?' | b'[')) {
        return Err(crate::Error::Bound("query path glob"));
    }
    Ok(())
}

fn query_window(root: &Path, manifest: &Manifest, second: u64) -> Result<Vec<(i32, u64)>> {
    let connection = Engine::new();
    let budget = Arc::new(ByteBudget::new(
        usize::try_from(crate::resources::WORKING_BYTES_MAX)
            .map_err(|_| crate::Error::Bound("query memory"))?,
    ));
    let directory = root.join(&manifest.data_directory);
    let source = |table: Table| {
        if matches!(table, Table::Events) {
            return connection.event_source(root, manifest, &budget);
        }
        table_source(
            &connection,
            table,
            TableSource::Parquet(&directory.join(format!("{}.parquet", table.name()))),
        )
    };

    let first = second
        .checked_mul(1_000_000_000)
        .ok_or(crate::Error::Bound("query time"))?;
    let end = first
        .checked_add(1_000_000_000)
        .ok_or(crate::Error::Bound("query time"))?;

    let exit = crate::schema::event_kind::EXIT;
    let measured = crate::schema::availability::MEASURED;
    let sql = format!(
        "WITH t AS (SELECT publication FROM {} \
        WHERE wall_ns>={first} AND wall_ns<{end}), all_usage AS (\
        SELECT p.publication,p.pid,p.awake_ns_at_start,p.user_ns,0 priority \
        FROM {} p \
        JOIN t USING(publication) WHERE p.usage_availability={measured} UNION ALL \
        SELECT e.publication,e.pid,e.awake_ns_at_start,e.user_ns,1 priority \
        FROM {} e \
        JOIN t USING(publication) WHERE e.kind={exit} AND e.availability={measured}) \
        SELECT pid,user_ns FROM all_usage QUALIFY row_number() OVER \
        (PARTITION BY publication,pid,awake_ns_at_start ORDER BY priority DESC)=1 ORDER BY pid",
        source(Table::Ticks)?,
        source(Table::Processes)?,
        source(Table::Events)?
    );

    let frame = connection.query(&sql)?;

    let mut processes = Vec::with_capacity(frame.height());
    for index in 0..frame.height() {
        let row = Row::new(&frame, index)?;
        processes.push((row.get(0)?, row.get(1)?));
    }

    Ok(processes)
}

pub(crate) fn storage_type(data_type: &DataType) -> Result<&'static str> {
    match data_type {
        DataType::UInt8 => Ok("UTINYINT"),
        DataType::UInt16 => Ok("USMALLINT"),
        DataType::UInt32 => Ok("UINTEGER"),
        DataType::UInt64 => Ok("UBIGINT"),
        DataType::Int32 => Ok("INTEGER"),
        DataType::Int64 => Ok("BIGINT"),
        DataType::Float32 => Ok("FLOAT"),
        DataType::Float64 => Ok("DOUBLE"),
        DataType::Boolean => Ok("BOOLEAN"),
        DataType::String => Ok("VARCHAR"),
        DataType::Binary => Ok("BLOB"),
        _ => Err(crate::Error::Bound("recording storage type")),
    }
}
