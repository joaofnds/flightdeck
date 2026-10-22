//! Typed committed endpoints feed the same owned aggregate as live publications.
use collector::processes::Usage;
use persistence::engine::{Cells, Engine};
use polars::prelude::{
    BinaryType, DataFrame, DataType, Int32Type, LazyFrame, PolarsPhysicalType, SortMultipleOptions,
    UInt8Type, UInt64Type, col, len, lit,
};

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
    pub events_first: u64,
    pub events_end: u64,
}

const PROCESS_COLUMNS: &str = "pid, process_unique_id, pid_version, identity_availability, \
    executable_name, app_path, usage_availability, awake_ns_at_start, user_ns, system_ns, \
    energy_nj, instructions, super_instructions, cycles, idle_wakeups, interrupt_wakeups, \
    pageins, resident_bytes, footprint_bytes, disk_read_bytes, disk_written_bytes";

pub(crate) struct Sources<'source> {
    pub processes: &'source str,
    pub events: &'source str,
}

impl Sources<'_> {
    pub(crate) fn query(&self) -> String {
        format!(
            "(SELECT {PROCESS_COLUMNS}, publication, 0::UTINYINT app_origin, \
            NULL::UBIGINT app_event, NULL::UTINYINT app_kind, NULL::UBIGINT app_tick, threads, \
            gpu_ns FROM {processes} UNION ALL SELECT pid, process_unique_id, pid_version, \
            identity_availability, CASE WHEN kind=3 THEN NULL ELSE executable_name END, \
            CASE WHEN kind=3 THEN NULL ELSE app_path END, usage_availability, \
            awake_ns_at_start, user_ns, system_ns, energy_nj, instructions, super_instructions, \
            cycles, idle_wakeups, interrupt_wakeups, pageins, resident_bytes, footprint_bytes, \
            disk_read_bytes, disk_written_bytes, publication, 1::UTINYINT app_origin, \
            event app_event, kind app_kind, run_tick app_tick, NULL::INTEGER threads, \
            NULL::UBIGINT gpu_ns FROM {events})",
            processes = self.processes,
            events = self.events,
        )
    }
}

struct FrameSize {
    count: usize,
    members: usize,
    starts: usize,
    execs: usize,
    exits: usize,
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
const BATCH_COLUMNS: usize = 28;
const ROW_LOGICAL_BYTES: usize = BATCH_COLUMNS * size_of::<Option<u64>>();
const SENTINEL_LOGICAL_BYTES: usize =
    ROW_LOGICAL_BYTES + 2 * collector::ring::PROGRAM_PATH_BYTES_MAX;
const BATCH_LOGICAL_BYTES_MAX: usize = 8 * 1024 * 1024;
const SINGLETON_LOGICAL_BYTES_MAX: usize = super::PROGRAM_FRAME_BYTES_MAX
    + (apps::PROCESS_INTERVALS_MAX + collector::tracker::EVENTS_PER_TICK_MAX) * ROW_LOGICAL_BYTES
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
    fn logical_bytes(&self) -> Result<usize> {
        self.count
            .checked_mul(ROW_LOGICAL_BYTES)
            .and_then(|fixed| fixed.checked_add(self.bytes))
            .ok_or(Error::Bound("committed App batch logical bytes"))
    }

