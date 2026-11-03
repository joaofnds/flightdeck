//! Read-only census of one registered original; no salvage or publication authority.
use super::attempt::{RegisteredAttempt, TABLE_FILES, batch_number};
use crate::event_table::index::Index;
use crate::resources::{ByteBudget, ByteReservation};
use crate::transaction::AttemptOperation;
use crate::{Error, Result, WindowIdentity};
use collector::recording::private::{
    DirectoryIdentity, DirectoryWork, PrivateDirectory, PrivateNames,
};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

#[path = "original_census.rs"]
pub(super) mod census;

const BATCHES: usize = 4096;
const PASS_POSITIONS: usize = 128 - 16;
const POSITIONS: usize = TABLE_FILES.len() + 2 + BATCHES;
const SUMMARY_BYTES: usize = BATCHES * size_of::<BatchFact>() + 16 * 1024;
const _: () = {
    assert!(BATCHES == crate::event_recovery::batch::BATCHES_MAX);
    assert!(SUMMARY_BYTES < 1024 * 1024);
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginalPlacement {
    Pending,
    Ready,
}

/// Fixed codec1 positions; metadata follows the eleven ordinary table files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum OriginalFile {
    Ticks,
    Memory,
    Cpus,
    Interfaces,
    GpuStates,
    CoreStates,
    Fans,
    Processes,
    Coalitions,
    AppRanks,
    AppParts,
    Metadata,
}
impl OriginalFile {
    const fn position(self) -> usize {
        match self {
            Self::Ticks => 0,
            Self::Memory => 1,
            Self::Cpus => 2,
            Self::Interfaces => 3,
            Self::GpuStates => 4,
            Self::CoreStates => 5,
            Self::Fans => 6,
            Self::Processes => 7,
            Self::Coalitions => 8,
            Self::AppRanks => 9,
            Self::AppParts => 10,
            Self::Metadata => 11,
        }
    }
}
const _: () = assert!(OriginalFile::Metadata.position() == TABLE_FILES.len());

