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
    let mut inherited = 0_u64;
    let mut rows = 0_u64;
    match &metadata.body {
        Body::LossOnly => return Ok(()),
        Body::Ordinary(manifest) => {
            verify_ticks(directory, metadata, manifest, budget, work)?;
            work.namespace_attempts += 1;
            work.node_attempts += 1;
            let events = directory.open_directory("events")?;
            work.namespace_attempts += 1;
            work.raw_admissions += 1;
            let index = crate::event_table::index::Index::read_file(
                &mut events.open_file("index")?,
                manifest,
            )
            .map_err(source_error)?;
            if index.identities != metadata.captured {
                return Err(invalid());
            }
            for (ordinal, batch) in index.batches.iter().enumerate() {
                let counts = verify_batch(&events, metadata, batch, ordinal, true, budget, work)?;
                rows += counts.0;
                inherited += counts.1;
            }
        }
        Body::Events(manifest) => {
            for (ordinal, batch) in manifest.batches.iter().enumerate() {
                let counts = verify_batch(
                    directory,
                    metadata,
                    batch,
                    ordinal,
                    metadata.kind == crate::transaction::CanonicalKind::Fragment,
                    budget,
                    work,
                )?;
                rows += counts.0;
                inherited += counts.1;
            }
        }
    }
    let domain = &metadata.loss.inherited;
    if inherited.checked_add(domain.events) != Some(domain.event_inherited_end - domain.event_start)
        || (rows - inherited).checked_add(metadata.loss.events_new)
            != Some(metadata.loss.event_end - domain.event_inherited_end)
    {
        return Err(invalid());
    }
    Ok(())
}
fn verify_batch(
    directory: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    batch: &crate::event_recovery::Batch,
    ordinal: usize,
    associated: bool,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<(u64, u64)> {
    work.namespace_attempts += 1;
    work.raw_admissions += 1;
    let file = directory.open_file(&crate::event_recovery::batch_name(ordinal))?;
    let frame = crate::event_recovery::read::read_batch_file(
        file,
        batch,
        metadata.captured,
        associated,
        budget,
    )
    .map_err(source_error)?;
    let numbers = frame
        .column("event")
        .map_err(crate::metadata::invalid)?
        .u64()
        .map_err(crate::metadata::invalid)?;
    let inherited = numbers
        .into_no_null_iter()
        .filter(|number| *number < metadata.loss.inherited.event_inherited_end)
        .count();
    Ok((
        collector::u64_from_usize(frame.height()),
        collector::u64_from_usize(inherited),
    ))
}
fn verify_ticks(
    directory: &PrivateDirectory,
    metadata: &CanonicalMetadata,
    manifest: &crate::Manifest,
    budget: &Arc<ByteBudget>,
    work: &mut ReadWork,
) -> io::Result<()> {
    work.namespace_attempts += 1;
    work.raw_admissions += 1;
    let limits = DecodeLimits {
        compressed_bytes: 8 * 1024 * 1024,
        expanded_bytes: 8 * 1024 * 1024,
        rows: 61,
        row_groups: 1,
        columns: crate::schema::TICK_COLUMNS_MAX,
    };
    let table = AdmittedTable::from_file_budgeted_with_limits(
        directory.open_file("ticks.parquet")?,
        budget,
        &limits,
    )
    .map_err(admission_error)?;
    validate_tick_schema(&table)?;
    let count = manifest
        .tables
        .iter()
        .find(|table| table.name == "ticks")
        .ok_or_else(invalid)?
        .rows;
    if table.row_count() != count {
        return Err(invalid());
    }
    let frame = table
        .read_metadata_budgeted(&["publication"], Some(61))
        .map_err(read_error)?;
    let publications = frame
        .column("publication")
        .map_err(crate::metadata::invalid)?
        .u64()
        .map_err(crate::metadata::invalid)?;
    if publications.null_count() != 0 || publications.get(0) != Some(metadata.canonical_origin) {
        return Err(invalid());
    }
    let mut previous = None;
    for publication in publications.into_no_null_iter() {
        if publication < metadata.loss.inherited.publication_inherited_end
            || publication >= metadata.original_end
            || previous.is_some_and(|old| publication <= old)
        {
            return Err(invalid());
        }
        previous = Some(publication);
    }
    if previous.and_then(|last| last.checked_add(1)) != Some(metadata.original_end) {
        return Err(invalid());
    }
    Ok(())
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

fn source_error(error: crate::Error) -> io::Error {
    match error {
        crate::Error::Io(error) => error,
        crate::Error::Bound("terminal read budget") => io::Error::from(io::ErrorKind::OutOfMemory),
        error => crate::metadata::invalid(error),
    }
}
fn admission_error(error: AdmissionRefusal) -> io::Error {
    match error {
        AdmissionRefusal::Budget => io::Error::from(io::ErrorKind::OutOfMemory),
        AdmissionRefusal::Bound(message) => crate::metadata::invalid(message),
        AdmissionRefusal::Io { kind, raw_os_error } => {
            raw_os_error.map_or_else(|| io::Error::from(kind), io::Error::from_raw_os_error)
        }
    }
}
fn read_error(error: AdmittedReadRefusal) -> io::Error {
    match error {
        AdmittedReadRefusal::Admission(error) => admission_error(error),
        AdmittedReadRefusal::Decode(error) => source_error(error),
    }
}