    fn validate(&self) -> Result<()> {
        let maximum = apps::PROCESS_INTERVALS_MAX + collector::tracker::EVENTS_PER_TICK_MAX;
        if self.count > maximum {
            return Err(Error::Bound("committed App row bound"));
        }

        if self.members + self.starts + self.execs > apps::PROCESS_INTERVALS_MAX {
            return Err(Error::Bound("committed App process bound"));
        }

        if self.starts + self.execs + self.exits > collector::tracker::EVENTS_PER_TICK_MAX {
            return Err(Error::Bound("committed App event bound"));
        }

        if self.count != self.members + self.starts + self.execs + self.exits {
            return Err(Error::Bound("committed App row kind"));
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
        connection: &Engine,
        source: &str,
        operation: &Operation,
        output: &mut Option<Box<Self>>,
    ) -> Result<()> {
        *output = None;

        let result = Self::committed_many(
            std::slice::from_ref(endpoint),
            connection,
            source,
            operation,
            |frame| {
                *output = Some(frame);
                Ok(())
            },
        );

        if result.is_err() {
            *output = None;
        }

        result
    }

    /// Source is normalized immutable process/event data. Callbacks perform no SQL here.
    pub(crate) fn committed_many(
        endpoints: &[Endpoint],
        connection: &Engine,
        source: &str,
        operation: &Operation,
        mut consume: impl FnMut(Box<Self>) -> Result<()>,
    ) -> Result<()> {
        operation.attach(connection)?;
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
    connection: &Engine,
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

fn committed_many_size(publication: u64, sizes: &[PublicationSize]) -> &FrameSize {
    match sizes.binary_search_by_key(&publication, |entry| entry.publication) {
        Ok(index) => &sizes[index].size,
        Err(_) => &FrameSize {
            count: 0,
            members: 0,
            starts: 0,
            execs: 0,
            exits: 0,
            bytes: 0,
        },
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

        if endpoint.events_first > endpoint.events_end {
            return Err(Error::Bound("committed App event range"));
        }

        previous = Some(endpoint.stamp.publication);
        publications.push(endpoint.stamp.publication.to_string());
    }

    Ok(publications.join(","))
}

fn committed_many_sizes(
    batch: &Batch<'_>,
    connection: &Engine,
    operation: &Operation,
    output: &mut Vec<PublicationSize>,
) -> Result<()> {
    let plan = committed_many_sizes_plan(batch, connection)?;
    let frame = connection.collect(plan)?;
    let columns = SizeColumns::new(&frame)?;
    for row in 0..=batch.endpoints.len() {
        operation.check()?;
        if row == frame.height() {
            return Ok(());
        }

        if row == batch.endpoints.len() {
            return Err(Error::Bound("committed App batch size rows"));
        }

        let publication = required_cell(&columns.publication, row)?;
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
            count: usize::try_from(required_cell(&columns.count, row)?)
                .map_err(|_| Error::Bound("committed App count"))?,
            members: committed_many_sizes_count(&columns.members, row)?,
            starts: committed_many_sizes_count(&columns.starts, row)?,
            execs: committed_many_sizes_count(&columns.execs, row)?,
            exits: committed_many_sizes_count(&columns.exits, row)?,
            bytes: usize::try_from(required_cell(&columns.bytes, row)?)
                .map_err(|_| Error::Bound("committed App bytes"))?,
        };
        size.validate()?;

        output.push(PublicationSize { publication, size });
    }

    Err(Error::Bound("committed App batch size rows"))
}

/// The columns of a batch's size rows, in the order its size query aggregates them.
struct SizeColumns<'frame> {
    publication: Cells<'frame, UInt64Type>,
    count: Cells<'frame, UInt64Type>,
    bytes: Cells<'frame, UInt64Type>,
    members: Cells<'frame, UInt64Type>,
    starts: Cells<'frame, UInt64Type>,
    execs: Cells<'frame, UInt64Type>,
    exits: Cells<'frame, UInt64Type>,
}

impl<'frame> SizeColumns<'frame> {
    fn new(frame: &'frame DataFrame) -> Result<Self> {
        Ok(Self {
            publication: Cells::named(frame, "publication")?,
            count: Cells::named(frame, "count")?,
            bytes: Cells::named(frame, "bytes")?,
            members: Cells::named(frame, "members")?,
            starts: Cells::named(frame, "starts")?,
            execs: Cells::named(frame, "execs")?,
            exits: Cells::named(frame, "exits")?,
        })
    }
}

fn committed_many_sizes_plan(batch: &Batch<'_>, connection: &Engine) -> Result<LazyFrame> {
    let plan = connection.plan(&format!(
        "SELECT * FROM {source} WHERE publication IN ({publications})",
        source = batch.source,
        publications = batch.publications,
    ))?;

    let program_bytes = |name| {
        col(name)
            .binary()
            .size_bytes()
            .fill_null(lit(0_u32))
            .cast(DataType::UInt64)
    };
    let event_count = |kind| {
        col("app_origin")
            .eq(lit(1_u8))
            .and(col("app_kind").eq(lit(kind)))
            .cast(DataType::UInt64)
            .sum()
    };

    let limit = u32::try_from(batch.endpoints.len() + 1)
        .map_err(|_| Error::Bound("committed App batch endpoints"))?;

    Ok(plan
        .group_by([col("publication")])
        .agg([
            len().cast(DataType::UInt64).alias("count"),
            (program_bytes("executable_name") + program_bytes("app_path"))
                .sum()
                .alias("bytes"),
            col("app_origin")
                .eq(lit(0_u8))
                .cast(DataType::UInt64)
                .sum()
                .alias("members"),
            event_count(1_u8).alias("starts"),
            event_count(2_u8).alias("execs"),
            event_count(3_u8).alias("exits"),
        ])
        .sort(
            ["publication"],
            SortMultipleOptions {
                descending: vec![false],
                nulls_last: vec![false],
                multithreaded: false,
                maintain_order: true,
                limit: None,
            },
        )
        .limit(limit))
}

fn committed_many_rows(
    batch: &Batch<'_>,
    sizes: &[PublicationSize],
    connection: &Engine,
    operation: &Operation,
    consume: &mut impl FnMut(Box<Frame>) -> Result<()>,
) -> Result<()> {
    // This accounts requested logical cells and captured bytes, excluding engine overhead.
    let size = committed_many_query_size(batch.endpoints, sizes)?;
    assert!(
        size.rows
            <= ENDPOINTS_MAX
                * (apps::PROCESS_INTERVALS_MAX + collector::tracker::EVENTS_PER_TICK_MAX)
    );
    let limit = size.rows + 1;
    let frame = connection.query(&format!(
        "SELECT {PROCESS_COLUMNS}, publication, app_origin, app_event, app_kind, app_tick, \
        threads, gpu_ns FROM {source} WHERE publication IN ({publications}) \
        ORDER BY publication, app_origin, app_event, pid LIMIT {limit}",
        source = batch.source,
        publications = batch.publications,
    ))?;
    let mut rows = BatchCursor {
        columns: BatchColumns::new(&frame)?,
        row_count: frame.height(),
        index: 0,
    };
    for endpoint in batch.endpoints {
        operation.check()?;
        let size = committed_many_size(endpoint.stamp.publication, sizes);
        let mut frame = committed_frame(endpoint, size);
        committed_many_members(&mut rows, &mut frame, endpoint, size, operation)?;
        committed_finish(&frame, size)?;
        operation.check()?;
        consume(frame)?;
    }
    if rows.next().is_some() {
        return Err(Error::Bound("committed App batch changed row count"));
    }

    // Returning releases the native frame before the next group is collected.
    operation.check()
}

/// The rows of a batch, read in the order the batch query sorts them.
struct BatchCursor<'frame> {
    columns: BatchColumns<'frame>,
    row_count: usize,
    index: usize,
}

