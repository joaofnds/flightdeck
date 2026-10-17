use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{Duration, Instant};

use collector::recording::discovery::Selection;
use collector::ring::{Identity, RunIdentity};
use duckdb::types::ValueRef;
use persistence::layout::{self, DirectoryNames, Kind, WindowLocation};
use persistence::schema::Table;
use persistence::{Error, Manifest, Result, read_committed};

use super::identity_reference;
use super::rows::{self, Row, TABLES};

struct Reference {
    identity: RunIdentity,
    identity_capable: bool,
    deadline: Instant,
    seconds: Vec<u64>,
    first: u64,
    newest: u64,
    publications: u64,
    tick_seen: bool,
    wall_ns: u64,
}

pub fn run(root: &Path, reference: &Path) -> Result<()> {
    persistence::query::validate_root(root)?;
    collector::recording::private::verify_directory(root)?;
    let deadline = Instant::now() + Duration::from_mins(5);
    let source = inspect(reference, deadline)?;
    let manifests = discover(root, &source)?;
    if manifests.is_empty() {
        return Err(Error::Bound("no committed source windows discovered"));
    }

    let connection = duckdb::Connection::open_in_memory()?;
    connection.execute_batch(
        "SET threads=1; SET memory_limit='512MiB'; \
        SET max_temp_directory_size='0B'; SET autoinstall_known_extensions=false; \
        SET autoload_known_extensions=false;",
    )?;
    let (cancel, cancellation) = std::sync::mpsc::sync_channel::<()>(0);
    let interrupt = connection.interrupt_handle();
    let watchdog = std::thread::Builder::new()
        .name("recording-check-deadline".to_owned())
        .stack_size(65_536)
        .spawn(move || watch_deadline(&cancellation, &interrupt, deadline))?;
    let result = run_comparison(&connection, root, reference, &manifests, &source);
    drop(cancel);
    watchdog
        .join()
        .map_err(|_| bound("reference watchdog failed"))??;

    result
}

fn watch_deadline(
    cancellation: &std::sync::mpsc::Receiver<()>,
    interrupt: &duckdb::InterruptHandle,
    deadline: Instant,
) -> Result<()> {
    match cancellation.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
    }

    // DuckDB clears interruption at query startup. Keep expiry effective across that boundary.
    for _attempt in 0..10_000 {
        interrupt.interrupt();
        match cancellation.recv_timeout(Duration::from_millis(1)) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    Err(bound("reference watchdog interruption bound"))
}

fn run_comparison(
    connection: &duckdb::Connection,
    root: &Path,
    reference: &Path,
    manifests: &[Manifest],
    source: &Reference,
) -> Result<()> {
    let mut rows = 0_u64;
    for (table, order) in TABLES {
        rows += compare_table(connection, root, reference, manifests, source, table, order)?;
    }
    if source.identity_capable {
        for (table, order) in [
            ("process_identity", "publication,pid"),
            ("event_identity", "publication,event"),
        ] {
            rows += compare_table(connection, root, reference, manifests, source, table, order)?;
        }
    }
    check_deadline(source.deadline)?;
    let capability = if source.identity_capable {
        "original numeric fields and all captured process/event identities match"
    } else {
        "original numeric fields match; reference-v1 has no identity fidelity capability"
    };
    println!(
        "verified {} publications and {rows} typed rows across {} immutable windows; {capability}",
        source.publications,
        manifests.len()
    );

    Ok(())
}

fn read_identity(reader: &mut BufReader<File>, deadline: Instant) -> Result<(RunIdentity, bool)> {
    let mut line = String::with_capacity(32_769);
    read_line(reader, &mut line, deadline)?;
    let mut fields = line.split_whitespace();
    let identity_capable = match fields.next() {
        Some("flightdeck-reference-1") => false,
        Some("flightdeck-reference-2") => {
            if fields.next() != Some("identity-1") {
                return Err(bound("reference identity capability"));
            }

            true
        }
        _ => return Err(bound("reference version")),
    };
    let mut words = [0; 6];
    for word in &mut words {
        *word = fields
            .next()
            .ok_or(bound("reference identity"))?
            .parse()
            .map_err(|_| bound("reference identity"))?;
    }
    if fields.next().is_some() {
        return Err(bound("reference identity width"));
    }

    Ok((
        RunIdentity {
            recording: Identity([words[0], words[1]]),
            run: Identity([words[2], words[3]]),
            boot: Identity([words[4], words[5]]),
        },
        identity_capable,
    ))
}

