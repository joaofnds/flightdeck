use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender};

use collector::ring::{
    FoundEvent, IdentityObservation, ProgramBuffers, ProgramLengths, RingReader, RunIdentity,
};
use polars::prelude::*;

use super::{Batch, Manifest, Recovery};
use crate::encoder::{columns, usage_columns};
use crate::event_observation::{BATCH_BYTES_MAX, BATCH_ROWS_MAX, ROWS_MAX};
use crate::owned_identity::{OwnedIdentity, copy_identity_code, usage_code};
use crate::resources::ByteBudget;
use crate::window::EventRecord;
use crate::{Error, Losses, NumberedEvent, Result};

pub(crate) struct Row {
    pub publication: Option<u64>,
    pub event: EventRecord,
    pub identity: Option<OwnedIdentity>,
    pub name: Option<Vec<u8>>,
    pub app: Option<Vec<u8>>,
}

struct RowIdentity {
    identity: OwnedIdentity,
    name: Option<Vec<u8>>,
    app: Option<Vec<u8>>,
}

pub(crate) const PROGRAM_BYTES_MAX: usize = 2 * collector::ring::PROGRAM_PATH_BYTES_MAX;
pub(crate) const ROWS_BYTES_MAX: usize = BATCH_ROWS_MAX * size_of::<Row>();
pub(crate) const BATCH_ROWS_MIN: usize = (BATCH_BYTES_MAX - ROWS_BYTES_MAX) / PROGRAM_BYTES_MAX;
// The census and the index are sized to this count, which a tail in batches of 8 MiB reached.
pub(crate) const BATCHES_MAX: usize = 4096;
// Payload, Arrow/Polars columns and rechunking, compressed pages and codec workspace,
// writer buffer, bounded batch descriptors and per-file/footer bookkeeping coexist here.
pub(crate) const WORKING_BYTES: usize =
    BATCH_BYTES_MAX * 4 + 1024 * 1024 + BATCHES_MAX * size_of::<Batch>() + 1024 * 1024;
/// What `read_ahead` holds beside the batch being written, the rows of one batch and its frame.
pub(crate) const READ_AHEAD_BYTES: usize = BATCH_BYTES_MAX * 2;
const _: () = {
    assert!(ROWS_BYTES_MAX < BATCH_BYTES_MAX);
    assert!(BATCH_ROWS_MIN > 0);
    assert!(BATCH_ROWS_MIN <= BATCH_ROWS_MAX);
    assert!(BATCHES_MAX < 10_000);
    assert!(ROWS_MAX.div_ceil(BATCH_ROWS_MIN) <= BATCHES_MAX);
    assert!(crate::WRITE_BUFFER_BYTES == 1024 * 1024);
    assert!(READ_AHEAD_BYTES == 2 * BATCH_BYTES_MAX);
};

#[cfg(test)]
mod tests;

/// A batch's rows as the frame its file takes, checked before anything writes it, and what the
/// index says of it.
pub(crate) struct FramedBatch {
    pub frame: DataFrame,
    pub batch: Batch,
}

pub(crate) fn framed(rows: &[Row], identities: bool) -> Result<FramedBatch> {
    let frame = frame(rows, identities)?;
    validate_frame(&frame, identities)?;
    Ok(FramedBatch {
        frame,
        batch: Batch {
            first: rows[0].event.number,
            end: rows[rows.len() - 1].event.number + 1,
            rows: collector::u64_from_usize(rows.len()),
        },
    })
}

/// Runs `read` on a thread of its own and `write` on this one, for each batch `read` sends, so
/// the batch being written has the next one coming. The channel holds nothing, so `read` waits
/// with one batch in hand until `write` takes it, and the files change on this thread in the order
/// they would with no second thread. An error from `write` stops `read` at its next send, and one
/// from `read` arrives after `write` has seen every batch sent before it.
pub(crate) fn read_ahead<Item: Send>(
    read: impl FnOnce(&SyncSender<Item>) -> Result<()> + Send,
    write: impl FnMut(Item) -> Result<()>,
) -> Result<()> {
    std::thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::sync_channel(0);
        let reader = std::thread::Builder::new()
            .name("event-read-ahead".to_owned())
            .spawn_scoped(scope, move || read(&sender))?;
        let written = read_ahead_write(&receiver, write);
        drop(receiver);
        let read = reader
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        if let (Err(_), Err(error)) = (&written, &read) {
            collector::diagnostics::error(format_args!("event read ahead also failed: {error}"));
        }

        written.and(read)
    })
}

