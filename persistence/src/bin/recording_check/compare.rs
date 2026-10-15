use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use collector::recording::discovery::{
    DIRECTORY_BUFFER_WORDS, DirectoryCursor, DirectoryEntry, Selection, WindowName,
};
use collector::ring::{Identity, RunIdentity};
use duckdb::types::ValueRef;
use persistence::{Error, Manifest, Result, read_committed};

use super::rows::{Row, TABLES};

struct Reference {
    identity: RunIdentity,
    seconds: Vec<u64>,
    first: u64,
    newest: u64,
    publications: u64,
}

pub fn run(root: &Path, reference: &Path) -> Result<()> {
    persistence::query::validate_root(root)?;
    let source = inspect(reference)?;
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
    let mut rows = 0_u64;
    for (table, order) in TABLES {
        rows += compare_table(
            &connection,
            root,
            reference,
            &manifests,
            &source,
            table,
            order,
        )?;
    }
    println!(
        "verified {} publications and {rows} typed rows across {} immutable windows; \
        all source fields, events, availability and float bits match",
        source.publications,
        manifests.len()
    );
    Ok(())
}

fn read_identity(reader: &mut BufReader<File>) -> Result<RunIdentity> {
    let mut line = String::new();
    read_line(reader, &mut line)?;
    let mut fields = line.split_whitespace();
    if fields.next() != Some("flightdeck-reference-1") {
        return Err(bound("reference version"));
    }
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
    Ok(RunIdentity {
        recording: Identity([words[0], words[1]]),
        run: Identity([words[2], words[3]]),
        boot: Identity([words[4], words[5]]),
    })
}

fn read_line(reader: &mut BufReader<File>, line: &mut String) -> Result<usize> {
    let count = std::io::Read::take(&mut *reader, 4097).read_line(line)?;
    if count > 4096 {
        return Err(bound("reference line limit"));
    }
    Ok(count)
}

fn inspect(path: &Path) -> Result<Reference> {
    let mut reader = BufReader::new(File::open(path)?);
    let identity = read_identity(&mut reader)?;
    let mut line = String::new();
    let mut source = Reference {
        identity,
        seconds: Vec::with_capacity(3601),
        first: 0,
        newest: 0,
        publications: 0,
    };
    let mut completed = false;
    let mut exhausted = false;
    for _ in 0..100_000_000_u64 {
        line.clear();
        if read_line(&mut reader, &mut line)? == 0 {
            exhausted = true;
            break;
        }
        if completed {
            return Err(bound("reference data after completion"));
        }
        if let Some(header) = line.strip_prefix("begin ") {
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
            source.seconds.push(wall / 1_000_000_000);
            if source.seconds.len() > 3601 {
                return Err(bound("reference duration bound"));
            }
        }
        if let Some(count) = line.strip_prefix("complete ") {
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

fn discover(root: &Path, source: &Reference) -> Result<Vec<Manifest>> {
    let mut manifests = Vec::with_capacity(64);
    visit(&root.join("data"), |run| {
        visit(&root.join("data").join(run), |day| {
            visit(&root.join("data").join(run).join(day), |name| {
                if name.starts_with('.') {
                    return Ok(());
                }
                let window = WindowName::parse(name)?;
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
                let marker = root
                    .join("commits")
                    .join(run)
                    .join(day)
                    .join(format!("{:020}.seal", window.window));
                let manifest = match read_committed(root, &marker) {
                    Ok(manifest) => manifest,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error.into()),
                };
                if manifest.identity.run != source.identity {
                    return Ok(());
                }
                if manifest.data_directory != format!("data/{run}/{day}/{name}") {
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

fn visit(path: &Path, mut entry: impl FnMut(&str) -> Result<()>) -> Result<()> {
    let mut buffer: Box<[u64; DIRECTORY_BUFFER_WORDS]> = vec![0; DIRECTORY_BUFFER_WORDS]
        .into_boxed_slice()
        .try_into()
        .map_err(|_| bound("directory buffer"))?;
    let mut cursor = DirectoryCursor::open(path, &mut buffer)?;
    for _ in 0..10_000_000_u64 {
        match cursor.next_name()? {
            DirectoryEntry::Name(name) => entry(name)?,
            DirectoryEntry::Complete => return Ok(()),
        }
    }
    Err(bound("discovery incomplete"))
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
    let files: Vec<_> = manifests
        .iter()
        .map(|manifest| {
            let path = root
                .join(&manifest.data_directory)
                .join(format!("{table}.parquet"));
            persistence::query::parquet_source(&path).map(|path| format!("'{path}'"))
        })
        .collect::<Result<_>>()?;
    let sql = format!(
        "SELECT * FROM read_parquet([{}]) WHERE publication>={} \
        AND publication<={} ORDER BY {order}",
        files.join(","),
        source.first,
        source.newest
    );
    let mut statement = connection.prepare(&sql)?;
    let mut actual = statement.query([])?;
    let columns = actual
        .as_ref()
        .ok_or(bound("missing executed statement"))?
        .column_count();
    let mut expected = BufReader::new(File::open(reference)?);
    let mut line = String::new();
    let mut count = 0_u64;
    let prefix = format!("{table} ");
    for _ in 0..100_000_000_u64 {
        line.clear();
        if read_line(&mut expected, &mut line)? == 0 {
            break;
        }
        if !line.starts_with(&prefix) {
            continue;
        }
        let row = actual
            .next()?
            .ok_or(bound("disk has fewer rows than source reference"))?;
        let mut bytes = Vec::with_capacity(line.len());
        let publication: u64 = row.get(0)?;
        let mut target = Row::new(&mut bytes, table, publication)?;
        for column in 1..columns {
            value(&mut target, row.get_ref(column)?)?;
        }
        target.end()?;
        if bytes != line.as_bytes() {
            eprintln!(
                "mismatch table={table} publication={publication}\nsource={}disk={}",
                line,
                String::from_utf8_lossy(&bytes)
            );
            return Err(bound("source and disk typed values differ"));
        }
        count += 1;
    }
    if actual.next()?.is_some() {
        return Err(bound("disk has extra rows"));
    }
    println!("{table}: {count} exact rows");
    Ok(count)
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
        ValueRef::Text(value) => {
            target.text(std::str::from_utf8(value).map_err(|_| bound("disk text encoding"))?)
        }
        _ => Err(bound("unexpected disk column type")),
    }
}

fn bound(message: &'static str) -> Error {
    Error::Bound(message)
}