fn read_line(reader: &mut BufReader<File>, line: &mut String, deadline: Instant) -> Result<usize> {
    assert!(line.is_empty());
    check_deadline(deadline)?;
    let count =
        std::io::Read::take(&mut *reader, rows::REFERENCE_LINE_BYTES_MAX + 1).read_line(line)?;
    if u64::try_from(count).map_err(|_| bound("reference line size"))?
        > rows::REFERENCE_LINE_BYTES_MAX
    {
        return Err(bound("reference line limit"));
    }

    if count > 0 {
        if !line.ends_with('\n') {
            return Err(bound("reference unterminated line"));
        }
    }

    Ok(count)
}

fn inspect(path: &Path, deadline: Instant) -> Result<Reference> {
    if path.metadata()?.len() > rows::REFERENCE_BYTES_MAX {
        return Err(bound("reference exceeds 8 GiB"));
    }

    let mut reader = BufReader::new(File::open(path)?);
    let (identity, identity_capable) = read_identity(&mut reader, deadline)?;
    let mut line = String::with_capacity(32_769);
    let mut source = Reference {
        identity,
        identity_capable,
        deadline,
        seconds: Vec::with_capacity(3601),
        first: 0,
        newest: 0,
        publications: 0,
        tick_seen: false,
        wall_ns: 0,
    };
    let mut completed = false;
    let mut exhausted = false;
    for _ in 0..rows::REFERENCE_LINES_MAX {
        line.clear();
        if read_line(&mut reader, &mut line, deadline)? == 0 {
            exhausted = true;
            break;
        }

        if completed {
            return Err(bound("reference data after completion"));
        }

        inspect_row_kind(&line, &mut source)?;
        if let Some(header) = line.strip_prefix("begin ") {
            inspect_begin(header, &mut source)?;
        }
        if let Some(count) = line.strip_prefix("complete ") {
            if !source.tick_seen {
                return Err(bound("reference missing tick"));
            }
            let count: u64 = count.trim().parse().map_err(|_| bound("reference count"))?;
            if count != source.publications {
                return Err(bound("reference completion count"));
            }

            completed = true;
        }
    }
    if !exhausted {
        return Err(bound("reference enumeration incomplete"));
    }

    if !completed {
        return Err(bound("reference is incomplete"));
    }

    if source.publications == 0 {
        return Err(bound("reference contains no publications"));
    }

    source.seconds.sort_unstable();
    source.seconds.dedup();

    Ok(source)
}

fn inspect_begin(header: &str, source: &mut Reference) -> Result<()> {
    if source.publications > 0 {
        if !source.tick_seen {
            return Err(bound("reference missing tick"));
        }
    }

    let (publication, wall) = header
        .trim()
        .split_once(' ')
        .ok_or(bound("reference tick"))?;
    let publication: u64 = publication
        .parse()
        .map_err(|_| bound("reference publication"))?;
    if source.first == 0 {
        source.first = publication;
    }
    if publication <= source.newest {
        return Err(bound("reference publication order"));
    }

    source.newest = publication;
    source.publications += 1;
    let wall: u64 = wall.parse().map_err(|_| bound("reference wall time"))?;
    if source.seconds.len() == 3601 {
        return Err(bound("reference duration bound"));
    }
    source.seconds.push(wall / 1_000_000_000);
    source.wall_ns = wall;
    source.tick_seen = false;

    Ok(())
}

fn inspect_row_kind(line: &str, source: &mut Reference) -> Result<()> {
    let kind = line.split_once(' ').ok_or(bound("reference row kind"))?.0;
    if matches!(kind, "begin" | "complete") {
        return Ok(());
    }

    if !TABLES.iter().any(|(table, _)| *table == kind) {
        if !source.identity_capable {
            return Err(bound("reference row kind"));
        }

        if !matches!(kind, "process_identity" | "event_identity") {
            return Err(bound("reference row kind"));
        }
    }

    if source.publications == 0 {
        return Err(bound("reference row before publication"));
    }

    let publication = line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.strip_prefix('u'))
        .ok_or(bound("reference row publication"))?
        .parse::<u64>()
        .map_err(|_| bound("reference row publication"))?;
    if publication != source.newest {
        return Err(bound("reference row publication"));
    }
    if kind == "ticks" {
        if source.tick_seen {
            return Err(bound("reference duplicate tick"));
        }
        let wall = line
            .split_whitespace()
            .nth(3)
            .and_then(|value| value.strip_prefix('u'))
            .ok_or(bound("reference tick time"))?;
        let wall: u64 = wall.parse().map_err(|_| bound("reference tick time"))?;
        if wall != source.wall_ns {
            return Err(bound("reference tick time"));
        }

        source.tick_seen = true;
    }

    Ok(())
}

