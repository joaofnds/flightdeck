//! Typed committed endpoints feed the same owned aggregate as live publications.
use collector::processes::Usage;
use duckdb::{Connection, Row, types::ValueRef};

use super::{
    Frame, IdentityUnavailable, Member, ProcessCreation, ProgramBytes, ProgramSpan, RunIdentity,
};
use crate::apps::{self, Stamp, Unavailable};
use crate::cancellation::Operation;
use persistence::{Error, Result};

pub(crate) struct Endpoint {
    pub identity: RunIdentity,
    pub stamp: Stamp,
    pub run_tick: u64,
    pub wall_ns: Option<u64>,
    pub usage_refused: u32,
    pub usage_gone: u32,
}

const PROCESS_COLUMNS: &str = "pid, process_unique_id, pid_version, identity_availability, \
    executable_name, app_path, usage_availability, awake_ns_at_start, user_ns, system_ns, \
    energy_nj, instructions, super_instructions, cycles, idle_wakeups, interrupt_wakeups, \
    pageins, resident_bytes, footprint_bytes, disk_read_bytes, disk_written_bytes";

#[derive(Clone, Copy)]
struct FrameSize {
    count: usize,
    bytes: usize,
}

struct PublicationSize {
    publication: u64,
    size: FrameSize,
}

struct Batch<'source> {
    endpoints: &'source [Endpoint],
    source: &'source str,
    publications: &'source str,
}

pub(crate) const ENDPOINTS_MAX: usize = 61;
const BATCH_COLUMNS: usize = 22;
const ROW_LOGICAL_BYTES: usize = BATCH_COLUMNS * size_of::<Option<u64>>();
const SENTINEL_LOGICAL_BYTES: usize =
    ROW_LOGICAL_BYTES + 2 * collector::ring::PROGRAM_PATH_BYTES_MAX;
const BATCH_LOGICAL_BYTES_MAX: usize = 8 * 1024 * 1024;
const SINGLETON_LOGICAL_BYTES_MAX: usize = super::PROGRAM_FRAME_BYTES_MAX
    + apps::PROCESS_INTERVALS_MAX * ROW_LOGICAL_BYTES
    + SENTINEL_LOGICAL_BYTES;

struct QuerySize {
    rows: usize,
    logical_bytes: usize,
}

const _: () = {
    assert!(SINGLETON_LOGICAL_BYTES_MAX < 64 * 1024 * 1024);
    assert!(SENTINEL_LOGICAL_BYTES < BATCH_LOGICAL_BYTES_MAX);
};

impl FrameSize {
    fn logical_bytes(self) -> Result<usize> {
        self.count
            .checked_mul(ROW_LOGICAL_BYTES)
            .and_then(|fixed| fixed.checked_add(self.bytes))
            .ok_or(Error::Bound("committed App batch logical bytes"))
    }

    fn validate(self) -> Result<()> {
        if self.count > apps::PROCESS_INTERVALS_MAX {
            return Err(Error::Bound("committed App process bound"));
        }

        if self.bytes > super::PROGRAM_FRAME_BYTES_MAX {
            return Err(Error::Bound("committed App byte bound"));
        }

        Ok(())
    }
}

