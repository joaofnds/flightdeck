use super::metadata::{Body, CanonicalMetadata};
use super::tree::{CanonicalKey, Namespace, ReadWork, invalid};
use crate::engine::{AdmissionRefusal, AdmittedReadRefusal, AdmittedTable, DecodeLimits};
use crate::resources::{ByteBudget, ByteReservation};
use collector::recording::private::PrivateDirectory;
use collector::ring::RunIdentity;
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

const _: () =
    assert!((128 + 24 * 4096) * 2 + size_of::<CanonicalMetadata>() + 4097 + 512 < 256 * 1024);

pub(crate) struct AdmittedCanonical {
    pub metadata: CanonicalMetadata,
    pub device: u64,
    pub inode: u64,
    _metadata_reservation: ByteReservation,
}
pub(crate) fn admit(
    root: &PrivateDirectory,
    mut gate: File,
    namespace: Namespace,
    key: &CanonicalKey,
    run: &RunIdentity,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<AdmittedCanonical> {
    let reservation = budget
        .reserve(256 * 1024)
        .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
    let metadata = CanonicalMetadata::read(&mut gate)?;
    if metadata.run != *run {
        return Err(invalid());
    }
    metadata.validate_key(namespace, key)?;
    let directory = raw_directory(root, &metadata, work)?;
    work.raw_admissions += 1;
    work.namespace_attempts += 1;
    let raw = directory.open_file("metadata")?;
    let before = gate.metadata()?;
    let after = raw.metadata()?;
    if before.dev() != after.dev() || before.ino() != after.ino() {
        return Err(invalid());
    }
    verify_rows(&directory, &metadata, budget, work)?;
    Ok(AdmittedCanonical {
        metadata,
        device: before.dev(),
        inode: before.ino(),
        _metadata_reservation: reservation,
    })
}
fn raw_directory(
    root: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    work: &mut ReadWork,
) -> io::Result<PrivateDirectory> {
    work.namespace_attempts += 1;
    work.node_attempts += 1;
    let mut directory = root.open_directory(metadata.base.name())?;
    work.namespace_attempts += 1;
    work.node_attempts += 1;
    directory = directory
        .open_directory(&collector::recording::identity_text(metadata.transaction).to_string())?;
    work.namespace_attempts += 1;
    work.node_attempts += 1;
    directory = directory.open_directory(metadata.child.name())?;
    Ok(directory)
}
fn verify_rows(
    directory: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<()> {
    let _workspace = if matches!(metadata.body, Body::LossOnly) {
        None
    } else {
        Some(
            budget
                .reserve(crate::event_recovery::batch::WORKING_BYTES)
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?,
        )
    };
    let (rows, inherited) = match &metadata.body {
        Body::LossOnly => return Ok(()),
        Body::Ordinary(manifest) => {
            verify_rows_ordinary(directory, metadata, manifest, budget, work)?
        }
        Body::Events(manifest) => {
            verify_rows_batches(directory, metadata, &manifest.batches, None, budget, work)?
        }
    };
    let domain = &metadata.loss.inherited;
    if inherited.checked_add(domain.events) != Some(domain.event_inherited_end - domain.event_start)
        || (rows - inherited).checked_add(metadata.loss.events_new)
            != Some(metadata.loss.event_end - domain.event_inherited_end)
    {
        return Err(invalid());
    }
    Ok(())
}
fn verify_rows_ordinary(
    directory: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    manifest: &crate::Manifest,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<(u64, u64)> {
    let ticks = verify_ticks(directory, metadata, manifest, budget, work, None)?;
    work.namespace_attempts += 1;
    work.node_attempts += 1;
    let events = directory.open_directory("events")?;
    work.namespace_attempts += 1;
    work.raw_admissions += 1;
    let index =
        crate::event_table::index::Index::read_file(&mut events.open_file("index")?, manifest)
            .map_err(source_error)?;
    if index.identities != metadata.captured {
        return Err(invalid());
    }
    verify_rows_batches(
        &events,
        metadata,
        &index.batches,
        Some(&ticks),
        budget,
        work,
    )
}
fn verify_rows_batches(
    directory: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    batches: &[crate::event_recovery::Batch],
    ticks: Option<&[TickSpan]>,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<(u64, u64)> {
    let mut rows = 0;
    let mut inherited = 0;
    for (ordinal, batch) in batches.iter().enumerate() {
        let counts = verify_batch(
            directory,
            metadata,
            batch,
            &BatchMode {
                ordinal,
                associated: metadata.kind != crate::transaction::CanonicalKind::Terminal,
                ticks,
            },
            budget,
            work,
        )?;
        rows += counts.0;
        inherited += counts.1;
    }
    Ok((rows, inherited))
}
pub(crate) struct TickSpan {
    pub publication: u64,
    pub first: u64,
    pub end: u64,
}
pub(crate) struct BatchMode<'ticks> {
    pub ordinal: usize,
    pub associated: bool,
    pub ticks: Option<&'ticks [TickSpan]>,
}
pub(crate) fn verify_batch(
    directory: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    batch: &crate::event_recovery::Batch,
    mode: &BatchMode<'_>,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<(u64, u64)> {
    work.namespace_attempts += 1;
    let file = directory.open_file(&crate::event_recovery::batch_name(mode.ordinal))?;
    verify_batch_file(file, metadata, batch, mode, budget, work)
}
pub(crate) fn verify_batch_file(
    file: File,
    metadata: &CanonicalMetadata,
    batch: &crate::event_recovery::Batch,
    mode: &BatchMode<'_>,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<(u64, u64)> {
    let (frame, counts) = admit_batch_file(file, metadata, batch, mode, budget, work, None)?;
    drop(frame);
    Ok(counts)
}
pub(crate) fn admit_batch_file(
    file: File,
    metadata: &CanonicalMetadata,
    batch: &crate::event_recovery::Batch,
    mode: &BatchMode<'_>,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
    raw_work: Option<&dyn crate::engine::RawTableWork>,
) -> io::Result<(polars::prelude::DataFrame, (u64, u64))> {
    work.raw_admissions += 1;
    let frame = crate::event_recovery::read::read_batch_file_with_work(
        file,
        batch,
        metadata.captured,
        mode.associated,
        budget,
        &mut 0,
        raw_work,
    )
    .map_err(source_error)?;
    let numbers = frame
        .column("event")
        .map_err(crate::metadata::invalid)?
        .u64()
        .map_err(crate::metadata::invalid)?;
    if let Some(ticks) = mode.ticks {
        let publications = frame
            .column("publication")
            .map_err(crate::metadata::invalid)?
            .u64()
            .map_err(crate::metadata::invalid)?;
        for (index, number) in numbers.into_no_null_iter().enumerate() {
            match publications.get(index) {
                Some(publication)
                    if ticks.iter().any(|tick| {
                        tick.publication == publication && number >= tick.first && number < tick.end
                    }) => {}
                None if !ticks
                    .iter()
                    .any(|tick| number >= tick.first && number < tick.end) => {}
                _ => return Err(invalid()),
            }
        }
    }
    let inherited = numbers
        .into_no_null_iter()
        .filter(|number| *number < metadata.loss.inherited.event_inherited_end)
        .count();
    let counts = (
        collector::u64_from_usize(frame.height()),
        collector::u64_from_usize(inherited),
    );
    Ok((frame, counts))
}
pub(crate) fn verify_ticks(
    directory: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    manifest: &crate::Manifest,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
    raw_work: Option<&dyn crate::engine::RawTableWork>,
) -> io::Result<Vec<TickSpan>> {
    let frame = tick_frame(directory, manifest, budget, work, raw_work)?;
    verify_ticks_frame(&frame, metadata, manifest)
}
fn tick_frame(
    directory: &PrivateDirectory,
    manifest: &crate::Manifest,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
    raw_work: Option<&dyn crate::engine::RawTableWork>,
) -> io::Result<polars::prelude::DataFrame> {
    work.namespace_attempts += 1;
    work.raw_admissions += 1;
    let table = admit_tick_file(directory.open_file("ticks.parquet")?, budget, raw_work)?;
    let count = manifest
        .tables
        .iter()
        .find(|table| table.name == "ticks")
        .ok_or_else(invalid)?
        .rows;
    if table.row_count() != count {
        return Err(invalid());
    }
    tick_columns(&table, raw_work)
}
fn admit_tick_file(
    file: File,
    budget: &Arc<ByteBudget>,
    work: Option<&dyn crate::engine::RawTableWork>,
) -> io::Result<AdmittedTable> {
    let limits = DecodeLimits {
        compressed_bytes: 8 * 1024 * 1024,
        expanded_bytes: 8 * 1024 * 1024,
        rows: 61,
        row_groups: 1,
        columns: crate::schema::TICK_COLUMNS_MAX,
    };
    let table = AdmittedTable::from_file_limited_counted(file, budget, &limits, work)
        .map_err(admission_error)?;
    validate_tick_schema(&table)?;
    Ok(table)
}
fn tick_columns(
    table: &AdmittedTable,
    work: Option<&dyn crate::engine::RawTableWork>,
) -> io::Result<polars::prelude::DataFrame> {
    table
        .read_metadata_counted(
            &[
                "publication",
                "run_tick",
                "continuous_ns",
                "wall_ns",
                "awake_ns",
                "events_first",
                "events_end",
            ],
            Some(61),
            work,
        )
        .map_err(read_error)
}
/// Footer admission can prove that a non-tick original contains no physical rows.
pub(crate) fn zero_table_file(
    file: File,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<bool> {
    work.raw_admissions += 1;
    let table = AdmittedTable::from_file_budgeted(file, budget).map_err(admission_error)?;
    Ok(table.row_count() == 0)
}
/// Proves absence of tick rows using the same physical/schema admission as canonical reads.
pub(crate) fn zero_tick_file(
    file: File,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<bool> {
    work.raw_admissions += 1;
    let table = admit_tick_file(file, budget, None)?;
    if table.row_count() != 0 {
        return Ok(false);
    }
    let frame = tick_columns(&table, None)?;
    if frame.height() != 0 {
        return Err(invalid());
    }
    Ok(true)
}

fn verify_ticks_frame(
    frame: &polars::prelude::DataFrame,
    metadata: &CanonicalMetadata,
    manifest: &crate::Manifest,
) -> io::Result<Vec<TickSpan>> {
    let publications = tick_column(frame, "publication")?;
    if publications.null_count() != 0 || publications.get(0) != Some(metadata.canonical_origin) {
        return Err(invalid());
    }
    let run_ticks = tick_column(frame, "run_tick")?;
    let continuous = tick_column(frame, "continuous_ns")?;
    let wall = tick_column(frame, "wall_ns")?;
    let awake = tick_column(frame, "awake_ns")?;
    let firsts = tick_column(frame, "events_first")?;
    let ends = tick_column(frame, "events_end")?;
    let mut ticks = Vec::with_capacity(61);
    let mut previous_end = manifest.events_first;
    if tick_bounds(wall) != manifest.wall_ns
        || tick_bounds(continuous) != manifest.continuous_ns
        || tick_bounds(awake) != manifest.awake_ns
    {
        return Err(invalid());
    }
    let mut previous = None;
    for (index, publication) in publications.into_no_null_iter().enumerate() {
        if continuous.get(index).is_some() != wall.get(index).is_some()
            || continuous.get(index).is_some() != awake.get(index).is_some()
            || metadata.timing().window(
                run_ticks.get(index).ok_or_else(invalid)?,
                continuous.get(index),
            )? != metadata.window
        {
            return Err(invalid());
        }
        if publication < metadata.loss.inherited.publication_inherited_end
            || publication >= metadata.original_end
            || previous.is_some_and(|old| publication <= old)
        {
            return Err(invalid());
        }
        let first = firsts.get(index).ok_or_else(invalid)?;
        let end = ends.get(index).ok_or_else(invalid)?;
        if first < previous_end || first > end || end > manifest.events_end {
            return Err(invalid());
        }
        ticks.push(TickSpan {
            publication,
            first,
            end,
        });
        previous_end = end;
        previous = Some(publication);
    }
    if previous.and_then(|last| last.checked_add(1)) != Some(metadata.original_end) {
        return Err(invalid());
    }
    if previous_end != manifest.events_end {
        return Err(invalid());
    }
    Ok(ticks)
}

fn tick_column<'frame>(
    frame: &'frame polars::prelude::DataFrame,
    name: &str,
) -> io::Result<&'frame polars::prelude::UInt64Chunked> {
    frame
        .column(name)
        .map_err(crate::metadata::invalid)?
        .u64()
        .map_err(crate::metadata::invalid)
}

fn tick_bounds(column: &polars::prelude::UInt64Chunked) -> Option<crate::TimeBounds> {
    crate::metadata::clock_bounds((0..column.len()).map(|index| column.get(index)))
}
fn validate_tick_schema(table: &AdmittedTable) -> io::Result<()> {
    for group in std::iter::once(model::catalog::TICKS.columns)
        .chain(model::catalog::TICKS.groups.iter().copied())
    {
        let mut present = 0;
        for column in group {
            if let Some((_, kind)) = table.fields().find(|(name, _)| *name == column.name) {
                if crate::query::storage_type(&kind).map_err(crate::metadata::invalid)?
                    != crate::schema::storage_name(column.kind)
                {
                    return Err(invalid());
                }
                present += 1;
            }
        }
        if (std::ptr::eq(group, model::catalog::TICKS.columns) || present != 0)
            && present != group.len()
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(crate) fn source_error(error: crate::Error) -> io::Error {
    match error {
        crate::Error::Io(error) => error,
        crate::Error::Bound("terminal read budget") => io::Error::from(io::ErrorKind::OutOfMemory),
        error => crate::metadata::invalid(error),
    }
}
pub(crate) fn admission_error(error: AdmissionRefusal) -> io::Error {
    match error {
        AdmissionRefusal::Budget => io::Error::from(io::ErrorKind::OutOfMemory),
        AdmissionRefusal::Bound(message) => crate::metadata::invalid(message),
        AdmissionRefusal::Io { kind, raw_os_error } => {
            raw_os_error.map_or_else(|| io::Error::from(kind), io::Error::from_raw_os_error)
        }
    }
}
pub(crate) fn read_error(error: AdmittedReadRefusal) -> io::Error {
    match error {
        AdmittedReadRefusal::Admission(error) => admission_error(error),
        AdmittedReadRefusal::Decode(error) => source_error(error),
    }
}

/// Reads only the already validated bounded tick identity/clock columns for publication.
pub(crate) fn observation_keys(
    directory: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
    raw_work: Option<&dyn crate::engine::RawTableWork>,
) -> io::Result<Vec<(Namespace, CanonicalKey)>> {
    let Body::Ordinary(manifest) = &metadata.body else {
        return Ok(Vec::new());
    };
    let frame = tick_frame(directory, manifest, budget, work, raw_work)?;
    verify_ticks_frame(&frame, metadata, manifest)?;
    let publications = tick_column(&frame, "publication")?;
    let clocks = tick_column(&frame, "wall_ns")?;
    let mut keys = Vec::with_capacity(61);
    for (index, publication) in publications.into_no_null_iter().enumerate() {
        let mut bytes = [0; 60];
        let (namespace, offset) = if let Some(clock) = clocks.get(index) {
            let hour = u32::try_from(clock / 3_600_000_000_000).map_err(|_| invalid())?;
            bytes[..4].copy_from_slice(&hour.to_be_bytes());
            bytes[4..12].copy_from_slice(&(clock % 3_600_000_000_000).to_be_bytes());
            (Namespace::Known, 12)
        } else {
            (Namespace::Unknown, 0)
        };
        let source = CanonicalKey::source(Namespace::Ordinary, &metadata.run, publication)?;
        bytes[offset..offset + 40].copy_from_slice(&source.bytes[..40]);
        keys.push((
            namespace,
            CanonicalKey::from_bytes(namespace, &bytes[..namespace.width()])?,
        ));
    }
    Ok(keys)
}