fn discover(root: &Path, source: &Reference) -> Result<Vec<Manifest>> {
    let mut manifests = Vec::with_capacity(128);
    visit(&layout::base(root, Kind::Data), |run| {
        layout::validate_run(run)?;
        let run_path = layout::run(root, Kind::Data, collector::recording::parse_identity(run)?);
        visit(&run_path, |day| {
            layout::validate_day(day)?;
            visit(&run_path.join(day), |name| {
                if name.starts_with('.') {
                    return Ok(());
                }
                check_deadline(source.deadline)?;
                let location = WindowLocation::parse(&DirectoryNames {
                    run,
                    day,
                    window: name,
                })?;
                let window = location.name;
                let mut selected = false;
                for second in &source.seconds {
                    if window.select_second(*second)? == Selection::Match {
                        selected = true;
                        break;
                    }
                }
                if !selected {
                    return Ok(());
                }
                let marker = location.key.marker(root);
                let manifest = match read_committed(root, &marker) {
                    Ok(manifest) => manifest,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error.into()),
                };
                if manifest.identity.run != source.identity {
                    return Ok(());
                }
                if Path::new(&manifest.data_directory) != location.ready_relative() {
                    return Err(bound(
                        "discovered directory does not match canonical marker",
                    ));
                }
                if manifests.len() == 128 {
                    return Err(bound("reference window count"));
                }
                manifests.push(manifest);
                Ok(())
            })
        })
    })?;
    Ok(manifests)
}

fn visit(path: &Path, entry: impl FnMut(&str) -> Result<()>) -> Result<()> {
    layout::visit_names(path, entry)
}

fn compare_table(
    connection: &duckdb::Connection,
    root: &Path,
    reference: &Path,
    manifests: &[Manifest],
    source: &Reference,
    table: &str,
    order: &str,
) -> Result<u64> {
    check_deadline(source.deadline)?;
    let schema_name = match table {
        "process_identity" => "processes",
        "event_identity" => "events",
        _ => table,
    };
    let (schema, _, _) = persistence::schema::TABLES
        .iter()
        .find(|entry| entry.0.name() == schema_name)
        .ok_or(bound("reference table"))?;
    let projection =
        compare_table_source(connection, root, manifests, *schema, table, source.deadline)?;
    let sql = format!(
        "SELECT * FROM ({projection}) WHERE publication>={} \
        AND publication<={} ORDER BY {order} LIMIT 100000001",
        source.first, source.newest
    );
    check_deadline(source.deadline)?;
    let mut statement = connection.prepare(&sql)?;
    check_deadline(source.deadline)?;
    let mut actual = statement.query([])?;
    let columns = actual
        .as_ref()
        .ok_or(bound("missing executed statement"))?
        .column_count();
    let mut expected = BufReader::new(File::open(reference)?);
    let mut line = String::with_capacity(32_769);
    let mut count = 0_u64;
    let prefix = format!("{table} ");
    for _ in 0..rows::REFERENCE_LINES_MAX {
        line.clear();
        if read_line(&mut expected, &mut line, source.deadline)? == 0 {
            break;
        }

        if !line.starts_with(&prefix) {
            continue;
        }

        let row = actual
            .next()?
            .ok_or(bound("disk has fewer rows than source reference"))?;
        compare_table_row(row, columns, table, &line)?;
        count += 1;
    }
    if count == 100_000_000 {
        return Err(bound("reference result bound"));
    }

    if actual.next()?.is_some() {
        return Err(bound("disk has extra rows"));
    }

    println!("{table}: {count} exact rows");

    Ok(count)
}

fn compare_table_row(row: &duckdb::Row<'_>, columns: usize, table: &str, line: &str) -> Result<()> {
    let mut bytes = Vec::with_capacity(32_768);
    let publication: u64 = row.get(0)?;
    let mut target = Row::new(&mut bytes, table, publication)?;
    for column in 1..columns {
        value(&mut target, row.get_ref(column)?)?;
    }
    target.end()?;
    assert!(bytes.len() <= 32_768);
    if bytes != line.as_bytes() {
        eprintln!(
            "mismatch table={table} publication={publication}\nsource={}disk={}",
            line,
            String::from_utf8_lossy(&bytes)
        );
        return Err(bound("source and disk typed values differ"));
    }

    Ok(())
}