impl BatchCursor<'_> {
    /// The index of the next row, or `None` past the last.
    fn next(&mut self) -> Option<usize> {
        if self.index == self.row_count {
            return None;
        }

        let index = self.index;
        self.index += 1;

        Some(index)
    }
}

/// The columns of a batch's rows, each read whole in the type its normalized source holds it
/// in, which a cell by cell read would convert once for every cell.
struct BatchColumns<'frame> {
    pid: Cells<'frame, Int32Type>,
    process_unique_id: Cells<'frame, UInt64Type>,
    pid_version: Cells<'frame, Int32Type>,
    identity_availability: Cells<'frame, UInt8Type>,
    executable_name: Cells<'frame, BinaryType>,
    app_path: Cells<'frame, BinaryType>,
    usage_availability: Cells<'frame, UInt8Type>,
    usage: UsageColumns<'frame>,
    publication: Cells<'frame, UInt64Type>,
    app_origin: Cells<'frame, UInt8Type>,
    app_event: Cells<'frame, UInt64Type>,
    app_kind: Cells<'frame, UInt8Type>,
    app_tick: Cells<'frame, UInt64Type>,
    threads: Cells<'frame, Int32Type>,
    gpu_ns: Cells<'frame, UInt64Type>,
}

impl<'frame> BatchColumns<'frame> {
    /// The columns of `frame`, by the names the batch query gives them.
    fn new(frame: &'frame DataFrame) -> Result<Self> {
        // The batch bounds its bytes by this count of columns a row.
        assert_eq!(frame.width(), BATCH_COLUMNS);

        Ok(Self {
            pid: Cells::named(frame, "pid")?,
            process_unique_id: Cells::named(frame, "process_unique_id")?,
            pid_version: Cells::named(frame, "pid_version")?,
            identity_availability: Cells::named(frame, "identity_availability")?,
            executable_name: Cells::named(frame, "executable_name")?,
            app_path: Cells::named(frame, "app_path")?,
            usage_availability: Cells::named(frame, "usage_availability")?,
            usage: UsageColumns::new(frame)?,
            publication: Cells::named(frame, "publication")?,
            app_origin: Cells::named(frame, "app_origin")?,
            app_event: Cells::named(frame, "app_event")?,
            app_kind: Cells::named(frame, "app_kind")?,
            app_tick: Cells::named(frame, "app_tick")?,
            threads: Cells::named(frame, "threads")?,
            gpu_ns: Cells::named(frame, "gpu_ns")?,
        })
    }
}

