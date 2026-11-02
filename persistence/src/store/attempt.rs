use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use collector::recording::bootstrap::codec::{self, SlotKind};
use collector::recording::private::{self, FileLock, PrivateDirectory};
use collector::ring::{Found, Identity, PublicationTiming, RingReader};

use super::{StagingPermit, Store};
use crate::transaction::{AttemptInput, AttemptOperation, Predecessor};

/// The persister owner must remain held while the registered capability is usable.
///
/// ```compile_fail
/// use collector::ring::RingReader;
/// use persistence::{Store, transaction::AttemptInput};
/// use std::sync::Arc;
/// use persistence::resources::ByteBudget;
/// fn release_owner(store: Store, input: &AttemptInput, reader: &mut RingReader,
///                  budget: &Arc<ByteBudget>) {
///     let attempt = store.register_attempt(input, reader, budget).unwrap();
///     drop(store);
///     drop(attempt);
/// }
/// ```
///
/// A writable file must not escape the registered capability either.
///
/// ```compile_fail
/// use std::io::Write;
/// use collector::ring::RingReader;
/// use persistence::{Store, transaction::AttemptInput};
/// use std::sync::Arc;
/// use persistence::resources::ByteBudget;
/// fn release_attempt(store: &Store, input: &AttemptInput, reader: &mut RingReader,
///                    budget: &Arc<ByteBudget>) {
///     let attempt = store.register_attempt(input, reader, budget).unwrap();
///     let mut file = attempt.create_file("ticks.parquet").unwrap();
///     drop(attempt);
///     file.write_all(b"unowned").unwrap();
/// }
/// ```
pub struct RegisteredAttempt<'store> {
    transaction: Identity,
    input: AttemptInput,
    directory: PrivateDirectory,
    resumed: bool,
    _permit: StagingPermit,
    owner: &'store FileLock,
    root: &'store PrivateDirectory,
    placement: super::inventory::OriginalPlacement,
}

/// An admitted writable file that retains the registered owner's lifetime.
pub struct RegisteredFile<'owner> {
    file: File,
    _owner: &'owner FileLock,
}

impl std::fmt::Debug for RegisteredFile<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredFile")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

impl Write for RegisteredFile<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl RegisteredAttempt<'_> {
    #[must_use]
    pub fn transaction(&self) -> Identity {
        self.transaction
    }
    #[must_use]
    pub fn input(&self) -> &AttemptInput {
        &self.input
    }
    #[must_use]
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    pub fn open_file(&self, name: &str) -> io::Result<File> {
        self.validate_file_name(name)?;
        self.directory.open_file(name)
    }

    pub fn create_file(&self, name: &str) -> io::Result<RegisteredFile<'_>> {
        self.validate_file_name(name)?;
        if self.resumed {
            return Err(unresolved(
                "occupied raw inventory requires salvage reconciliation",
            ));
        }
        Ok(RegisteredFile {
            file: self.directory.create_file(name)?,
            _owner: self.owner,
        })
    }

    fn events_directory(&self) -> io::Result<PrivateDirectory> {
        if self.resumed {
            return Err(unresolved(
                "occupied event inventory requires salvage reconciliation",
            ));
        }
        if !matches!(self.input.operation, AttemptOperation::Ordinary { .. }) {
            return Err(invalid());
        }
        self.directory.create_directory("events")
    }

    /// Supplies an exclusively created, admitted File to the existing bounded encoder.
    pub fn create_event_batch(&self, ordinal: u32) -> io::Result<RegisteredFile<'_>> {
        if ordinal >= 4096 {
            return Err(invalid());
        }
        let name =
            crate::event_recovery::batch_name(usize::try_from(ordinal).map_err(|_| invalid())?);
        match self.input.operation {
            AttemptOperation::Ordinary { .. } => Ok(RegisteredFile {
                file: self.events_directory()?.create_file(&name)?,
                _owner: self.owner,
            }),
            AttemptOperation::Terminal { .. } => self.create_file(&name),
        }
    }

    /// Creates the fixed ordinary index through the same retained writable capability.
    pub fn create_event_index(&self) -> io::Result<RegisteredFile<'_>> {
        Ok(RegisteredFile {
            file: self.events_directory()?.create_file("index")?,
            _owner: self.owner,
        })
    }

    fn validate_file_name(&self, name: &str) -> io::Result<()> {
        let admitted = match self.input.operation {
            AttemptOperation::Ordinary { .. } => name == "metadata" || TABLE_FILES.contains(&name),
            AttemptOperation::Terminal { .. } => name == "metadata" || batch_number(name).is_ok(),
        };
        if admitted { Ok(()) } else { Err(invalid()) }
    }
}

