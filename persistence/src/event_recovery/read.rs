use std::path::{Path, PathBuf};
use std::sync::Arc;

use collector::ring::RunIdentity;
use polars::prelude::*;

use super::{EventPage, Manifest};
use crate::engine::{AdmissionRefusal, AdmittedReadRefusal, AdmittedTable, DecodeLimits};
use crate::event_observation::{BATCH_BYTES_MAX, BATCH_ROWS_MAX};
use crate::resources::ByteBudget;
use crate::{Error, Result};

// Plain scalar values, binary offsets, validity/definition levels and page framing fit
// twice the owned payload. Compressed bytes additionally allow bounded footer/framing.
const EXPANDED_BYTES_MAX: usize = BATCH_BYTES_MAX * 2;
const COMPRESSED_BYTES_MAX: usize = EXPANDED_BYTES_MAX + 8 * 1024 * 1024;
const READ_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// Reads one page of a run's saved tail. A walk of the whole tail uses `Pages`, which checks the
/// tail's files once rather than once a page.
pub fn read_page(
    root: &Path,
    run: &RunIdentity,
    first_event: u64,
    limit: usize,
) -> Result<EventPage> {
    Pages::open(root, run)?.read(first_event, limit)
}

/// A run's saved tail.
pub struct Pages {
    directory: PathBuf,
    manifest: Manifest,
}

impl Pages {
    pub fn open(root: &Path, run: &RunIdentity) -> Result<Self> {
        super::validate_run(root, run)?;
        let budget = Arc::new(ByteBudget::new(READ_BUDGET_BYTES));
        let catalog = crate::canonical::catalog::Catalog::open(root, &budget)?;
        let (manifest, directory) = catalog
            .terminal_of(run, &mut crate::canonical::ReadWork::default())?
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;

        Ok(Self {
            directory: root.join(directory),
            manifest,
        })
    }

    /// Reads at most one physical batch, validates all its rows, then selects ascending events.
    /// `next` resumes after returned rows or at the next batch, including gaps in event numbers.
    pub fn read(&self, first_event: u64, limit: usize) -> Result<EventPage> {
        read_page_of(&self.directory, &self.manifest, first_event, limit)
    }
}

fn read_page_of(
    directory: &Path,
    manifest: &Manifest,
    first_event: u64,
    limit: usize,
) -> Result<EventPage> {
    if first_event == 0 || limit == 0 || limit > BATCH_ROWS_MAX {
        return Err(Error::Bound("terminal event page"));
    }
    let budget = Arc::new(ByteBudget::new(READ_BUDGET_BYTES));
    let _reservation = budget
        .reserve(super::batch::WORKING_BYTES)
        .ok_or(Error::Bound("terminal decode bytes"))?;
    let selected = manifest
        .batches
        .iter()
        .position(|batch| batch.end > first_event);
    let mut rows = super::batch::empty(manifest.identities)?;
    let mut next = None;
    if let Some(index) = selected {
        rows = read_batch(directory, manifest, index, &budget)?;
        let event = rows.column("event")?.u64()?;
        let offset = event
            .into_no_null_iter()
            .take_while(|number| *number < first_event)
            .count();
        let length = (rows.height() - offset).min(limit);
        next = if offset + length < rows.height() {
            event.get(offset + length)
        } else {
            manifest.batches.get(index + 1).map(|batch| batch.first)
        };
        rows = rows.slice(
            i64::try_from(offset).map_err(|_| Error::Bound("terminal page offset"))?,
            length,
        );
    }
    let rows =
        crate::engine::frame_lease::copy_event_frame(&rows, &budget).map_err(
            |error| match error {
                AdmittedReadRefusal::Admission(error) => admission_error(error),
                AdmittedReadRefusal::Decode(error) => error,
            },
        )?;
    Ok(EventPage {
        rows,
        next,
        coverage: manifest.recovery.events_first..manifest.recovery.events_end,
        losses: manifest.recovery.losses,
    })
}

fn read_batch(
    directory: &Path,
    manifest: &Manifest,
    index: usize,
    budget: &Arc<ByteBudget>,
) -> Result<DataFrame> {
    let batch = &manifest.batches[index];
    let path = directory.join(super::batch_name(index));
    let file = collector::recording::private::open_private_file(&path, false)?;
    read_batch_file(file, batch, manifest.identities, false, budget)
}