/// The columns of a measured `Usage`.
struct UsageColumns<'frame> {
    awake_ns_at_start: Cells<'frame, UInt64Type>,
    user_ns: Cells<'frame, UInt64Type>,
    system_ns: Cells<'frame, UInt64Type>,
    energy_nj: Cells<'frame, UInt64Type>,
    instructions: Cells<'frame, UInt64Type>,
    super_instructions: Cells<'frame, UInt64Type>,
    cycles: Cells<'frame, UInt64Type>,
    idle_wakeups: Cells<'frame, UInt64Type>,
    interrupt_wakeups: Cells<'frame, UInt64Type>,
    pageins: Cells<'frame, UInt64Type>,
    resident_bytes: Cells<'frame, UInt64Type>,
    footprint_bytes: Cells<'frame, UInt64Type>,
    disk_read_bytes: Cells<'frame, UInt64Type>,
    disk_written_bytes: Cells<'frame, UInt64Type>,
}

impl<'frame> UsageColumns<'frame> {
    /// The usage columns of `frame`.
    fn new(frame: &'frame DataFrame) -> Result<Self> {
        Ok(Self {
            awake_ns_at_start: Cells::named(frame, "awake_ns_at_start")?,
            user_ns: Cells::named(frame, "user_ns")?,
            system_ns: Cells::named(frame, "system_ns")?,
            energy_nj: Cells::named(frame, "energy_nj")?,
            instructions: Cells::named(frame, "instructions")?,
            super_instructions: Cells::named(frame, "super_instructions")?,
            cycles: Cells::named(frame, "cycles")?,
            idle_wakeups: Cells::named(frame, "idle_wakeups")?,
            interrupt_wakeups: Cells::named(frame, "interrupt_wakeups")?,
            pageins: Cells::named(frame, "pageins")?,
            resident_bytes: Cells::named(frame, "resident_bytes")?,
            footprint_bytes: Cells::named(frame, "footprint_bytes")?,
            disk_read_bytes: Cells::named(frame, "disk_read_bytes")?,
            disk_written_bytes: Cells::named(frame, "disk_written_bytes")?,
        })
    }