fn read_ahead_write<Item>(
    receiver: &Receiver<Item>,
    mut write: impl FnMut(Item) -> Result<()>,
) -> Result<()> {
    for _batch in 0..BATCHES_MAX {
        let Ok(item) = receiver.recv() else {
            return Ok(());
        };
        write(item)?;
    }
    match receiver.recv() {
        Ok(_) => Err(Error::Bound("event batches read ahead")),
        Err(_) => Ok(()),
    }
}

/// Writes the tail into the file `create` gives for each batch, by its position among them.
pub(crate) fn write_tail_into<File: std::io::Write>(
    reader: RingReader,
    run: &RunIdentity,
    coverage: std::ops::Range<u64>,
    mut losses: Losses,
    budget: &Arc<ByteBudget>,
    mut create: impl FnMut(usize) -> Result<File>,
) -> Result<Manifest> {
    let _reservation = budget
        .reserve(WORKING_BYTES + READ_AHEAD_BYTES)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
    let captured = reader.dictionary_capacity().is_some();
    let name = reader.name().to_owned();
    drop(reader);
    let mut manifest = Manifest {
        recovery: Recovery {
            run: *run,
            events_first: coverage.start,
            events_end: coverage.end,
            rows: 0,
            losses,
        },
        identities: captured,
        batches: Vec::with_capacity(BATCHES_MAX),
    };
    let source = TailSource {
        name: &name,
        run,
        coverage: &coverage,
        captured,
    };
    read_ahead(
        |batches| write_tail_read(&source, &mut losses, batches),
        |mut framed: FramedBatch| {
            let file = create(manifest.batches.len())?;
            crate::encoder::encode_write_event_batch(&mut framed.frame, file)?;
            manifest.recovery.rows += framed.batch.rows;
            manifest.batches.push(framed.batch);
            Ok(())
        },
    )?;
    manifest.recovery.losses = losses;
    manifest.validate()?;
    Ok(manifest)
}

struct TailSource<'tail> {
    name: &'tail str,
    run: &'tail RunIdentity,
    coverage: &'tail std::ops::Range<u64>,
    captured: bool,
}

/// `write_tail_into`'s reading, which opens the ring for each batch so the pages one batch touched
/// stop counting against the process.
fn write_tail_read(
    source: &TailSource<'_>,
    losses: &mut Losses,
    batches: &SyncSender<FramedBatch>,
) -> Result<()> {
    let mut next = source.coverage.start;
    for _batch in 0..=BATCHES_MAX {
        let mut reader = RingReader::open_generation(source.name)?;
        if reader
            .identity()
            .is_some_and(|identity| identity != *source.run)
            || reader.dictionary_capacity().is_some() != source.captured
            || reader.event_newest()? + 1 != source.coverage.end
        {
            return Err(Error::Bound("terminal source changed"));
        }
        reader.seek_event(next)?;
        let rows = read_batch(&mut reader, source.coverage.end, losses)?;
        next = reader.next_event();
        drop(reader);
        if rows.is_empty() {
            assert!(next >= source.coverage.end);
            return Ok(());
        }
        if batches.send(framed(&rows, source.captured)?).is_err() || next >= source.coverage.end {
            return Ok(());
        }
    }
    Err(Error::Bound("terminal event batches"))
}