fn compare_table_source(
    connection: &duckdb::Connection,
    root: &Path,
    manifests: &[Manifest],
    table: Table,
    label: &str,
    deadline: Instant,
) -> Result<String> {
    assert!(manifests.len() <= 128);
    let columns = match label {
        "process_identity" => identity_reference::PROCESS_COLUMNS.to_owned(),
        "event_identity" => identity_reference::EVENT_COLUMNS.to_owned(),
        _ => table.columns().collect::<Vec<_>>().join(","),
    };
    let mut sources = Vec::with_capacity(manifests.len());

    for manifest in manifests {
        check_deadline(deadline)?;
        let path = root
            .join(&manifest.data_directory)
            .join(format!("{}.parquet", table.name()));
        let source = persistence::query::table_source(
            connection,
            table,
            persistence::query::TableSource::Parquet(&path),
        )?;
        let measured = if label == "processes" {
            " WHERE usage_availability=0".to_owned()
        } else {
            String::new()
        };
        sources.push(format!("SELECT {columns} FROM {source}{measured}"));
    }

    Ok(sources.join(" UNION ALL "))
}

fn value(target: &mut Row<'_, Vec<u8>>, value: ValueRef<'_>) -> Result<()> {
    match value {
        ValueRef::Null => target.null(),
        ValueRef::Boolean(value) => target.boolean(value),
        ValueRef::TinyInt(value) => target.signed(i64::from(value)),
        ValueRef::SmallInt(value) => target.signed(i64::from(value)),
        ValueRef::Int(value) => target.signed(i64::from(value)),
        ValueRef::BigInt(value) => target.signed(value),
        ValueRef::UTinyInt(value) => target.unsigned(u64::from(value)),
        ValueRef::USmallInt(value) => target.unsigned(u64::from(value)),
        ValueRef::UInt(value) => target.unsigned(u64::from(value)),
        ValueRef::UBigInt(value) => target.unsigned(value),
        ValueRef::Float(value) => target.float(value),
        ValueRef::Blob(value) => target.blob(value),
        ValueRef::Text(value) => {
            target.text(std::str::from_utf8(value).map_err(|_| bound("disk text encoding"))?)
        }
        _ => Err(bound("unexpected disk column type")),
    }
}

fn bound(message: &'static str) -> Error {
    Error::Bound(message)
}

fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(bound("reference verification time bound"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, RecvTimeoutError};

    #[test]
    fn expired_watchdog_survives_query_startup_and_interrupts_execution() {
        // Goal: expiry between preparation and execution remains effective after startup.
        // Method: expire a real connection, confirm its watcher stays alive, then run work.
        let (ready, handle) = mpsc::sync_channel(1);
        let (proceed, permitted) = mpsc::sync_channel(1);
        let (finished, result) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let connection = duckdb::Connection::open_in_memory().unwrap();
            let mut statement = connection
                .prepare("SELECT count(*) FROM range(1000000000000) t(i) WHERE i % 7 = 0")
                .unwrap();
            ready.send(connection.interrupt_handle()).unwrap();
            permitted.recv_timeout(Duration::from_secs(5)).unwrap();
            let result = statement.query_row([], |row| row.get::<_, i64>(0));
            finished.send(result).unwrap();
        });
        let interrupt = handle.recv_timeout(Duration::from_secs(5)).unwrap();
        let (cancel, cancellation) = mpsc::sync_channel(0);
        let (stopped, completion) = mpsc::sync_channel(1);
        let watcher_interrupt = interrupt.clone();
        let watcher = std::thread::spawn(move || {
            let result = watch_deadline(&cancellation, &watcher_interrupt, Instant::now());
            stopped.send(result).unwrap();
        });

        assert!(matches!(
            completion.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Timeout)
        ));
        proceed.send(()).unwrap();
        let outcome = result.recv_timeout(Duration::from_secs(5));
        if outcome.is_err() {
            interrupt.interrupt();
        }
        drop(cancel);
        completion
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        watcher.join().unwrap();
        worker.join().unwrap();

        let error = outcome
            .unwrap()
            .unwrap_err()
            .to_string()
            .to_ascii_lowercase();
        assert!(error.contains("interrupt"), "unexpected error: {error}");
        assert!(matches!(
            check_deadline(Instant::now()),
            Err(Error::Bound("reference verification time bound"))
        ));
    }
}