pub(super) const TABLE_FILES: [&str; 11] = [
    "ticks.parquet",
    "memory.parquet",
    "cpus.parquet",
    "interfaces.parquet",
    "gpu_states.parquet",
    "core_states.parquet",
    "fans.parquet",
    "processes.parquet",
    "coalitions.parquet",
    "app_ranks.parquet",
    "app_parts.parquet",
];

const _: () = assert!(
    crate::schema::TABLES.len() + crate::schema::DERIVED_TABLES.len() == TABLE_FILES.len() + 1
);

impl Store {
    /// Validates source evidence and the canonical frontier before registration.
    pub fn register_attempt(
        &self,
        input: &AttemptInput,
        reader: &mut RingReader,
        budget: &Arc<crate::resources::ByteBudget>,
    ) -> io::Result<RegisteredAttempt<'_>> {
        self.register_attempt_inner(input, Some(reader), &mut [0; 4097], Some(budget), None)
    }

    /// The query's retained observation is released by this handoff.
    /// ```compile_fail
    /// use persistence::{Store, transaction::AttemptInput, canonical::SourceFrontierRead};
    /// use collector::ring::RingReader;
    /// fn retain_query(store: &Store, input: &AttemptInput, reader: &mut RingReader,
    ///                 mut query: SourceFrontierRead<'_, '_>) {
    ///     let attempt = store.register_attempt_from_frontier(input, reader, query).unwrap();
    ///     query.step().unwrap();
    /// }
    /// ```
    pub fn register_attempt_from_frontier(
        &self,
        input: &AttemptInput,
        reader: &mut RingReader,
        frontier: crate::canonical::SourceFrontierRead<'_, '_>,
    ) -> io::Result<RegisteredAttempt<'_>> {
        self.register_attempt_inner(input, Some(reader), &mut [0; 4097], None, Some(frontier))
    }

    /// Reopens retained authority without requiring a surviving transport. It grants no append.
    pub fn resume_attempt(&self) -> io::Result<RegisteredAttempt<'_>> {
        if !self.protocol_two {
            return Err(unresolved(
                "registered attempts require an initialized protocol-two root",
            ));
        }
        let mut bytes = [0; 4097];
        let length = read_slot(&self.directory, &mut bytes)?;
        let (_, input) = AttemptInput::decode_registered(&bytes[..length], self.recording)?;
        self.register_attempt_inner(&input, None, &mut bytes, None, None)
    }

    fn register_attempt_inner(
        &self,
        input: &AttemptInput,
        reader: Option<&mut RingReader>,
        scratch: &mut [u8; 4097],
        budget: Option<&Arc<crate::resources::ByteBudget>>,
        frontier: Option<crate::canonical::SourceFrontierRead<'_, '_>>,
    ) -> io::Result<RegisteredAttempt<'_>> {
        if !self.protocol_two {
            return Err(unresolved(
                "registered attempts require an initialized protocol-two root",
            ));
        }
        input.validate()?;
        if self
            .staging
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let permit = StagingPermit(Arc::clone(&self.staging));
        let root = &self.directory;
        validate_controls(root, self.recording)?;

        let length = read_slot(root, scratch)?;
        let slot = codec::decode_slot(&scratch[..length], self.recording, SlotKind::Foreground)?;
        let (transaction, resumed) = match slot {
            codec::Slot::Idle => {
                let reader = reader
                    .ok_or_else(|| unresolved("new registration requires source evidence"))?;
                if input.run.recording != self.recording
                    || reader.identity() != Some(input.run)
                    || reader.dictionary_capacity().is_some() != input.captured
                {
                    return Err(invalid());
                }
                self.validate_source_frontier(input, reader, budget, frontier)?;
                let transaction = collector::recording::random_identity()?;
                let mut bytes = [0; 4096];
                input.encode_registered(transaction, &mut bytes)?;
                private::atomic_write_in(root, "foreground", &bytes)?;
                (transaction, false)
            }
            codec::Slot::Occupied { .. } => {
                let (transaction, registered) =
                    AttemptInput::decode_registered(&scratch[..length], self.recording)?;
                if registered != *input {
                    return Err(invalid());
                }
                (transaction, true)
            }
        };
        let directory = self.register_attempt_directory(input, transaction, resumed)?;
        Ok(RegisteredAttempt {
            transaction,
            input: input.clone(),
            directory: directory.0,
            placement: directory.1,
            resumed,
            _permit: permit,
            owner: &self.owner,
            root: &self.directory,
        })
    }

    fn register_attempt_directory(
        &self,
        input: &AttemptInput,
        transaction: Identity,
        resumed: bool,
    ) -> io::Result<(PrivateDirectory, super::inventory::OriginalPlacement)> {
        let root = &self.directory;
        let base = root.open_directory(match input.operation {
            AttemptOperation::Ordinary { .. } => "data",
            AttemptOperation::Terminal { .. } => "recovery-data",
        })?;
        let name = collector::recording::identity_text(transaction).to_string();
        private::full_sync(&root.open_file("foreground")?)?;
        root.full_sync()?;
        let object = base.create_directory(&name)?;
        object.visit_names(5, |name| {
            if !["pending", "ready", "fragment-0", "fragment-1", "loss"].contains(&name) {
                return Err(invalid());
            }
            drop(object.open_directory(name)?);
            Ok(())
        })?;
        // A visible slot may outlive a failed directory sync. Restore authority before descendants.
        private::full_sync(&root.open_file("foreground")?)?;
        root.full_sync()?;
        object.full_sync()?;
        base.full_sync()?;
        let selected = super::inventory::select_original(&object)?;
        if let Some(selected) = selected {
            if !resumed {
                selected.0.visit_names(0, |_| Err(invalid()))?;
            }
            Ok(selected)
        } else {
            Ok((
                object.create_directory("pending")?,
                super::inventory::OriginalPlacement::Pending,
            ))
        }
    }

    pub fn source_frontier(
        &self,
        run: &collector::ring::RunIdentity,
        budget: &Arc<crate::resources::ByteBudget>,
        work: &mut crate::canonical::ReadWork,
    ) -> io::Result<Option<crate::canonical::SourceFrontier>> {
        if !self.protocol_two {
            return Err(unresolved(
                "canonical frontier requires protocol-two authority",
            ));
        }
        if run.recording != self.recording {
            return Err(invalid());
        }
        crate::canonical::SourceFrontier::read(&self.directory, run, budget, work)
    }

    fn validate_source_frontier(
        &self,
        input: &AttemptInput,
        reader: &mut RingReader,
        budget: Option<&Arc<crate::resources::ByteBudget>>,
        frontier: Option<crate::canonical::SourceFrontierRead<'_, '_>>,
    ) -> io::Result<()> {
        let frontier = if let Some(mut query) = frontier {
            query.validate_owner(&self.directory, &input.run)?;
            query.result()?.cloned()
        } else {
            self.source_frontier(
                &input.run,
                budget.ok_or_else(invalid)?,
                &mut crate::canonical::ReadWork::default(),
            )?
        };
        if let Some(frontier) = frontier {
            frontier.matches_input(input)?;
        } else if input.losses.predecessor != Predecessor::Genesis
            || input.losses.prior_window.is_some()
            || input.losses.publication_start != reader.publication_first()
            || input.losses.event_start != reader.event_first()
        {
            return Err(invalid());
        }
        if input.losses.event_inherited_end != input.losses.event_start || input.losses.events != 0
        {
            return Err(unresolved(
                "inherited event rows require registered inventory reconciliation",
            ));
        }
        let generation = reader.generation();
        let position = reader.next_publication();
        let result = validate_scanning_prefix(input, reader)
            .and_then(|()| validate_selected_window(input, reader));
        reader.seek_publication(position)?;
        result?;
        if reader.generation() != generation || reader.identity() != Some(input.run) {
            return Err(invalid());
        }
        if let AttemptOperation::Terminal { event_end } = input.operation {
            if !reader.completed() || reader.event_newest()?.checked_add(1) != Some(event_end) {
                return Err(unresolved(
                    "terminal final endpoint requires acquired completion and fixed generation",
                ));
            }
        }
        Ok(())
    }
}