fn read_batch(reader: &mut RingReader, end: u64, losses: &mut Losses) -> Result<Vec<Row>> {
    let mut rows = Vec::with_capacity(BATCH_ROWS_MAX);
    let mut bytes = ROWS_BYTES_MAX;
    let mut name = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let mut app = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let mut buffers = ProgramBuffers {
        executable_name: &mut name,
        app_path: &mut app,
    };
    for _step in 0..=reader.shape().event_capacity {
        if reader.next_event() >= end
            || rows.len() == BATCH_ROWS_MAX
            || bytes + PROGRAM_BYTES_MAX > BATCH_BYTES_MAX
        {
            return Ok(rows);
        }
        let mut identity = IdentityObservation::EMPTY;
        let captured = reader.dictionary_capacity().is_some();
        let (found, lengths) = if captured {
            reader.read_event_identity_bytes(&mut identity, &mut buffers)?
        } else {
            (reader.read_event()?, ProgramLengths::default())
        };
        match found {
            FoundEvent::Event {
                number,
                tick,
                event,
            } => {
                let row = copy_row_read(
                    &NumberedEvent {
                        number,
                        tick,
                        event: &event,
                    },
                    captured.then_some(&identity),
                    &buffers,
                    lengths,
                )?;
                bytes +=
                    row.name.as_ref().map_or(0, Vec::len) + row.app.as_ref().map_or(0, Vec::len);
                rows.push(row);
            }
            FoundEvent::Missed { first, end: missed } => {
                losses.events += missed.min(end).saturating_sub(first);
            }
            FoundEvent::Nothing => return Err(Error::Bound("terminal event frontier")),
        }
    }
    Err(Error::Bound("terminal event scan"))
}

pub(crate) fn copy_row(
    reader: &RingReader,
    event: &NumberedEvent<'_>,
    identity: Option<&IdentityObservation>,
) -> Result<Row> {
    let mut row = copy_row_fields(event, identity)?;
    if let Some(reference) = identity.and_then(IdentityObservation::program) {
        let mut name = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
        let mut app = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
        let (name_length, app_length) = reader.copy_program(
            reference,
            &mut ProgramBuffers {
                executable_name: &mut name,
                app_path: &mut app,
            },
        )?;
        row.name = Some(name[..name_length].to_vec());
        row.app = (app_length > 0).then(|| app[..app_length].to_vec());
    }
    Ok(row)
}

/// `copy_row` for an identity whose read left its program bytes in `buffers` and returned their
/// `lengths`. Recovering
/// the most rows a recording retains has a 30-second deadline, and a second copy from the ring
/// would cost it time and find nothing, since those bytes are immutable and that read validated
/// them.
pub(crate) fn copy_row_read(
    event: &NumberedEvent<'_>,
    identity: Option<&IdentityObservation>,
    buffers: &ProgramBuffers<'_>,
    lengths: ProgramLengths,
) -> Result<Row> {
    let mut row = copy_row_fields(event, identity)?;
    if identity.and_then(IdentityObservation::program).is_some() {
        row.name = Some(buffers.executable_name[..lengths.executable_name].to_vec());
        row.app = (lengths.app_path > 0).then(|| buffers.app_path[..lengths.app_path].to_vec());
    }
    Ok(row)
}

fn copy_row_fields(
    event: &NumberedEvent<'_>,
    identity: Option<&IdentityObservation>,
) -> Result<Row> {
    let event = EventRecord::copy(event);
    let mut row = Row {
        publication: None,
        event,
        identity: None,
        name: None,
        app: None,
    };
    if let Some(identity) = identity {
        if identity.pid != row.event.pid || usage_code(identity.usage) != row.event.availability {
            return Err(Error::Bound("terminal native identity"));
        }
        row.identity = Some(OwnedIdentity {
            pid: identity.pid,
            creation: identity.creation().copied(),
            availability: copy_identity_code(identity.availability()),
            usage: usage_code(identity.usage),
            program: None,
        });
    }
    Ok(row)
}

