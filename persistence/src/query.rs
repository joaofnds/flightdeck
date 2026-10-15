//! Disk-only acceptance queries. The application query boundary belongs to TASK-20.

use std::io;
use std::path::Path;

use collector::recording::discovery::{
    DIRECTORY_BUFFER_WORDS, DirectoryCursor, DirectoryEntry, Selection, WindowName,
};

use crate::{Manifest, Result, read_committed};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Second {
    pub processes: Vec<(i32, u64)>,
    pub unavailable_time_windows: u64,
}

/// Errors preserve incomplete enumeration rather than returning a complete empty answer.
pub fn processes_at_second(root: &Path, second: u64) -> Result<Second> {
    let mut result = Second::default();
    visit_names(&root.join("data"), |run| {
        validate_run(run)?;
        visit_names(&root.join("data").join(run), |day| {
            validate_day(day)?;
            visit_names(&root.join("data").join(run).join(day), |name| {
                if name.starts_with('.') {
                    return Ok(());
                }
                let window = WindowName::parse(name)?;
                if window.select_second(second)? == Selection::Outside {
                    return Ok(());
                }
                let marker = root
                    .join("commits")
                    .join(run)
                    .join(day)
                    .join(format!("{:020}.seal", window.window));
                let manifest = match read_committed(root, &marker) {
                    Ok(manifest) => manifest,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error.into()),
                };
                if manifest.data_directory != format!("data/{run}/{day}/{name}") {
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

fn visit_names(path: &Path, mut visit: impl FnMut(&str) -> Result<()>) -> Result<()> {
    let mut buffer: Box<[u64; DIRECTORY_BUFFER_WORDS]> = vec![0; DIRECTORY_BUFFER_WORDS]
        .into_boxed_slice()
        .try_into()
        .map_err(|_| crate::Error::Bound("directory buffer"))?;
    let mut cursor = DirectoryCursor::open(path, &mut buffer)?;
    for _entry in 0..u64::MAX {
        match cursor.next_name()? {
            DirectoryEntry::Complete => return Ok(()),
            DirectoryEntry::Name(name) => visit(name)?,
        }
    }
    Err(crate::Error::Bound("query directory traversal"))
}

fn query_window(root: &Path, manifest: &Manifest, second: u64) -> Result<Vec<(i32, u64)>> {
    let connection = duckdb::Connection::open_in_memory()?;
    connection.execute_batch(
        "SET autoinstall_known_extensions=false; \
        SET autoload_known_extensions=false; SET threads=1; SET memory_limit='256MB';",
    )?;
    let directory = root.join(&manifest.data_directory);
    let source = |table| {
        directory
            .join(format!("{table}.parquet"))
            .to_string_lossy()
            .replace('\'', "''")
    };
    let first = second
        .checked_mul(1_000_000_000)
        .ok_or(crate::Error::Bound("query time"))?;
    let end = first
        .checked_add(1_000_000_000)
        .ok_or(crate::Error::Bound("query time"))?;
    let sql = format!(
        "WITH t AS (SELECT publication FROM read_parquet('{}') \
        WHERE wall_ns>={first} AND wall_ns<{end}), all_usage AS (\
        SELECT p.publication,p.pid,p.awake_ns_at_start,p.user_ns,0 priority \
        FROM read_parquet('{}') p \
        JOIN t USING(publication) UNION ALL \
        SELECT e.publication,e.pid,e.awake_ns_at_start,e.user_ns,1 priority \
        FROM read_parquet('{}') e \
        JOIN t USING(publication) WHERE e.kind=3 AND e.availability=0) \
        SELECT pid,user_ns FROM all_usage QUALIFY row_number() OVER \
        (PARTITION BY publication,pid,awake_ns_at_start ORDER BY priority DESC)=1 ORDER BY pid",
        source("ticks"),
        source("processes"),
        source("events")
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::Error::Engine)
}

fn validate_run(name: &str) -> Result<()> {
    if name.len() != 32 {
        return Err(crate::Error::Bound("run directory"));
    }
    if !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(crate::Error::Bound("run directory"));
    }
    Ok(())
}

fn validate_day(name: &str) -> Result<()> {
    if name.len() != 20 {
        return Err(crate::Error::Bound("elapsed day directory"));
    }
    if !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(crate::Error::Bound("elapsed day directory"));
    }
    Ok(())
}