pub(crate) fn read_batch_file(
    file: std::fs::File,
    batch: &super::Batch,
    identities: bool,
    associated: bool,
    budget: &Arc<ByteBudget>,
) -> Result<DataFrame> {
    read_batch_file_counted(file, batch, identities, associated, budget, &mut 0)
}
pub(crate) fn read_batch_file_counted(
    file: std::fs::File,
    batch: &super::Batch,
    identities: bool,
    associated: bool,
    budget: &Arc<ByteBudget>,
    decoded_bytes: &mut u64,
) -> Result<DataFrame> {
    read_batch_file_with_work(
        file,
        batch,
        identities,
        associated,
        budget,
        decoded_bytes,
        None,
    )
}
pub(crate) fn read_batch_file_with_work(
    file: std::fs::File,
    batch: &super::Batch,
    identities: bool,
    associated: bool,
    budget: &Arc<ByteBudget>,
    decoded_bytes: &mut u64,
    work: Option<&dyn crate::engine::RawTableWork>,
) -> Result<DataFrame> {
    let frame = decode_batch_file(file, identities, budget, work)?;
    *decoded_bytes += collector::u64_from_usize(frame.estimated_size(true));
    read_batch_validate(&frame, batch, identities, associated)?;
    Ok(frame)
}

fn decode_batch_file(
    file: std::fs::File,
    identities: bool,
    budget: &Arc<ByteBudget>,
    work: Option<&dyn crate::engine::RawTableWork>,
) -> Result<DataFrame> {
    decode_batch_file_columns(file, identities, false, budget, work)
}

fn decode_batch_file_columns(
    file: std::fs::File,
    identities: bool,
    numbers_only: bool,
    budget: &Arc<ByteBudget>,
    work: Option<&dyn crate::engine::RawTableWork>,
) -> Result<DataFrame> {
    let limits = DecodeLimits {
        compressed_bytes: collector::u64_from_usize(COMPRESSED_BYTES_MAX),
        expanded_bytes: collector::u64_from_usize(EXPANDED_BYTES_MAX),
        rows: collector::u64_from_usize(BATCH_ROWS_MAX),
        row_groups: 1,
        columns: model::catalog::EVENTS.column_count(),
    };
    let admitted = AdmittedTable::from_file_limited_counted(file, budget, &limits, work)
        .map_err(admission_error)?;
    if admitted.row_count() == 0 {
        return Err(Error::Bound("empty physical event batch"));
    }
    let profile =
        crate::identity::validate_fields(crate::schema::Table::Events, || admitted.fields())?;
    if profile != identities || !crate::event_observation::extended_fields(admitted.fields())? {
        return Err(Error::Bound("physical event profile"));
    }
    let mut whole: Vec<_> = model::catalog::EVENTS
        .columns
        .iter()
        .map(|column| column.name)
        .collect();
    if identities {
        whole.extend(model::catalog::IDENTITY.iter().map(|column| column.name));
    }
    whole.extend(
        model::catalog::EVENT_OBSERVATION
            .iter()
            .map(|column| column.name),
    );
    let columns = if numbers_only {
        &["event"][..]
    } else {
        &whole[..]
    };
    let frame = admitted
        .read_metadata_counted(
            columns,
            Some(u32::try_from(BATCH_ROWS_MAX).map_err(|_| Error::Bound("terminal batch rows"))?),
            work,
        )
        .map_err(|error| match error {
            AdmittedReadRefusal::Admission(error) => admission_error(error),
            AdmittedReadRefusal::Decode(error) => error,
        })?;
    Ok(frame)
}

/// Derives an original batch extent from fully validated rows, independently of its ordinal/index.
pub(crate) fn read_original_batch_file(
    file: std::fs::File,
    identities: bool,
    associated: bool,
    budget: &Arc<ByteBudget>,
    decoded_bytes: &mut u64,
) -> Result<super::Batch> {
    let frame = decode_batch_file(file, identities, budget, None)?;
    *decoded_bytes += collector::u64_from_usize(frame.estimated_size(true));
    let batch = read_original_batch_extent(&frame)?;
    read_batch_validate(&frame, &batch, identities, associated)?;
    if batch.first == 0 {
        return Err(Error::Bound("original event origin"));
    }
    Ok(batch)
}

/// An original batch's extent from its event numbers alone, for a proof that goes on to read every
/// batch whole. Reading each of them twice took 1.9 of the 4.1 s of a proof at the largest shape on
/// an M5 Pro.
pub(crate) fn read_original_batch_numbers(
    file: std::fs::File,
    identities: bool,
    budget: &Arc<ByteBudget>,
    decoded_bytes: &mut u64,
) -> Result<super::Batch> {
    let frame = decode_batch_file_columns(file, identities, true, budget, None)?;
    *decoded_bytes += collector::u64_from_usize(frame.estimated_size(true));
    let batch = read_original_batch_extent(&frame)?;
    if batch.first == 0 {
        return Err(Error::Bound("original event origin"));
    }
    Ok(batch)
}