fn validate_scanning_prefix(input: &AttemptInput, reader: &mut RingReader) -> io::Result<()> {
    reader.seek_publication(input.losses.publication_start)?;
    let mut timing = PublicationTiming::default();
    let bound = reader
        .shape()
        .slot_count
        .checked_add(2)
        .ok_or_else(invalid)?;
    for _ in 0..bound {
        if reader.next_publication() >= input.losses.publication_inherited_end {
            return Ok(());
        }
        match reader.read_timing_into(&mut timing)? {
            Found::Missed { first, end } if first < end => {}
            _ => return Err(invalid()),
        }
    }
    Err(invalid())
}

pub(super) fn batch_number(name: &str) -> io::Result<u32> {
    let digits = name.strip_suffix(".parquet").ok_or_else(invalid)?;
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let number: u32 = digits.parse().map_err(|_| invalid())?;
    if number >= 4096 {
        return Err(invalid());
    }
    Ok(number)
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
fn unresolved(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, reason)
}

fn read_slot(root: &PrivateDirectory, bytes: &mut [u8; 4097]) -> io::Result<usize> {
    let mut file = root.open_file("foreground")?;
    let mut length = 0;
    while length < bytes.len() {
        let count = file.read(&mut bytes[length..])?;
        if count == 0 {
            break;
        }
        length += count;
    }
    Ok(length)
}