    /// The usage of row `row`, whose every field must hold a value.
    fn usage(&self, row: usize) -> Result<Usage> {
        Ok(Usage {
            awake_ns_at_start: required_cell(&self.awake_ns_at_start, row)?,
            user_ns: required_cell(&self.user_ns, row)?,
            system_ns: required_cell(&self.system_ns, row)?,
            energy_nj: required_cell(&self.energy_nj, row)?,
            instructions: required_cell(&self.instructions, row)?,
            super_instructions: required_cell(&self.super_instructions, row)?,
            cycles: required_cell(&self.cycles, row)?,
            idle_wakeups: required_cell(&self.idle_wakeups, row)?,
            interrupt_wakeups: required_cell(&self.interrupt_wakeups, row)?,
            pageins: required_cell(&self.pageins, row)?,
            resident_bytes: required_cell(&self.resident_bytes, row)?,
            footprint_bytes: required_cell(&self.footprint_bytes, row)?,
            disk_read_bytes: required_cell(&self.disk_read_bytes, row)?,
            disk_written_bytes: required_cell(&self.disk_written_bytes, row)?,
        })
    }
}

fn committed_many_members(
    rows: &mut BatchCursor<'_>,
    frame: &mut Frame,
    endpoint: &Endpoint,
    size: &FrameSize,
    operation: &Operation,
) -> Result<()> {
    let mut next = endpoint.events_first;
    for _index in 0..size.count {
        operation.check()?;
        let row = rows
            .next()
            .ok_or(Error::Bound("committed App batch changed row count"))?;
        if required_cell(&rows.columns.publication, row)? != frame.stamp.publication {
            return Err(Error::Bound("committed App batch row association"));
        }

        match required_cell(&rows.columns.app_origin, row)? {
            0 => committed_member(&rows.columns, row, frame)?,
            1 => committed_event(&rows.columns, row, frame, endpoint, &mut next)?,
            _ => return Err(Error::Bound("committed App row origin")),
        }
    }

    super::events::finish(frame);

    Ok(())
}

fn committed_frame(endpoint: &Endpoint, size: &FrameSize) -> Box<Frame> {
    Box::new(Frame {
        identity: endpoint.identity,
        stamp: endpoint.stamp,
        run_tick: endpoint.run_tick,
        wall_ns: endpoint.wall_ns,
        members: Vec::with_capacity(size.members),
        starts: Vec::with_capacity(size.starts),
        execs: Vec::with_capacity(size.execs),
        exits: Vec::with_capacity(size.exits),
        arena: Vec::with_capacity(size.bytes),
        usage_refused: endpoint.usage_refused,
        usage_gone: endpoint.usage_gone,
    })
}

fn committed_finish(frame: &Frame, size: &FrameSize) -> Result<()> {
    if (
        frame.members.len(),
        frame.starts.len(),
        frame.execs.len(),
        frame.exits.len(),
    ) != (size.members, size.starts, size.execs, size.exits)
    {
        return Err(Error::Bound("committed App changed row count"));
    }

    if frame.arena.len() != size.bytes {
        return Err(Error::Bound("committed App changed byte count"));
    }

    assert!(frame.arena.len() <= super::PROGRAM_FRAME_BYTES_MAX);

    Ok(())
}

fn committed_member(columns: &BatchColumns<'_>, row: usize, frame: &mut Frame) -> Result<()> {
    let pid = required_cell(&columns.pid, row)?;
    if frame.members.last().is_some_and(|member| member.pid >= pid) {
        return Err(Error::Bound("duplicate committed App PID"));
    }

    frame.members.push(Member {
        pid,
        creation: committed_creation(columns, row)?,
        program: committed_program(columns, row, &mut frame.arena)?,
        usage: committed_usage(columns, row)?,
        gpu_ns: columns.gpu_ns.optional(row)?,
        threads: columns.threads.optional(row)?.and_then(super::threads),
    });

    Ok(())
}

