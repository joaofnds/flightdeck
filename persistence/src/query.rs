//! Disk-only acceptance queries. The application query boundary belongs to TASK-20.

use std::io;
use std::path::Path;

use crate::layout::{self, DirectoryNames, Kind, WindowLocation};
use collector::recording::discovery::Selection;

use crate::{Manifest, Result, read_committed};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Second {
    pub processes: Vec<(i32, u64)>,
    pub unavailable_time_windows: u64,
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

/// Produces one literal SQL path after rejecting the engine's glob metacharacters.
pub fn parquet_source(path: &Path) -> Result<String> {
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
    let connection = duckdb::Connection::open_in_memory()?;
    connection.execute_batch(
        "SET autoinstall_known_extensions=false; \
        SET autoload_known_extensions=false; SET threads=1; SET memory_limit='256MB';",
    )?;
    let directory = root.join(&manifest.data_directory);
    let source = |table| parquet_source(&directory.join(format!("{table}.parquet")));
    let first = second
        .checked_mul(1_000_000_000)
        .ok_or(crate::Error::Bound("query time"))?;
    let end = first
        .checked_add(1_000_000_000)
        .ok_or(crate::Error::Bound("query time"))?;
    let exit = crate::schema::event_kind::EXIT;
    let measured = crate::schema::availability::MEASURED;
    let sql = format!(
        "WITH t AS (SELECT publication FROM read_parquet('{}') \
        WHERE wall_ns>={first} AND wall_ns<{end}), all_usage AS (\
        SELECT p.publication,p.pid,p.awake_ns_at_start,p.user_ns,0 priority \
        FROM read_parquet('{}') p \
        JOIN t USING(publication) UNION ALL \
        SELECT e.publication,e.pid,e.awake_ns_at_start,e.user_ns,1 priority \
        FROM read_parquet('{}') e \
        JOIN t USING(publication) WHERE e.kind={exit} AND e.availability={measured}) \
        SELECT pid,user_ns FROM all_usage QUALIFY row_number() OVER \
        (PARTITION BY publication,pid,awake_ns_at_start ORDER BY priority DESC)=1 ORDER BY pid",
        source("ticks")?,
        source("processes")?,
        source("events")?
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::Error::Engine)
}
