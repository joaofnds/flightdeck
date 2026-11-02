use std::path::Path;
use std::sync::Arc;

use collector::ring::{FoundEvent, IdentityObservation, ProgramBuffers, RingReader, RunIdentity};
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

pub(crate) const PROGRAM_BYTES_MAX: usize = 2 * collector::ring::PROGRAM_PATH_BYTES_MAX;
pub(crate) const ROWS_BYTES_MAX: usize = BATCH_ROWS_MAX * size_of::<Row>();
const BATCH_ROWS_MIN: usize = (BATCH_BYTES_MAX - ROWS_BYTES_MAX) / PROGRAM_BYTES_MAX;
pub(crate) const BATCHES_MAX: usize = ROWS_MAX.div_ceil(BATCH_ROWS_MIN);
// Payload, Arrow/Polars columns and rechunking, compressed pages and codec workspace,
// writer buffer, bounded batch descriptors and per-file/footer bookkeeping coexist here.
pub(crate) const WORKING_BYTES: usize =
    BATCH_BYTES_MAX * 8 + 1024 * 1024 + BATCHES_MAX * size_of::<Batch>() + 1024 * 1024;
const _: () = {
    assert!(ROWS_BYTES_MAX < BATCH_BYTES_MAX);
    assert!(BATCH_ROWS_MIN > 0);
    assert!(BATCH_ROWS_MIN <= BATCH_ROWS_MAX);
    assert!(BATCHES_MAX < 10_000);
    assert!(BATCHES_MAX == 4096);
    assert!(crate::WRITE_BUFFER_BYTES == 1024 * 1024);
};

pub(crate) fn write_tail(
    pending: &Path,
    reader: RingReader,
    run: &RunIdentity,
    coverage: std::ops::Range<u64>,
    mut losses: Losses,
    memory_limit: usize,
) -> Result<Manifest> {
    let budget = Arc::new(ByteBudget::new(memory_limit));
    let _reservation = budget
        .reserve(WORKING_BYTES)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
    let captured = reader.dictionary_capacity().is_some();
    let name = reader.name().to_owned();
    let mut next = coverage.start;
    let mut source = Some(reader);
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
    for index in 0..BATCHES_MAX {
        let mut reader = match source.take() {
            Some(reader) => reader,
            None => RingReader::open_generation(&name)?,
        };
        if reader.identity().is_some_and(|identity| identity != *run)
            || reader.dictionary_capacity().is_some() != captured
            || reader.event_newest()? + 1 != coverage.end
        {
            return Err(Error::Bound("terminal source changed"));
        }
        reader.seek_event(next)?;
        let rows = read_batch(&mut reader, coverage.end, &mut losses)?;
        next = reader.next_event();
        drop(reader);
        if !rows.is_empty() {
            let mut frame = frame(&rows, captured)?;
            validate_frame(&frame, captured)?;
            crate::encoder::encode_write(
                &mut frame,
                crate::encoder::encode_open(&pending.join(super::batch_name(index)))?,
            )?;
            let batch = Batch {
                first: rows[0].event.number,
                end: rows[rows.len() - 1].event.number + 1,
                rows: collector::u64_from_usize(rows.len()),
            };
            manifest.recovery.rows += batch.rows;
            manifest.batches.push(batch);
        }
        if next >= coverage.end {
            manifest.recovery.losses = losses;
            manifest.validate()?;
            return Ok(manifest);
        }
    }
    Err(Error::Bound("terminal event batches"))
}

fn read_batch(reader: &mut RingReader, end: u64, losses: &mut Losses) -> Result<Vec<Row>> {
    let mut rows = Vec::with_capacity(BATCH_ROWS_MAX);
    let mut bytes = ROWS_BYTES_MAX;
    for _step in 0..=reader.shape().event_capacity {
        if reader.next_event() >= end
            || rows.len() == BATCH_ROWS_MAX
            || bytes + PROGRAM_BYTES_MAX > BATCH_BYTES_MAX
        {
            return Ok(rows);
        }
        let mut identity = IdentityObservation::EMPTY;
        let captured = reader.dictionary_capacity().is_some();
        let found = if captured {
            reader.read_event_identity(&mut identity)?
        } else {
            reader.read_event()?
        };
        match found {
            FoundEvent::Event {
                number,
                tick,
                event,
            } => {
                let row = copy_row(
                    reader,
                    &NumberedEvent {
                        number,
                        tick,
                        event: &event,
                    },
                    captured.then_some(&identity),
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
        if let Some(reference) = identity.program() {
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
        validate_numeric_usage(frame, row, exit && code == availability::MEASURED)?;
    }
    Ok(())
}

fn validate_numeric_usage(frame: &DataFrame, row: usize, present: bool) -> Result<()> {
    for name in [
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
    ] {
        if frame.column(name)?.u64()?.get(row).is_some() != present {
            return Err(Error::Bound("event usage payload"));
        }
    }
    Ok(())
}