pub(crate) fn frame(rows: &[Row], identities: bool) -> Result<DataFrame> {
    let mut columns = columns!(rows;
        "publication" => |row| row.publication,
        "event" => |row| row.event.number,
        "run_tick" => |row| row.event.tick,
        "pid" => |row| row.event.pid,
        "kind" => |row| row.event.kind,
        "coalition" => |row| row.event.coalition,
        "availability" => |row| row.event.availability,
        "noticed" => |row| row.event.noticed,
    );
    columns.extend(usage_columns(rows, |row| row.event.usage.as_ref()));
    if identities {
        columns.extend(columns!(rows;
            "identity_version" => |_| crate::schema::IDENTITY_VERSION,
            "identity_availability" => |row| row.identity.as_ref().map(|value| value.availability),
            "usage_availability" => |row| row.identity.as_ref().map(|value| value.usage),
            "process_unique_id" => |row| {
                row.identity.as_ref().and_then(|value| {
                    value.creation.map(|creation| creation.unique_id)
                })
            },
            "pid_version" => |row| {
                row.identity.as_ref().and_then(|value| {
                    value.creation.map(|creation| creation.pid_version)
                })
            },
            "executable_name" => |row| row.name.as_deref(),
            "app_path" => |row| row.app.as_deref(),
        ));
    }
    columns.extend(columns!(rows;
        "event_observation_version" => |_| crate::event_observation::VERSION,
        "event_observation_availability" => |row| {
            if row.publication.is_some() { crate::event_observation::ASSOCIATED }
            else { crate::event_observation::UNAVAILABLE }
        },
    ));
    Ok(DataFrame::new(rows.len(), columns)?)
}

pub(crate) fn validate_frame(frame: &DataFrame, identities: bool) -> Result<()> {
    validate_numeric_frame(frame)?;
    crate::event_observation::validate(frame, 0..frame.height())?;
    if identities {
        let selected = crate::identity::event_validation_frame(frame)?;
        crate::identity::validate(
            crate::schema::Table::Events,
            &selected,
            0..selected.height(),
        )?;
    }
    Ok(())
}

pub(super) fn empty(identities: bool) -> Result<DataFrame> {
    frame(&[], identities)
}

const USAGE_COLUMNS: [&str; 14] = [
    "awake_ns_at_start",
    "user_ns",
    "system_ns",
    "energy_nj",
    "instructions",
    "super_instructions",
    "cycles",
    "idle_wakeups",
    "interrupt_wakeups",
    "pageins",
    "resident_bytes",
    "footprint_bytes",
    "disk_read_bytes",
    "disk_written_bytes",
];

fn validate_numeric_frame(frame: &DataFrame) -> Result<()> {
    use crate::schema::{availability, event_kind, noticed};
    if frame.height() > BATCH_ROWS_MAX {
        return Err(Error::Bound("event numeric rows"));
    }
    let kinds = frame.column("kind")?.u8()?;
    let availability = frame.column("availability")?.u8()?;
    let noticed = frame.column("noticed")?.u8()?;
    let coalition = frame.column("coalition")?.u64()?;
    for name in ["event", "run_tick", "pid"] {
        if frame.column(name)?.null_count() != 0 {
            return Err(Error::Bound("event required numeric value"));
        }
    }
    let mut usage = Vec::with_capacity(USAGE_COLUMNS.len());
    for name in USAGE_COLUMNS {
        usage.push(frame.column(name)?.u64()?);
    }
    for row in 0..frame.height() {
        let kind = kinds.get(row).ok_or(Error::Bound("event kind"))?;
        let code = availability
            .get(row)
            .ok_or(Error::Bound("event availability"))?;
        let exit = kind == event_kind::EXIT;
        let valid = match kind {
            event_kind::START | event_kind::EXEC => {
                code == availability::NOT_APPLICABLE && noticed.get(row).is_none()
            }
            event_kind::EXIT => {
                matches!(
                    code,
                    availability::MEASURED | availability::REFUSED | availability::GONE
                ) && matches!(
                    noticed.get(row),
                    Some(noticed::ON_EXIT | noticed::ALREADY_EXITED)
                )
            }
            _ => false,
        };
        if !valid || (kind != event_kind::START && coalition.get(row).is_some()) {
            return Err(Error::Bound("event numeric semantics"));
        }
        validate_numeric_usage(&usage, row, exit && code == availability::MEASURED)?;
    }
    Ok(())
}