impl Frame {
    pub(crate) fn committed(
        endpoint: &Endpoint,
        connection: &Connection,
        source: &str,
        operation: &Operation,
        output: &mut Option<Box<Self>>,
    ) -> Result<()> {
        *output = None;
        operation.check()?;
        assert_eq!(endpoint.stamp.boot, endpoint.identity.boot.0);
        assert_eq!(endpoint.stamp.run, endpoint.identity.run.0);

        let publication = endpoint.stamp.publication;
        let (count, bytes): (u64, u64) = connection.query_row(
            &format!(
                "SELECT count(*), coalesce(sum(coalesce(octet_length(executable_name), 0) + \
                coalesce(octet_length(app_path), 0)), 0)::UBIGINT \
                FROM {source} WHERE publication={publication}"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let count = usize::try_from(count).map_err(|_| Error::Bound("committed App count"))?;
        let bytes = usize::try_from(bytes).map_err(|_| Error::Bound("committed App bytes"))?;

        let size = FrameSize { count, bytes };
        size.validate()?;
        let mut frame = committed_frame(endpoint, size);
        committed_members(connection, source, operation, &mut frame)?;
        committed_finish(&frame, size)?;
        *output = Some(frame);

        Ok(())
    }

    /// Source is normalized immutable process data. Callbacks perform no SQL on this connection.
    pub(crate) fn committed_many(
        endpoints: &[Endpoint],
        connection: &Connection,
        source: &str,
        operation: &Operation,
        mut consume: impl FnMut(Box<Self>) -> Result<()>,
    ) -> Result<()> {
        operation.check()?;
        if endpoints.is_empty() {
            return Ok(());
        }

        let publications = committed_many_publications(endpoints)?;
        let batch = Batch {
            endpoints,
            source,
            publications: &publications,
        };
        let mut sizes = Vec::with_capacity(endpoints.len());
        committed_many_sizes(&batch, connection, operation, &mut sizes)?;
        committed_many_chunks(&batch, &sizes, connection, operation, &mut consume)
    }
}

fn committed_many_chunks(
    batch: &Batch<'_>,
    sizes: &[PublicationSize],
    connection: &Connection,
    operation: &Operation,
    consume: &mut impl FnMut(Box<Frame>) -> Result<()>,
) -> Result<()> {
    let mut first = 0;
    for _chunk in 0..ENDPOINTS_MAX {
        operation.check()?;
        if first == batch.endpoints.len() {
            return Ok(());
        }

        let count = committed_many_chunk_count(&batch.endpoints[first..], sizes)?;
        assert!(count > 0);
        let end = first + count;
        let endpoints = &batch.endpoints[first..end];
        let publications = committed_many_publications(endpoints)?;
        let chunk = Batch {
            endpoints,
            source: batch.source,
            publications: &publications,
        };
        committed_many_rows(&chunk, sizes, connection, operation, consume)?;
        first = end;
    }
    assert_eq!(first, batch.endpoints.len());

    operation.check()
}

fn committed_many_chunk_count(endpoints: &[Endpoint], sizes: &[PublicationSize]) -> Result<usize> {
    let mut logical_bytes = SENTINEL_LOGICAL_BYTES;
    let mut count = 0;
    for endpoint in endpoints {
        let size = committed_many_size(endpoint.stamp.publication, sizes);
        let next = logical_bytes
            .checked_add(size.logical_bytes()?)
            .ok_or(Error::Bound("committed App batch logical bytes"))?;
        if next > BATCH_LOGICAL_BYTES_MAX {
            if count == 0 {
                return Ok(1);
            }

            break;
        }
        logical_bytes = next;
        count += 1;
    }

    Ok(count)
}

fn committed_many_size(publication: u64, sizes: &[PublicationSize]) -> FrameSize {
    match sizes.binary_search_by_key(&publication, |entry| entry.publication) {
        Ok(index) => sizes[index].size,
        Err(_) => FrameSize { count: 0, bytes: 0 },
    }
}

fn committed_many_query_size(
    endpoints: &[Endpoint],
    sizes: &[PublicationSize],
) -> Result<QuerySize> {
    let mut size = QuerySize {
        rows: 0,
        logical_bytes: SENTINEL_LOGICAL_BYTES,
    };
    for endpoint in endpoints {
        let frame = committed_many_size(endpoint.stamp.publication, sizes);
        size.rows = size
            .rows
            .checked_add(frame.count)
            .ok_or(Error::Bound("committed App batch rows"))?;
        size.logical_bytes = size
            .logical_bytes
            .checked_add(frame.logical_bytes()?)
            .ok_or(Error::Bound("committed App batch logical bytes"))?;
    }
    let maximum = if endpoints.len() == 1 {
        SINGLETON_LOGICAL_BYTES_MAX
    } else {
        BATCH_LOGICAL_BYTES_MAX
    };
    if size.logical_bytes > maximum {
        return Err(Error::Bound("committed App batch query bytes"));
    }

    Ok(size)
}

fn committed_many_publications(endpoints: &[Endpoint]) -> Result<String> {
    if endpoints.len() > ENDPOINTS_MAX {
        return Err(Error::Bound("committed App batch endpoints"));
    }

    let first = endpoints
        .first()
        .ok_or(Error::Bound("committed App batch empty"))?;
    let mut previous = None;
    let mut publications = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        if endpoint.identity != first.identity {
            return Err(Error::Bound("committed App batch identity"));
        }

        assert_eq!(endpoint.stamp.boot, endpoint.identity.boot.0);
        assert_eq!(endpoint.stamp.run, endpoint.identity.run.0);
        if previous.is_some_and(|previous| previous >= endpoint.stamp.publication) {
            return Err(Error::Bound("committed App batch publication order"));
        }

        previous = Some(endpoint.stamp.publication);
        publications.push(endpoint.stamp.publication.to_string());
    }

    Ok(publications.join(","))
}

fn committed_many_sizes(
    batch: &Batch<'_>,
    connection: &Connection,
    operation: &Operation,
    output: &mut Vec<PublicationSize>,
) -> Result<()> {
    let limit = batch.endpoints.len() + 1;
    let mut statement = connection.prepare(&format!(
        "SELECT publication, count(*), coalesce(sum(coalesce(octet_length(executable_name), 0) + \
        coalesce(octet_length(app_path), 0)), 0)::UBIGINT FROM {source} \
        WHERE publication IN ({publications}) GROUP BY publication \
        ORDER BY publication LIMIT {limit}",
        source = batch.source,
        publications = batch.publications,
    ))?;
    let mut rows = statement.query([])?;
    for index in 0..=batch.endpoints.len() {
        operation.check()?;
        let Some(row) = rows.next()? else {
            return Ok(());
        };

        if index == batch.endpoints.len() {
            return Err(Error::Bound("committed App batch size rows"));
        }

        let publication = required(row, 0)?;
        if batch
            .endpoints
            .binary_search_by_key(&publication, |endpoint| endpoint.stamp.publication)
            .is_err()
        {
            return Err(Error::Bound("committed App batch unwanted publication"));
        }

        if output
            .last()
            .is_some_and(|entry| entry.publication >= publication)
        {
            return Err(Error::Bound("committed App batch duplicate size"));
        }

        let size = FrameSize {
            count: usize::try_from(required::<u64>(row, 1)?)
                .map_err(|_| Error::Bound("committed App count"))?,
            bytes: usize::try_from(required::<u64>(row, 2)?)
                .map_err(|_| Error::Bound("committed App bytes"))?,
        };
        size.validate()?;
        output.push(PublicationSize { publication, size });
    }

    Err(Error::Bound("committed App batch size rows"))
}

fn committed_many_rows(
    batch: &Batch<'_>,
    sizes: &[PublicationSize],
    connection: &Connection,
    operation: &Operation,
    consume: &mut impl FnMut(Box<Frame>) -> Result<()>,
) -> Result<()> {
    // This accounts requested logical cells and BLOBs, not DuckDB overhead or physical memory.
    let size = committed_many_query_size(batch.endpoints, sizes)?;
    assert!(size.rows <= ENDPOINTS_MAX * apps::PROCESS_INTERVALS_MAX);
    let limit = size.rows + 1;
    let mut statement = connection.prepare(&format!(
        "SELECT {PROCESS_COLUMNS}, publication FROM {source} WHERE publication IN ({publications}) \
        ORDER BY publication, pid LIMIT {limit}",
        source = batch.source,
        publications = batch.publications,
    ))?;
    let mut rows = statement.query([])?;
    for endpoint in batch.endpoints {
        operation.check()?;
        let size = committed_many_size(endpoint.stamp.publication, sizes);
        let mut frame = committed_frame(endpoint, size);
        committed_many_members(&mut rows, &mut frame, size, operation)?;
        committed_finish(&frame, size)?;
        operation.check()?;
        consume(frame)?;
    }
    if rows.next()?.is_some() {
        return Err(Error::Bound("committed App batch changed row count"));
    }

    // Returning releases Rows and its owning Statement/result before the next group is prepared.
    operation.check()
}

fn committed_many_members(
    rows: &mut duckdb::Rows<'_>,
    frame: &mut Frame,
    size: FrameSize,
    operation: &Operation,
) -> Result<()> {
    for _index in 0..size.count {
        operation.check()?;
        let row = rows
            .next()?
            .ok_or(Error::Bound("committed App batch changed row count"))?;
        if required::<u64>(row, 21)? != frame.stamp.publication {
            return Err(Error::Bound("committed App batch row association"));
        }

        committed_member(row, frame)?;
    }

    Ok(())
}

fn committed_frame(endpoint: &Endpoint, size: FrameSize) -> Box<Frame> {
    Box::new(Frame {
        identity: endpoint.identity,
        stamp: endpoint.stamp,
        run_tick: endpoint.run_tick,
        wall_ns: endpoint.wall_ns,
        members: Vec::with_capacity(size.count),
        starts: Vec::new(),
        execs: Vec::new(),
        exits: Vec::new(),
        arena: Vec::with_capacity(size.bytes),
        usage_refused: endpoint.usage_refused,
        usage_gone: endpoint.usage_gone,
    })
}

fn committed_finish(frame: &Frame, size: FrameSize) -> Result<()> {
    if frame.members.len() != size.count {
        return Err(Error::Bound("committed App changed row count"));
    }

    if frame.arena.len() != size.bytes {
        return Err(Error::Bound("committed App changed byte count"));
    }

    assert!(frame.arena.len() <= super::PROGRAM_FRAME_BYTES_MAX);

    Ok(())
}

fn committed_members(
    connection: &Connection,
    source: &str,
    operation: &Operation,
    frame: &mut Frame,
) -> Result<()> {
    let publication = frame.stamp.publication;
    let limit = apps::PROCESS_INTERVALS_MAX + 1;
    let mut statement = connection.prepare(&format!(
        "SELECT {PROCESS_COLUMNS} \
        FROM {source} WHERE publication={publication} ORDER BY pid LIMIT {limit}"
    ))?;
    let mut rows = statement.query([])?;
    for index in 0..=apps::PROCESS_INTERVALS_MAX {
        operation.check()?;
        let Some(row) = rows.next()? else {
            return Ok(());
        };

        if index == frame.members.capacity() {
            return Err(Error::Bound("committed App process bound"));
        }

        committed_member(row, frame)?;
    }

    Err(Error::Bound("committed App process bound"))
}

fn committed_member(row: &Row<'_>, frame: &mut Frame) -> Result<()> {
    let pid = required(row, 0)?;
    if frame.members.last().is_some_and(|member| member.pid >= pid) {
        return Err(Error::Bound("duplicate committed App PID"));
    }

    frame.members.push(Member {
        pid,
        creation: committed_creation(row)?,
        program: committed_program(row, &mut frame.arena)?,
        usage: committed_usage(row)?,
    });

    Ok(())
}

fn required<T: duckdb::types::FromSql>(row: &Row<'_>, index: usize) -> Result<T> {
    row.get::<_, Option<T>>(index)?
        .ok_or(Error::Bound("missing committed App field"))
}

fn committed_creation(row: &Row<'_>) -> Result<Option<ProcessCreation>> {
    match (row.get(1)?, row.get(2)?) {
        (Some(unique_id), Some(pid_version)) => Ok(Some(ProcessCreation {
            unique_id,
            pid_version,
        })),
        (None, None) => Ok(None),
        _ => Err(Error::Bound("committed App creation")),
    }
}

fn committed_program(
    row: &Row<'_>,
    arena: &mut Vec<u8>,
) -> Result<std::result::Result<ProgramSpan, IdentityUnavailable>> {
    let reason = match required::<u8>(row, 3)? {
        0 => None,
        1 => Some(IdentityUnavailable::Refused),
        2 => Some(IdentityUnavailable::Gone),
        3 => Some(IdentityUnavailable::Unsupported),
        4 => Some(IdentityUnavailable::Changed),
        5 => Some(IdentityUnavailable::Invalid),
        6 => Some(IdentityUnavailable::Exhausted),
        _ => return Err(Error::Bound("committed App identity availability")),
    };

    if let Some(reason) = reason {
        return Ok(Err(reason));
    }

    let ValueRef::Blob(name) = row.get_ref(4)? else {
        return Err(Error::Bound("committed App executable bytes"));
    };

    let app_path = match row.get_ref(5)? {
        ValueRef::Null => None,
        ValueRef::Blob(bytes) => Some(bytes),
        _ => return Err(Error::Bound("committed App path bytes")),
    };
    let app_bytes = app_path.map_or(0, <[u8]>::len);

    if arena.len() + name.len() + app_bytes > arena.capacity() {
        return Err(Error::Bound("committed App changed byte count"));
    }

    Ok(Ok(ProgramSpan::capture(
        arena,
        &ProgramBytes {
            executable_name: name,
            app_path,
        },
    )))
}

fn committed_usage(row: &Row<'_>) -> Result<std::result::Result<Usage, Unavailable>> {
    match required::<u8>(row, 6)? {
        0 => Ok(Ok(Usage {
            awake_ns_at_start: required(row, 7)?,
            user_ns: required(row, 8)?,
            system_ns: required(row, 9)?,
            energy_nj: required(row, 10)?,
            instructions: required(row, 11)?,
            super_instructions: required(row, 12)?,
            cycles: required(row, 13)?,
            idle_wakeups: required(row, 14)?,
            interrupt_wakeups: required(row, 15)?,
            pageins: required(row, 16)?,
            resident_bytes: required(row, 17)?,
            footprint_bytes: required(row, 18)?,
            disk_read_bytes: required(row, 19)?,
            disk_written_bytes: required(row, 20)?,
        })),
        1 => Ok(Err(Unavailable::Refused)),
        2 => Ok(Err(Unavailable::Gone)),
        _ => Err(Error::Bound("committed App usage availability")),
    }
}

#[cfg(test)]
mod tests;