/// File identity includes size/change timestamps so a same-inode rewrite is not silently adopted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl FileIdentity {
    pub(super) fn read(file: &File) -> io::Result<Self> {
        let stat = file.metadata()?;
        Ok(Self {
            device: stat.dev(),
            inode: stat.ino(),
            bytes: stat.len(),
            modified: (stat.mtime(), stat.mtime_nsec()),
            changed: (stat.ctime(), stat.ctime_nsec()),
        })
    }
    pub(crate) fn census_encode(&self, bytes: &mut [u8; 56]) -> io::Result<()> {
        if !(0..1_000_000_000).contains(&self.modified.1)
            || !(0..1_000_000_000).contains(&self.changed.1)
        {
            return Err(invalid());
        }
        for (index, value) in [self.device, self.inode, self.bytes].iter().enumerate() {
            bytes[index * 8..index * 8 + 8].copy_from_slice(&value.to_be_bytes());
        }
        for (index, value) in [
            self.modified.0,
            self.modified.1,
            self.changed.0,
            self.changed.1,
        ]
        .iter()
        .enumerate()
        {
            bytes[24 + index * 8..32 + index * 8].copy_from_slice(&value.to_be_bytes());
        }
        Ok(())
    }
    pub(crate) fn census_decode(bytes: &[u8; 56]) -> io::Result<Self> {
        let word = |index: usize| -> io::Result<u64> {
            Ok(u64::from_be_bytes(
                bytes[index * 8..index * 8 + 8]
                    .try_into()
                    .map_err(|_| invalid())?,
            ))
        };
        let signed = |index: usize| -> io::Result<i64> {
            Ok(i64::from_be_bytes(
                bytes[index * 8..index * 8 + 8]
                    .try_into()
                    .map_err(|_| invalid())?,
            ))
        };
        let identity = Self {
            device: word(0)?,
            inode: word(1)?,
            bytes: word(2)?,
            modified: (signed(3)?, signed(4)?),
            changed: (signed(5)?, signed(6)?),
        };
        let mut expected = [0; 56];
        identity.census_encode(&mut expected)?;
        if expected != *bytes {
            return Err(invalid());
        }
        Ok(identity)
    }
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchFact {
    Unprobed,
    Missing,
    Malformed {
        file: FileIdentity,
        framed: bool,
    },
    Complete {
        file: FileIdentity,
        first: u64,
        end: u64,
        rows: u64,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexFact {
    Unprobed,
    Missing,
    Malformed {
        file: FileIdentity,
    },
    Valid {
        file: FileIdentity,
        first: u64,
        end: u64,
        rows: u64,
        batches: u32,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryStep {
    Continue,
    Enumerated,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct InventoryWork {
    pub positions: u64,
    pub open_attempts: u64,
    pub reopens: u64,
    pub raw_admissions: u64,
    pub decoded_bytes: u64,
    pub index_bytes: u64,
    pub raw_file_clones: u64,
    pub directory: DirectoryWork,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NamesPhase {
    Original,
    Events,
    Done,
}

/// Borrows the retained Store registration and reserves summary/index/one-batch storage.
/// Enumeration grants no append, exact-loss, endpoint-completion or cleanup permission.
///
/// ```compile_fail
/// use std::sync::Arc;
/// use persistence::{RegisteredAttempt, resources::ByteBudget};
/// fn append_during_census(attempt: &mut RegisteredAttempt<'_>, budget: &Arc<ByteBudget>) {
///     let mut census = attempt.original_inventory(budget).unwrap();
///     let file = attempt.create_event_batch(1).unwrap();
///     census.step().unwrap();
/// }
/// ```
pub struct OriginalInventory<'attempt, 'store> {
    attempt: InventoryOwner<'attempt, 'store>,
    names: Option<PrivateNames>,
    names_phase: NamesPhase,
    name_count: usize,
    dots: usize,

    events: Option<PrivateDirectory>,
    original_identity: DirectoryIdentity,
    events_identity: Option<DirectoryIdentity>,
    next: usize,
    batches: Vec<BatchFact>,
    files: [Option<FileIdentity>; 12],
    index: IndexFact,
    index_data: Option<Index>,
    rows: u64,
    maximum_end: u64,
    overlaps: bool,
    unordered: bool,
    index_conflict: bool,
    previous_first: u64,
    work: InventoryWork,
    budget: Arc<ByteBudget>,
    _reservation: ByteReservation,
}
pub(super) struct NormalInventory {
    census_header: Option<census::CensusHeader>,
    cleanup_original_inode: (u64, u64),
    cleanup_events_inode: Option<(u64, u64)>,
    files: [Option<FileIdentity>; 12],
    batches: Vec<BatchFact>,
    index: IndexFact,
    original_identity: Option<DirectoryIdentity>,
    events_identity: Option<DirectoryIdentity>,
    _reservation: ByteReservation,
}
impl NormalInventory {
    pub(super) fn cleanup_directories(
        &self,
        original: &PrivateDirectory,
        events: &PrivateDirectory,
    ) -> io::Result<()> {
        if original.device_inode()? != self.cleanup_original_inode
            || Some(events.device_inode()?) != self.cleanup_events_inode
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn cleanup_batch(&self, ordinal: usize, file: &mut File) -> io::Result<bool> {
        if file.metadata()?.nlink() != 1 {
            return Err(invalid());
        }
        let identity = FileIdentity::read(file)?;
        let expected = match self.batches.get(ordinal) {
            Some(BatchFact::Complete { file, .. } | BatchFact::Malformed { file, .. }) => *file,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unpreserved/conflicting original census identity",
                ));
            }
        };
        if identity != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unpreserved/conflicting original census identity",
            ));
        }
        let disposable = matches!(
            self.batches[ordinal],
            BatchFact::Malformed { framed: false, .. }
        );
        if disposable && parquet_framing(file, identity.bytes)? {
            return Err(invalid());
        }
        if FileIdentity::read(file)? != identity {
            return Err(invalid());
        }
        Ok(disposable)
    }
    pub(super) fn cleanup_index(&self, file: &File) -> io::Result<bool> {
        if file.metadata()?.nlink() != 1 {
            return Err(invalid());
        }
        let (expected, malformed) = match self.index {
            IndexFact::Valid { file, .. } => (file, false),
            IndexFact::Malformed { file } => (file, true),
            _ => return Err(invalid()),
        };
        if FileIdentity::read(file)? != expected {
            return Err(invalid());
        }
        Ok(malformed)
    }
    pub(super) fn cleanup_absent(&self, ordinal: usize) -> io::Result<()> {
        if self.batches.get(ordinal) != Some(&BatchFact::Missing) {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn absent(&self, name: &str) -> io::Result<bool> {
        let position = TABLE_FILES
            .iter()
            .position(|held| *held == name)
            .ok_or_else(invalid)?;
        Ok(self.files[position].is_none())
    }

    pub(super) fn file(&self, name: &str, file: &File) -> io::Result<()> {
        let position = if name == "metadata" {
            11
        } else {
            TABLE_FILES
                .iter()
                .position(|held| *held == name)
                .ok_or_else(invalid)?
        };
        if self.files[position] != Some(FileIdentity::read(file)?) {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn batch(&self, ordinal: usize, file: &File) -> io::Result<()> {
        let Some(BatchFact::Complete { file: expected, .. }) = self.batches.get(ordinal) else {
            return Err(invalid());
        };
        if *expected != FileIdentity::read(file)? {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn index(&self, file: &File) -> io::Result<()> {
        let IndexFact::Valid { file: expected, .. } = self.index else {
            return Err(invalid());
        };
        if expected != FileIdentity::read(file)? {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn directories(
        &self,
        directory: &PrivateDirectory,
        events: Option<&PrivateDirectory>,
    ) -> io::Result<()> {
        if Some(directory.identity()?) != self.original_identity
            || events.map(PrivateDirectory::identity).transpose()? != self.events_identity
        {
            return Err(invalid());
        }
        Ok(())
    }
}
enum InventoryOwner<'attempt, 'store> {
    Borrowed(&'attempt RegisteredAttempt<'store>),
    Owned(Box<RegisteredAttempt<'store>>),
}
impl<'store> std::ops::Deref for InventoryOwner<'_, 'store> {
    type Target = RegisteredAttempt<'store>;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(owner) => owner,
            Self::Owned(owner) => owner,
        }
    }
}
impl<'store> OriginalInventory<'store, 'store> {
    pub(super) fn owned_counted(
        mut owner: RegisteredAttempt<'store>,
        budget: &Arc<ByteBudget>,
        startup: &mut InventoryWork,
    ) -> io::Result<Self> {
        let OriginalInventory {
            attempt: _,
            names,
            names_phase,
            name_count,
            dots,
            events,
            original_identity,
            events_identity,
            next,
            batches,
            files,
            index,
            index_data,
            rows,
            maximum_end,
            overlaps,
            unordered,
            index_conflict,
            previous_first,
            work,
            budget,
            _reservation: reservation,
        } = owner.original_inventory_counted(budget, startup)?;
        Ok(Self {
            attempt: InventoryOwner::Owned(Box::new(owner)),
            names,
            names_phase,
            name_count,
            dots,
            events,
            original_identity,
            events_identity,
            next,
            batches,
            files,
            index,
            index_data,
            rows,
            maximum_end,
            overlaps,
            unordered,
            index_conflict,
            previous_first,
            work,
            budget,
            _reservation: reservation,
        })
    }
    pub(super) fn allow_candidates(&mut self) -> io::Result<()> {
        let InventoryOwner::Owned(owner) = &mut self.attempt else {
            return Err(invalid());
        };
        owner.candidates = true;
        Ok(())
    }
    pub(super) fn owner(&self) -> &RegisteredAttempt<'store> {
        &self.attempt
    }
    pub(super) fn into_normal(self) -> io::Result<(RegisteredAttempt<'store>, NormalInventory)> {
        let cleanup_original_inode = self.attempt.original_directory().device_inode()?;
        let cleanup_events_inode = self
            .events
            .as_ref()
            .map(PrivateDirectory::device_inode)
            .transpose()?;
        let Self {
            attempt,
            files,
            batches,
            index,
            index_data,
            original_identity,
            events_identity,
            _reservation: mut reservation,
            ..
        } = self;
        drop(index_data);
        reservation.shrink_to(SUMMARY_BYTES);
        let InventoryOwner::Owned(owner) = attempt else {
            return Err(invalid());
        };
        Ok((
            *owner,
            NormalInventory {
                census_header: None,
                cleanup_original_inode,
                cleanup_events_inode,
                files,
                batches,
                index,
                original_identity: Some(original_identity),
                events_identity,
                _reservation: reservation,
            },
        ))
    }
}
impl<'store> RegisteredAttempt<'store> {
    pub fn original_inventory<'attempt>(
        &'attempt mut self,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<OriginalInventory<'attempt, 'store>> {
        self.original_inventory_counted(budget, &mut InventoryWork::default())
    }

    pub fn original_inventory_counted<'attempt>(
        &'attempt mut self,
        budget: &Arc<ByteBudget>,
        work: &mut InventoryWork,
    ) -> io::Result<OriginalInventory<'attempt, 'store>> {
        let reservation = budget
            .reserve(
                SUMMARY_BYTES
                    + crate::event_table::index::WORKING_BYTES
                    + crate::event_recovery::batch::WORKING_BYTES,
            )
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        let before = work.open_attempts;
        let result = self.validate_original_placement(&mut work.open_attempts, &mut work.directory);
        work.reopens += work.open_attempts - before;
        result?;
        let events = if matches!(self.input().operation, AttemptOperation::Ordinary { .. }) {
            work.open_attempts += 1;
            optional_directory(self.original_directory(), "events")?
        } else {
            None
        };
        let names = self.original_directory().names(&mut work.directory)?;
        Ok(OriginalInventory {
            attempt: InventoryOwner::Borrowed(self),
            names: Some(names),
            names_phase: NamesPhase::Original,
            name_count: 0,
            dots: 0,

            original_identity: self.original_directory().identity()?,
            events_identity: events
                .as_ref()
                .map(PrivateDirectory::identity)
                .transpose()?,
            events,
            next: 0,
            batches: vec![BatchFact::Unprobed; BATCHES],
            files: [None; 12],
            index: IndexFact::Unprobed,
            index_data: None,
            rows: 0,
            maximum_end: 0,
            overlaps: false,
            unordered: false,
            index_conflict: false,
            previous_first: 0,
            work: *work,
            budget: Arc::clone(budget),
            _reservation: reservation,
        })
    }
}
impl OriginalInventory<'_, '_> {
    #[must_use]
    pub fn work(&self) -> InventoryWork {
        self.work
    }
    #[must_use]
    pub fn placement(&self) -> OriginalPlacement {
        self.attempt.original_placement()
    }
    #[must_use]
    pub fn index(&self) -> IndexFact {
        self.index
    }
    #[must_use]
    pub fn batches(&self) -> &[BatchFact] {
        &self.batches
    }
    #[must_use]
    pub fn file(&self, kind: OriginalFile) -> Option<FileIdentity> {
        self.files[kind.position()]
    }
    /// Actual index extents, never widened to the registered inherited prefix.
    #[must_use]
    pub fn index_conflicts(&self) -> bool {
        self.index_conflict
    }
    #[must_use]
    pub fn index_batch(&self, ordinal: u32) -> Option<(u64, u64, u64)> {
        self.index_data
            .as_ref()?
            .batches
            .get(usize::try_from(ordinal).ok()?)
            .map(|batch| (batch.first, batch.end, batch.rows))
    }
    #[must_use]
    pub fn physical_rows(&self) -> u64 {
        self.rows
    }
    #[must_use]
    pub fn has_overlaps(&self) -> bool {
        self.overlaps
    }
    #[must_use]
    pub fn has_unordered_extents(&self) -> bool {
        self.unordered
    }
    #[must_use]
    pub fn exceeds_logical_limit(&self) -> bool {
        self.rows > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
    }
    #[must_use]
    pub fn next_position(&self) -> usize {
        self.next
    }
    #[must_use]
    pub fn enumerated(&self) -> bool {
        self.names_phase == NamesPhase::Done && self.next == POSITIONS
    }
    #[must_use]
    pub fn registered_event_start(&self) -> u64 {
        self.attempt.input().losses.event_start
    }

    /// Leaves room for fixed framing: at most112 native names OR deterministic positions.
    pub fn step(&mut self) -> Result<InventoryStep> {
        self.validate_directories()?;
        if self.names_phase != NamesPhase::Done {
            let result = self.step_names();
            if result.is_err() {
                self.names = None;
                self.name_count = 0;
                self.dots = 0;
            }
            result?;
            return Ok(InventoryStep::Continue);
        }
        for _ in 0..PASS_POSITIONS {
            if self.next == POSITIONS {
                self.validate_directories()?;
                return Ok(InventoryStep::Enumerated);
            }
            self.work.positions += 1;
            self.probe_position()?;
            self.next += 1;
        }
        Ok(InventoryStep::Continue)
    }

    pub(super) fn normal_batches(
        &self,
        metadata: &crate::canonical::CanonicalMetadata,
    ) -> io::Result<Vec<crate::event_recovery::Batch>> {
        match &metadata.body {
            crate::canonical::metadata::Body::Ordinary(_) => Ok(self
                .index_data
                .as_ref()
                .ok_or_else(invalid)?
                .batches
                .clone()),
            crate::canonical::metadata::Body::Events(manifest) => Ok(manifest.batches.clone()),
            crate::canonical::metadata::Body::LossOnly => Err(invalid()),
        }
    }
    pub(super) fn metadata_file(&self, file: &File) -> io::Result<()> {
        if self.files[11] != Some(FileIdentity::read(file)?) {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn validate_normal(
        &self,
        metadata: &crate::canonical::CanonicalMetadata,
    ) -> io::Result<()> {
        use crate::canonical::metadata::Body;
        if !self.enumerated() || self.overlaps || self.unordered || self.exceeds_logical_limit() {
            return Err(invalid());
        }
        let batches: &[crate::event_recovery::Batch] = match &metadata.body {
            Body::Ordinary(manifest) => {
                if self.index_conflict {
                    return Err(invalid());
                }
                let index = self.index_data.as_ref().ok_or_else(invalid)?;
                if index.identity != manifest.identity
                    || index.first != manifest.events_first
                    || index.end != manifest.events_end
                    || manifest.event_batches
                        != Some(u32::try_from(index.batches.len()).map_err(|_| invalid())?)
                    || manifest
                        .tables
                        .iter()
                        .find(|table| table.name == "events")
                        .map(|table| table.rows)
                        != Some(index.rows)
                {
                    return Err(invalid());
                }
                for (position, name) in TABLE_FILES.iter().enumerate() {
                    let stem = name.strip_suffix(".parquet").ok_or_else(invalid)?;
                    let present = self.files[position].is_some();
                    let declared = manifest.tables.iter().any(|table| table.name == stem);
                    if (present && !declared)
                        || (!present && declared && !crate::schema::DERIVED_TABLES.contains(&stem))
                    {
                        return Err(invalid());
                    }
                }
                &index.batches
            }
            Body::Events(manifest)
                if metadata.kind == crate::transaction::CanonicalKind::Terminal =>
            {
                &manifest.batches
            }
            _ => return Err(io::Error::from(io::ErrorKind::Unsupported)),
        };
        for (ordinal, fact) in self.batches.iter().enumerate() {
            match (batches.get(ordinal), fact) {
                (None, BatchFact::Missing) => {}
                (
                    Some(expected),
                    BatchFact::Complete {
                        first, end, rows, ..
                    },
                ) if expected.first == *first && expected.end == *end && expected.rows == *rows => {
                }
                _ => return Err(invalid()),
            }
        }
        Ok(())
    }
    fn validate_directories(&mut self) -> io::Result<()> {
        if self.attempt.original_directory().identity()? != self.original_identity {
            return Err(invalid());
        }
        if self
            .events
            .as_ref()
            .map(PrivateDirectory::identity)
            .transpose()?
            != self.events_identity
        {
            return Err(invalid());
        }
        let before = self.work.open_attempts;
        let result = self
            .attempt
            .validate_original_placement(&mut self.work.open_attempts, &mut self.work.directory);
        self.work.reopens += self.work.open_attempts - before;
        result?;
        if matches!(
            self.attempt.input().operation,
            AttemptOperation::Ordinary { .. }
        ) {
            self.work.open_attempts += 1;
            self.work.reopens += 1;
            let current = optional_directory(self.attempt.original_directory(), "events")?;
            match (&self.events, current) {
                (None, None) => {}
                (Some(prior), Some(current)) if prior.same_inode(&current)? => {}
                _ => return Err(invalid()),
            }
        }
        Ok(())
    }

    fn step_names_open(&mut self) -> io::Result<()> {
        if self.names.is_none() {
            let directory = match self.names_phase {
                NamesPhase::Original => self.attempt.original_directory(),
                NamesPhase::Events => self.events.as_ref().ok_or_else(invalid)?,
                NamesPhase::Done => return Err(invalid()),
            };
            self.names = Some(directory.names(&mut self.work.directory)?);
        }
        Ok(())
    }
    fn step_names(&mut self) -> io::Result<()> {
        self.step_names_open()?;
        for _ in 0..PASS_POSITIONS {
            let Some(names) = self.names.as_mut() else {
                return Err(invalid());
            };
            let ordinary = matches!(
                self.attempt.input().operation,
                AttemptOperation::Ordinary { .. }
            );
            let limit = if self.names_phase == NamesPhase::Events || !ordinary {
                BATCHES + 1
            } else {
                TABLE_FILES.len() + 2
            };
            let mut dot = false;
            let found = names.step(&mut self.work.directory, |name| {
                dot = name == "." || name == "..";
                if dot {
                    return if self.dots < 2 {
                        Ok(())
                    } else {
                        Err(invalid())
                    };
                }
                if self.name_count >= limit {
                    return Err(invalid());
                }
                let valid = if self.names_phase == NamesPhase::Events || !ordinary {
                    batch_number(name).is_ok()
                        || name == if ordinary { "index" } else { "metadata" }
                } else {
                    TABLE_FILES.contains(&name) || name == "metadata" || name == "events"
                };
                if valid { Ok(()) } else { Err(invalid()) }
            })?;
            if !found {
                self.names = None;
                if self.names_phase == NamesPhase::Original && ordinary && self.events.is_some() {
                    self.names = Some(
                        self.events
                            .as_ref()
                            .ok_or_else(invalid)?
                            .names(&mut self.work.directory)?,
                    );
                    self.names_phase = NamesPhase::Events;
                    self.name_count = 0;
                    self.dots = 0;
                } else {
                    self.names_phase = NamesPhase::Done;
                }
                return Ok(());
            }
            if dot {
                self.dots += 1;
            } else {
                self.name_count += 1;
            }
        }
        Ok(())
    }

    fn probe_position(&mut self) -> Result<()> {
        if self.next < TABLE_FILES.len() + 1 {
            return self.probe_file();
        }
        if self.next == TABLE_FILES.len() + 1 {
            return self.probe_index();
        }
        let ordinal = self.next - TABLE_FILES.len() - 2;
        self.probe_batch(ordinal)?;
        if let Some(index) = &self.index_data {
            let matches = match (index.batches.get(ordinal), self.batches[ordinal]) {
                (None, BatchFact::Missing) => true,
                (
                    Some(expected),
                    BatchFact::Complete {
                        first, end, rows, ..
                    },
                ) => expected.first == first && expected.end == end && expected.rows == rows,
                _ => false,
            };
            self.index_conflict |= !matches;
        }
        Ok(())
    }

    fn probe_file(&mut self) -> Result<()> {
        let ordinary = matches!(
            self.attempt.input().operation,
            AttemptOperation::Ordinary { .. }
        );
        if !ordinary && self.next < TABLE_FILES.len() {
            return Ok(());
        }
        let name = TABLE_FILES.get(self.next).copied().unwrap_or("metadata");
        self.work.open_attempts += 1;
        let file = optional_file(self.attempt.original_directory(), name)?;
        if file.is_some() {
            self.work.raw_admissions += 1;
        }
        self.files[self.next] = file.as_ref().map(FileIdentity::read).transpose()?;
        Ok(())
    }

    fn probe_index(&mut self) -> Result<()> {
        let Some(events) = self.events.as_ref() else {
            self.index = IndexFact::Missing;
            return Ok(());
        };
        self.work.open_attempts += 1;
        let Some(mut file) = optional_file(events, "index")? else {
            self.index = IndexFact::Missing;
            return Ok(());
        };
        let identity = FileIdentity::read(&file)?;
        self.work.raw_admissions += 1;
        let window = match self.attempt.input().operation {
            AttemptOperation::Ordinary { window, .. } => window,
            AttemptOperation::Terminal { .. } => return Err(invalid().into()),
        };
        let expected = WindowIdentity {
            run: self.attempt.input().run,
            window,
        };
        let result =
            Index::read_attempt_file(&mut file, &expected, Some(self.attempt.input().captured));
        self.work.index_bytes += file.stream_position()?;
        if FileIdentity::read(&file)? != identity {
            return Err(invalid().into());
        }
        match result {
            Ok(index) => {
                if index.first < self.registered_event_start() {
                    return Err(invalid().into());
                }
                self.index = IndexFact::Valid {
                    file: identity,
                    first: index.first,
                    end: index.end,
                    rows: index.rows,
                    batches: u32::try_from(index.batches.len()).map_err(|_| invalid())?,
                };
                self.index_data = Some(index);
            }
            Err(Error::Bound(_)) => self.index = IndexFact::Malformed { file: identity },
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn probe_batch(&mut self, ordinal: usize) -> Result<()> {
        let ordinary = matches!(
            self.attempt.input().operation,
            AttemptOperation::Ordinary { .. }
        );
        let directory = if ordinary {
            self.events.as_ref()
        } else {
            Some(self.attempt.original_directory())
        };
        let Some(directory) = directory else {
            self.batches[ordinal] = BatchFact::Missing;
            return Ok(());
        };
        self.work.open_attempts += 1;
        let name = crate::event_recovery::batch_name(ordinal);
        let Some(mut file) = optional_file(directory, &name)? else {
            self.batches[ordinal] = BatchFact::Missing;
            return Ok(());
        };
        self.work.raw_admissions += 1;
        let identity = FileIdentity::read(&file)?;
        let framed = parquet_framing(&mut file, identity.bytes)?;
        if FileIdentity::read(&file)? != identity {
            return Err(invalid().into());
        }
        if !framed {
            self.batches[ordinal] = BatchFact::Malformed {
                file: identity,
                framed: false,
            };
            return Ok(());
        }
        self.work.raw_file_clones += 1;
        let result = crate::event_recovery::read::read_original_batch_file(
            file.try_clone()?,
            self.attempt.input().captured,
            ordinary,
            &self.budget,
            &mut self.work.decoded_bytes,
        );
        if FileIdentity::read(&file)? != identity {
            return Err(invalid().into());
        }
        match result {
            Ok(batch) => {
                let fact = self.probe_batch_complete(identity, &batch)?;
                self.overlaps |= batch.first < self.maximum_end;
                self.unordered |= batch.first <= self.previous_first;
                self.maximum_end = self.maximum_end.max(batch.end);
                self.previous_first = batch.first;
                self.rows += batch.rows;
                self.batches[ordinal] = fact;
            }
            Err(Error::Bound("changed parquet capture source")) => return Err(invalid().into()),
            Err(Error::Bound(_)) => {
                self.batches[ordinal] = BatchFact::Malformed {
                    file: identity,
                    framed: true,
                }
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn probe_batch_complete(
        &self,
        identity: FileIdentity,
        batch: &crate::event_recovery::Batch,
    ) -> Result<BatchFact> {
        if batch.first < self.registered_event_start() {
            return Err(invalid().into());
        }
        if let AttemptOperation::Terminal { event_end } = self.attempt.input().operation {
            if batch.end > event_end {
                return Err(invalid().into());
            }
        }
        Ok(BatchFact::Complete {
            file: identity,
            first: batch.first,
            end: batch.end,
            rows: batch.rows,
        })
    }

    /// Reopens a fixed ordinary table/metadata position only after census and identity checking.
    pub fn reopen_file(&mut self, kind: OriginalFile) -> io::Result<File> {
        if !self.enumerated() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let identity = self.file(kind).ok_or_else(invalid)?;
        self.validate_directories()?;
        self.work.open_attempts += 1;
        self.work.reopens += 1;
        let name = TABLE_FILES
            .get(kind.position())
            .copied()
            .unwrap_or("metadata");
        let file = self.attempt.original_directory().open_file(name)?;
        self.work.raw_admissions += 1;
        if FileIdentity::read(&file)? != identity {
            return Err(invalid());
        }
        Ok(file)
    }

    /// Reopens only the retained original and verifies the admitted file identity.
    pub fn reopen_batch(&mut self, ordinal: u32) -> io::Result<File> {
        if !self.enumerated() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let ordinal = usize::try_from(ordinal).map_err(|_| invalid())?;
        let Some(BatchFact::Complete { file: identity, .. }) = self.batches.get(ordinal).copied()
        else {
            return Err(invalid());
        };
        self.validate_directories()?;
        let directory = self
            .events
            .as_ref()
            .unwrap_or(self.attempt.original_directory());
        self.work.open_attempts += 1;
        self.work.reopens += 1;
        let file = directory.open_file(&crate::event_recovery::batch_name(ordinal))?;
        self.work.raw_admissions += 1;
        if FileIdentity::read(&file)? != identity {
            return Err(invalid());
        }
        Ok(file)
    }
}

pub(super) fn select_original(
    object: &PrivateDirectory,
) -> io::Result<Option<(PrivateDirectory, OriginalPlacement)>> {
    select_original_counted(object, &mut 0)
}
pub(super) fn select_original_counted(
    object: &PrivateDirectory,
    opens: &mut u64,
) -> io::Result<Option<(PrivateDirectory, OriginalPlacement)>> {
    select_original_mode(object, opens, false)
}
pub(super) fn select_original_mode(
    object: &PrivateDirectory,
    opens: &mut u64,
    candidates: bool,
) -> io::Result<Option<(PrivateDirectory, OriginalPlacement)>> {
    for name in ["fragment-0", "fragment-1", "loss"] {
        *opens += 1;
        if optional_directory(object, name)?.is_some() && (!candidates || name == "loss") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "registered candidate/loss placements require fixed-plan reconciliation",
            ));
        }
    }
    *opens += 1;
    let pending = optional_directory(object, "pending")?;
    let identity = pending
        .as_ref()
        .map(PrivateDirectory::identity)
        .transpose()?;
    drop(pending);
    *opens += 1;
    let ready = optional_directory(object, "ready")?;
    match (identity, ready) {
        (Some(_), Some(_)) => Err(invalid()),
        (Some(identity), None) => {
            *opens += 1;
            let pending = object.open_directory("pending")?;
            if pending.identity()? != identity {
                return Err(invalid());
            }
            Ok(Some((pending, OriginalPlacement::Pending)))
        }
        (None, Some(directory)) => Ok(Some((directory, OriginalPlacement::Ready))),
        (None, None) => Ok(None),
    }
}
fn optional_directory(
    parent: &PrivateDirectory,
    name: &str,
) -> io::Result<Option<PrivateDirectory>> {
    match parent.open_directory(name) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
fn optional_file(parent: &PrivateDirectory, name: &str) -> io::Result<Option<File>> {
    match parent.open_file(name) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
pub(super) fn parquet_framing(file: &mut File, bytes: u64) -> io::Result<bool> {
    if bytes < 12 {
        return Ok(false);
    }
    let mut first = [0; 4];
    let mut last = [0; 4];
    file.read_exact(&mut first)?;
    file.seek(SeekFrom::End(-4))?;
    file.read_exact(&mut last)?;
    file.rewind()?;
    Ok(first == *b"PAR1" && last == *b"PAR1")
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