fn validate_numeric_usage(usage: &[&UInt64Chunked], row: usize, present: bool) -> Result<()> {
    for column in usage {
        if column.get(row).is_some() != present {
            return Err(Error::Bound("event usage payload"));
        }
    }
    Ok(())
}

/// Copies one row from a fully validated bounded frame, retaining every persisted field.
pub(crate) fn owned_row(frame: &DataFrame, position: usize, captured: bool) -> Result<Row> {
    if position >= frame.height() {
        return Err(Error::Bound("owned event position"));
    }
    let number =
        |name: &str| -> Result<Option<u64>> { Ok(frame.column(name)?.u64()?.get(position)) };
    let required =
        |name: &str| -> Result<u64> { number(name)?.ok_or(Error::Bound("owned event number")) };
    let byte = |name: &str| -> Result<u8> {
        frame
            .column(name)?
            .u8()?
            .get(position)
            .ok_or(Error::Bound("owned event byte"))
    };
    let pid = frame
        .column("pid")?
        .i32()?
        .get(position)
        .ok_or(Error::Bound("owned event pid"))?;
    let usage = if number("user_ns")?.is_some() {
        Some(collector::processes::Usage {
            awake_ns_at_start: required("awake_ns_at_start")?,
            user_ns: required("user_ns")?,
            system_ns: required("system_ns")?,
            energy_nj: required("energy_nj")?,
            instructions: required("instructions")?,
            super_instructions: required("super_instructions")?,
            cycles: required("cycles")?,
            idle_wakeups: required("idle_wakeups")?,
            interrupt_wakeups: required("interrupt_wakeups")?,
            pageins: required("pageins")?,
            resident_bytes: required("resident_bytes")?,
            footprint_bytes: required("footprint_bytes")?,
            disk_read_bytes: required("disk_read_bytes")?,
            disk_written_bytes: required("disk_written_bytes")?,
        })
    } else {
        None
    };
    let mut row = Row {
        publication: number("publication")?,
        event: EventRecord {
            number: required("event")?,
            tick: required("run_tick")?,
            pid,
            kind: byte("kind")?,
            coalition: number("coalition")?,
            usage,
            availability: byte("availability")?,
            noticed: frame.column("noticed")?.u8()?.get(position),
        },
        identity: None,
        name: None,
        app: None,
    };
    if captured {
        let identity = owned_row_identity(frame, position, pid)?;
        row.identity = Some(identity.identity);
        row.name = identity.name;
        row.app = identity.app;
    }
    Ok(row)
}

fn owned_row_identity(frame: &DataFrame, position: usize, pid: i32) -> Result<RowIdentity> {
    let number =
        |name: &str| -> Result<Option<u64>> { Ok(frame.column(name)?.u64()?.get(position)) };
    let byte = |name: &str| -> Result<u8> {
        frame
            .column(name)?
            .u8()?
            .get(position)
            .ok_or(Error::Bound("owned identity byte"))
    };
    let identity = OwnedIdentity {
        pid,
        creation: match (
            number("process_unique_id")?,
            frame.column("pid_version")?.i32()?.get(position),
        ) {
            (Some(unique_id), Some(pid_version)) => Some(collector::ring::ProcessCreation {
                unique_id,
                pid_version,
            }),
            (None, None) => None,
            _ => return Err(Error::Bound("owned creation pair")),
        },
        availability: byte("identity_availability")?,
        usage: byte("usage_availability")?,
        program: None,
    };
    let name = frame
        .column("executable_name")?
        .binary()?
        .get(position)
        .map(<[u8]>::to_vec);
    let app = frame
        .column("app_path")?
        .binary()?
        .get(position)
        .map(<[u8]>::to_vec);
    Ok(RowIdentity {
        identity,
        name,
        app,
    })
}