fn validate_selected_window(input: &AttemptInput, reader: &mut RingReader) -> io::Result<()> {
    if let AttemptOperation::Ordinary {
        window,
        attempted_origin,
    } = input.operation
    {
        reader.seek_publication(attempted_origin)?;
        let mut timing = PublicationTiming::default();
        match reader.read_timing_into(&mut timing)? {
            Found::Publication { publication, .. } if publication == attempted_origin => {
                if crate::worker::timing_window_number(reader, &timing)
                    .map_err(crate::metadata::invalid)?
                    != window
                {
                    return Err(invalid());
                }
            }
            _ => {
                return Err(unresolved(concat!(
                    "selected timing disappeared before registration; ",
                    "prior timing evidence is not composed"
                )));
            }
        }
    }
    Ok(())
}

fn validate_controls(root: &PrivateDirectory, recording: Identity) -> io::Result<()> {
    match root.open_file("foreground.pending") {
        Ok(_) => {
            return Err(unresolved(
                "interrupted slot publication requires pending-slot reconciliation",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut state_bytes = [0; 256];
    let state = collector::recording::bootstrap::StateObservation::begin(
        root,
        recording,
        &mut state_bytes,
    )?
    .ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
    if !state.unchanged(&mut [0; 256])? {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    drop(state);
    Ok(())
}

impl RegisteredAttempt<'_> {
    pub(super) fn original_directory(&self) -> &PrivateDirectory {
        &self.directory
    }
    pub(super) fn validate_original_placement(
        &self,
        opens: &mut u64,
        names: &mut private::DirectoryWork,
    ) -> io::Result<()> {
        *opens += 1;
        let base = self.root.open_directory(match self.input.operation {
            AttemptOperation::Ordinary { .. } => "data",
            AttemptOperation::Terminal { .. } => "recovery-data",
        })?;
        *opens += 1;
        let object = base
            .open_directory(&collector::recording::identity_text(self.transaction).to_string())?;
        drop(base);
        object.visit_names_counted(5, names, |name| {
            if ["pending", "ready", "fragment-0", "fragment-1", "loss"].contains(&name) {
                Ok(())
            } else {
                Err(invalid())
            }
        })?;
        let (directory, placement) =
            super::inventory::select_original_counted(&object, opens)?.ok_or_else(invalid)?;
        if placement != self.placement || !directory.same_inode(&self.directory)? {
            return Err(invalid());
        }
        Ok(())
    }
    #[must_use]
    pub fn original_placement(&self) -> super::inventory::OriginalPlacement {
        self.placement
    }
}