fn committed_many_sizes_count(cells: &Cells<'_, UInt64Type>, row: usize) -> Result<usize> {
    let count = required_cell(cells, row)?;
    let maximum = apps::PROCESS_INTERVALS_MAX + collector::tracker::EVENTS_PER_TICK_MAX;
    let count = usize::try_from(count).map_err(|_| Error::Bound("committed App count"))?;
    if count > maximum {
        return Err(Error::Bound("committed App count"));
    }

    Ok(count)
}

fn committed_event(
    columns: &BatchColumns<'_>,
    row: usize,
    frame: &mut Frame,
    endpoint: &Endpoint,
    next: &mut u64,
) -> Result<()> {
    let number = required_cell(&columns.app_event, row)?;
    if number < *next {
        return Err(Error::Bound("committed App event order"));
    }

    if number >= endpoint.events_end {
        return Err(Error::Bound("committed App event range"));
    }

    if required_cell(&columns.app_tick, row)? != endpoint.run_tick {
        return Err(Error::Bound("committed App event tick"));
    }

    *next = number
        .checked_add(1)
        .ok_or(Error::Bound("committed App event number"))?;

    let pid = required_cell(&columns.pid, row)?;
    let creation = committed_creation(columns, row)?;

    match required_cell(&columns.app_kind, row)? {
        1 => {
            committed_event_program(columns, row)?;
            frame.starts.push(super::Start {
                number,
                pid,
                creation,
                program: committed_program(columns, row, &mut frame.arena)?,
                ambiguous: false,
                exec_number: None,
            });
        }
        2 => {
            committed_event_program(columns, row)?;
            frame.execs.push(super::Exec {
                number,
                pid,
                creation,
                program: committed_program(columns, row, &mut frame.arena)?,
            });
        }
        3 => frame.exits.push(super::Exit {
            number,
            pid,
            creation,
            usage: committed_usage(columns, row)?,
            ambiguous: false,
        }),
        _ => return Err(Error::Bound("committed App event kind")),
    }

    Ok(())
}

fn committed_event_program(columns: &BatchColumns<'_>, row: usize) -> Result<()> {
    if required_cell(&columns.usage_availability, row)? != 3 {
        return Err(Error::Bound("committed App event program usage"));
    }

    Ok(())
}

fn required_cell<'frame, T: PolarsPhysicalType>(
    cells: &Cells<'frame, T>,
    row: usize,
) -> Result<T::Physical<'frame>> {
    cells
        .optional(row)?
        .ok_or(Error::Bound("missing committed App field"))
}

fn committed_creation(columns: &BatchColumns<'_>, row: usize) -> Result<Option<ProcessCreation>> {
    match (
        columns.process_unique_id.optional(row)?,
        columns.pid_version.optional(row)?,
    ) {
        (Some(unique_id), Some(pid_version)) => Ok(Some(ProcessCreation {
            unique_id,
            pid_version,
        })),
        (None, None) => Ok(None),
        _ => Err(Error::Bound("committed App creation")),
    }
}

fn committed_program(
    columns: &BatchColumns<'_>,
    row: usize,
    arena: &mut Vec<u8>,
) -> Result<std::result::Result<ProgramSpan, IdentityUnavailable>> {
    let reason = match required_cell(&columns.identity_availability, row)? {
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

    let name = columns
        .executable_name
        .optional(row)?
        .ok_or(Error::Bound("committed App executable bytes"))?;
    let app_path = columns.app_path.optional(row)?;
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

fn committed_usage(
    columns: &BatchColumns<'_>,
    row: usize,
) -> Result<std::result::Result<Usage, Unavailable>> {
    match required_cell(&columns.usage_availability, row)? {
        0 => Ok(Ok(columns.usage.usage(row)?)),
        1 => Ok(Err(Unavailable::Refused)),
        2 => Ok(Err(Unavailable::Gone)),
        _ => Err(Error::Bound("committed App usage availability")),
    }
}

#[cfg(test)]
mod tests;