fn read_original_batch_extent(frame: &DataFrame) -> Result<super::Batch> {
    let events = frame.column("event")?.u64()?;
    let first = events.get(0).ok_or(Error::Bound("original first event"))?;
    let last = events
        .get(frame.height() - 1)
        .ok_or(Error::Bound("original last event"))?;

    Ok(super::Batch {
        first,
        end: last
            .checked_add(1)
            .ok_or(Error::Bound("original event overflow"))?,
        rows: collector::u64_from_usize(frame.height()),
    })
}

fn validate_owned_payload(frame: &DataFrame, identities: bool) -> Result<()> {
    let mut payload = super::batch::ROWS_BYTES_MAX;
    if identities {
        for name in ["executable_name", "app_path"] {
            let bytes = frame.column(name)?.binary()?;
            for value in bytes.iter().flatten() {
                if value.len() > collector::ring::PROGRAM_PATH_BYTES_MAX {
                    return Err(Error::Bound("original program bytes"));
                }
                payload = payload
                    .checked_add(value.len())
                    .ok_or(Error::Bound("original payload"))?;
            }
        }
    }
    if payload > BATCH_BYTES_MAX {
        return Err(Error::Bound("original payload"));
    }
    Ok(())
}

fn read_batch_validate(
    frame: &DataFrame,
    batch: &super::Batch,
    identities: bool,
    associated: bool,
) -> Result<()> {
    if collector::u64_from_usize(frame.height()) != batch.rows
        || frame.estimated_size(true) > BATCH_BYTES_MAX * 2
    {
        return Err(Error::Bound("terminal batch payload"));
    }
    super::batch::validate_frame(frame, identities)?;
    validate_owned_payload(frame, identities)?;
    if !associated && frame.column("publication")?.null_count() != frame.height() {
        return Err(Error::Bound("terminal associated observation"));
    }
    let events = frame.column("event")?.u64()?;
    let mut previous = None;
    if events.null_count() != 0 {
        return Err(Error::Bound("terminal event number"));
    }
    for event in events.into_no_null_iter() {
        if event < batch.first
            || event >= batch.end
            || previous.is_some_and(|previous| event <= previous)
        {
            return Err(Error::Bound("terminal event order"));
        }
        previous = Some(event);
    }
    if events.get(0) != Some(batch.first) || previous != Some(batch.end - 1) {
        return Err(Error::Bound("terminal batch extent"));
    }
    Ok(())
}

fn admission_error(error: AdmissionRefusal) -> Error {
    match error {
        AdmissionRefusal::Budget => std::io::Error::from(std::io::ErrorKind::OutOfMemory).into(),
        AdmissionRefusal::Bound(message) => Error::Bound(message),
        AdmissionRefusal::Io { kind, raw_os_error } => Error::Io(raw_os_error.map_or_else(
            || std::io::Error::from(kind),
            std::io::Error::from_raw_os_error,
        )),
    }
}

#[cfg(test)]
pub(crate) fn physical_captured_frame(extra: usize) -> DataFrame {
    let rows: Vec<_> = (0..16_384)
        .map(|index| {
            let mut name = vec![b'x'; 240 + if index == 0 { extra } else { 0 }];
            name[..16].copy_from_slice(format!("{index:016x}").as_bytes());
            crate::event_recovery::batch::Row {
                publication: None,
                event: crate::window::EventRecord {
                    number: collector::u64_from_usize(index) + 1,
                    tick: 1,
                    pid: 41,
                    kind: crate::schema::event_kind::EXEC,
                    coalition: None,
                    usage: None,
                    availability: crate::schema::availability::NOT_APPLICABLE,
                    noticed: None,
                },
                identity: Some(crate::owned_identity::OwnedIdentity {
                    pid: 41,
                    creation: Some(collector::ring::ProcessCreation {
                        unique_id: 991,
                        pid_version: 7,
                    }),
                    availability: crate::schema::identity_availability::MEASURED,
                    usage: crate::schema::availability::NOT_APPLICABLE,
                    program: None,
                }),
                name: Some(name),
                app: None,
            }
        })
        .collect();
    crate::event_recovery::batch::frame(&rows, true).unwrap()
}
