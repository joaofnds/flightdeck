use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{Duration, Instant};

use collector::recording::discovery::Selection;
use collector::ring::{Identity, RunIdentity};
use persistence::engine::{Engine, InterruptHandle, Row as QueryRow};
use persistence::layout::{self, DirectoryNames, Kind, WindowLocation};
use persistence::{Error, Manifest, Result, read_committed};
use polars::prelude::AnyValue;

use super::rows::{self, Row, TABLES};

#[path = "compare/cursor.rs"]
mod cursor;

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

struct ComparisonTable<'table> {
    label: &'table str,
    order: &'table str,
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

    let connection = Engine::new();
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
    interrupt: &InterruptHandle,
    deadline: Instant,
) -> Result<()> {
    match cancellation.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
    }

    // A query can start after expiry, so interrupt again until completion or the retry bound.
    for _attempt in 0..10_000 {
        interrupt.interrupt()?;
        match cancellation.recv_timeout(Duration::from_millis(1)) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    Err(bound("reference watchdog interruption bound"))
}

fn run_comparison(
    connection: &Engine,
    root: &Path,
    reference: &Path,
    manifests: &[Manifest],
    source: &Reference,
) -> Result<()> {
    let mut rows = 0_u64;
    for (table, order) in TABLES {
        rows += compare_table(
            connection,
            root,
            reference,
            manifests,
            source,
            &ComparisonTable {
                label: table,
                order,
            },
        )?;
    }
    if source.identity_capable {
        for (table, order) in [
            ("process_identity", "publication,pid"),
            ("event_identity", "publication,event"),
        ] {
            rows += compare_table(
                connection,
                root,
                reference,
                manifests,
                source,
                &ComparisonTable {
                    label: table,
                    order,
                },
            )?;
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
    discover_order(&mut manifests, &source.identity)?;
    Ok(manifests)
}

fn discover_order(manifests: &mut [Manifest], identity: &RunIdentity) -> Result<()> {
    if manifests.len() > 128 {
        return Err(bound("reference window count"));
    }
    manifests.sort_by_key(|manifest| manifest.publication_first);
    let mut previous_end = None;
    for manifest in manifests {
        if manifest.identity.run != *identity {
            return Err(bound("reference window run"));
        }
        if manifest.publication_first >= manifest.publication_end {
            return Err(bound("reference window publication range"));
        }
        if let Some(end) = previous_end {
            if manifest.publication_first < end {
                return Err(bound("reference overlapping publication ranges"));
            }
        }
        previous_end = Some(manifest.publication_end);
    }

    Ok(())
}

fn visit(path: &Path, entry: impl FnMut(&str) -> Result<()>) -> Result<()> {
    layout::visit_names(path, entry)
}

fn compare_table(
    connection: &Engine,
    root: &Path,
    reference: &Path,
    manifests: &[Manifest],
    source: &Reference,
    table: &ComparisonTable<'_>,
) -> Result<u64> {
    check_deadline(source.deadline)?;
    let mut actual = cursor::Cursor::new(connection, root, manifests, source, table)?;

    compare_table_rows(&mut actual, reference, source, table.label)
}

fn compare_table_rows(
    actual: &mut cursor::Cursor<'_>,
    reference: &Path,
    source: &Reference,
    table: &str,
) -> Result<u64> {
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

        let Some((row, columns)) = actual.next()? else {
            return Err(bound("disk has fewer rows than source reference"));
        };

        compare_table_row(&row, columns, table, &line)?;
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

fn compare_table_row(row: &QueryRow<'_>, columns: usize, table: &str, line: &str) -> Result<()> {
    let mut bytes = Vec::with_capacity(32_768);
    let publication: u64 = row.get(0)?;
    let mut target = Row::new(&mut bytes, table, publication)?;
    for column in 1..columns {
        value(&mut target, row.value(column)?)?;
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

fn value(target: &mut Row<'_, Vec<u8>>, value: AnyValue<'_>) -> Result<()> {
    match value {
        AnyValue::Null => target.null(),
        AnyValue::Boolean(value) => target.boolean(value),
        AnyValue::Int8(value) => target.signed(i64::from(value)),
        AnyValue::Int16(value) => target.signed(i64::from(value)),
        AnyValue::Int32(value) => target.signed(i64::from(value)),
        AnyValue::Int64(value) => target.signed(value),
        AnyValue::UInt8(value) => target.unsigned(u64::from(value)),
        AnyValue::UInt16(value) => target.unsigned(u64::from(value)),
        AnyValue::UInt32(value) => target.unsigned(u64::from(value)),
        AnyValue::UInt64(value) => target.unsigned(value),
        AnyValue::Float32(value) => target.float(value),
        AnyValue::Binary(value) => target.blob(value),
        AnyValue::BinaryOwned(value) => target.blob(&value),
        AnyValue::String(value) => target.text(value),
        AnyValue::StringOwned(value) => target.text(value.as_str()),
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
    use std::sync::mpsc;

    #[test]
    fn scalar_text_and_binary_match_exact_reference_bytes() {
        // Goal: native scalar storage preserves text and BLOB reference serialization.
        // Method: compare SQL literals, including zero and non-UTF-8 bytes, to a fixed reference.
        let connection = Engine::new();
        let frame = connection
            .query(
                "SELECT 7 AS publication, 'helper' AS name, \
                B'0000000011111111' AS path",
            )
            .unwrap();
        assert_eq!(frame.height(), 1);
        assert_eq!(frame.width(), 3);
        let row = QueryRow::new(&frame, 0).unwrap();
        assert!(matches!(row.value(1).unwrap(), AnyValue::StringOwned(_)));
        assert!(matches!(row.value(2).unwrap(), AnyValue::BinaryOwned(_)));

        compare_table_row(
            &row,
            frame.width(),
            "process_identity",
            "process_identity u7 s68656c706572 x00ff\n",
        )
        .unwrap();
    }

    #[test]
    fn expired_watchdog_interrupts_a_query_that_outlasts_the_deadline() {
        // Goal: after expiry, the watchdog alone stops comparison work that is still running.
        // Method: collect native work lasting past an expired watcher, then reuse the engine.
        let engine = Engine::new();
        let interrupt = engine.interrupt_handle();
        let (cancel, cancellation) = mpsc::sync_channel(0);
        let watcher =
            std::thread::spawn(move || watch_deadline(&cancellation, &interrupt, Instant::now()));

        let outcome = engine.collect(outlasting_plan());
        drop(cancel);
        let stopped = watcher.join().unwrap();

        stopped.unwrap();
        assert!(matches!(
            outcome,
            Err(Error::Engine(polars::error::PolarsError::ComputeError(message)))
                if message.as_ref() == "query interrupted"
        ));
        let frame = engine.query("SELECT 42").unwrap();
        assert_eq!(QueryRow::new(&frame, 0).unwrap().get::<i32>(0).unwrap(), 42);
    }

    fn outlasting_plan() -> polars::prelude::LazyFrame {
        // The first part stands for native work lasting 250 ms; the second part starts a new
        // executor, which observes the interruption that the watcher delivered meanwhile.
        use polars::prelude::{
            AllowedOptimizations, Column, DataFrame, IntoLazy, UnionArgs, col, concat, lit,
        };
        let first = DataFrame::new(1, vec![Column::new("value".into(), [11u64])])
            .unwrap()
            .lazy()
            .map(
                |frame| {
                    std::thread::sleep(Duration::from_millis(250));
                    Ok(frame)
                },
                AllowedOptimizations::empty(),
                None,
                Some("outlasting_comparison_work"),
            );
        let second = DataFrame::new(1, vec![Column::new("value".into(), [17u64])])
            .unwrap()
            .lazy()
            .select([(col("value") + lit(1u64)).alias("value")]);

        concat(
            [first, second],
            UnionArgs {
                parallel: false,
                rechunk: false,
                to_supertypes: false,
                diagonal: false,
                strict: true,
                from_partitioned_ds: false,
                maintain_order: true,
            },
        )
        .unwrap()
        .without_optimizations()
    }
}
