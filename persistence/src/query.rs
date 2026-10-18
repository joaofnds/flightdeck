//! Bounded query sources for saved and staged recording tables.

use std::io;
use std::path::Path;

use crate::engine::{Engine, Row};
use crate::layout::{self, DirectoryNames, Kind, WindowLocation};
use crate::schema::Table;
use collector::recording::discovery::Selection;
use polars::prelude::DataType;

use crate::{Manifest, Result, read_committed};

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

/// Validates optional identity and battery columns before exposing them to queries.
/// Legacy process/event sources gain typed unavailable metadata; counters remain unchanged.
/// Extended sources scan at most 1,048,577 rows, rejecting more than 1,048,576.
/// Battery tick validation scans at most 62 rows, rejecting more than 61.
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
    if matches!(table, Table::Processes | Table::Events) {
        return crate::identity::normalize(connection, table, source);
    }

    if matches!(table, Table::Ticks) {
        let source = crate::battery::normalize(connection, source)?;
        return crate::disks::normalize(connection, &source);
    }

    Ok(source.to_owned())
}

/// Errors preserve incomplete enumeration rather than returning a complete empty answer.
pub fn processes_at_second(root: &Path, second: u64) -> Result<Second> {
    validate_root(root)?;
    collector::recording::private::verify_directory(root)?;
    let mut result = Second::default();
    visit_names(&layout::base(root, Kind::Data), |run| {
        layout::validate_run(run)?;
        let run_path = layout::run(root, Kind::Data, collector::recording::parse_identity(run)?);
        visit_names(&run_path, |day| {
            layout::validate_day(day)?;
            visit_names(&run_path.join(day), |name| {
                if name.starts_with('.') {
                    return Ok(());
                }
                let location = WindowLocation::parse(&DirectoryNames {
                    run,
                    day,
                    window: name,
                })?;
                let window = location.name;
                if window.select_second(second)? == Selection::Outside {
                    return Ok(());
                }
                let marker = location.key.marker(root);
                let manifest = match read_committed(root, &marker) {
                    Ok(manifest) => manifest,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error.into()),
                };
                if Path::new(&manifest.data_directory) != location.ready_relative() {
                    return Err(io::Error::from(io::ErrorKind::InvalidData).into());
                }
                match window.select_second(second)? {
                    Selection::Match => result
                        .processes
                        .extend(query_window(root, &manifest, second)?),
                    Selection::Unknown => result.unavailable_time_windows += 1,
                    Selection::Outside => unreachable!("outside window filtered above"),
                }
                Ok(())
            })
        })
    })?;
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

/// Escapes one path as the body of an SQL string literal, refusing what `validate_root` refuses.
pub fn parquet_literal(path: &Path) -> Result<String> {
    validate_root(path)?;
    let text = path
        .to_str()
        .ok_or(crate::Error::Bound("query path encoding"))?;
    Ok(text.replace('\'', "''"))
}

fn visit_names(path: &Path, mut visit: impl FnMut(&str) -> Result<()>) -> Result<()> {
    layout::visit_names(path, &mut visit)
}

fn query_window(root: &Path, manifest: &Manifest, second: u64) -> Result<Vec<(i32, u64)>> {
    let connection = Engine::new();
    let directory = root.join(&manifest.data_directory);
    let source = |table: Table| {
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
