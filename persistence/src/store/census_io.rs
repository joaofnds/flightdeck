//! Paced selected-census construction and immutable witness admission.
use super::{
    BUFFER_BYTES, CensusHeader, CensusRecord, FILE_BYTES, HEADER_BYTES, RECORD_BYTES, RECORDS,
};
use crate::resources::{ByteBudget, ByteReservation};
use crate::store::census_digest::CensusDigest;
use crate::store::{FileIdentity, PublicationLedger, RegisteredAttempt};
use crate::transaction::census_binding::CensusBinding;
use collector::recording::{
    identity_text,
    private::{self, PrivateDirectory},
};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    sync::Arc,
};

pub(in crate::store) trait CensusFacts {
    fn record(&self, position: usize) -> io::Result<CensusRecord>;
}
pub(in crate::store) struct CensusWriter {
    header: CensusHeader,
    digest: Option<CensusDigest>,
    selected: Option<[u8; 32]>,
    selected_checked: usize,
    position: usize,
    written: usize,
    checked: usize,
    file: Option<File>,
    identity: Option<FileIdentity>,
    completed: Option<CensusBinding>,
    buffer: Vec<u8>,
    ledger: PublicationLedger,
    _reservation: ByteReservation,
}
impl CensusWriter {
    pub(in crate::store) fn header(&self) -> &CensusHeader {
        &self.header
    }
    pub(in crate::store) fn new(
        header: CensusHeader,
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<Self> {
        ledger.require_budget(budget)?;
        let reservation = budget
            .reserve(BUFFER_BYTES + 104 + 32 + 96 + size_of::<Self>())
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            header,
            digest: Some(CensusDigest::new()?),
            selected: None,
            selected_checked: 0,
            position: 0,
            written: 0,
            checked: 0,
            file: None,
            identity: None,
            completed: None,
            buffer: vec![0; BUFFER_BYTES],
            ledger: ledger.clone(),
            _reservation: reservation,
        })
    }
    /// Returns only a selected commitment or a fully synced/revalidated completed binding.
    pub(in crate::store) fn step(
        &mut self,
        facts: &impl CensusFacts,
        attempt: &RegisteredAttempt<'_>,
        binding: &CensusBinding,
    ) -> io::Result<Option<CensusBinding>> {
        if self.selected.is_none() {
            self.hash(facts)?;
            return Ok(None);
        }
        let selected = self.selected.ok_or_else(invalid)?;
        if binding.kind == 0 {
            if self.selected_checked < RECORDS {
                let next = self.step_select(facts, attempt)?;
                self.selected_checked = next;
                return Ok(None);
            }
            self.step_select(facts, attempt)?;
            return Ok(Some(CensusBinding {
                kind: 1,
                digest: selected,
                file: None,
            }));
        }
        if binding.digest != selected {
            return Err(invalid());
        }
        if let Some(completed) = self.completed {
            if binding.kind == 2 && *binding != completed {
                return Err(invalid());
            }
            return Ok(Some(completed));
        }
        let object = object(attempt, &self.ledger)?;
        if object.device_inode()? != self.header.object {
            return Err(invalid());
        }
        if self.written < FILE_BYTES {
            self.write(facts, &object, binding)?;
            return Ok(None);
        }
        if self.checked < RECORDS {
            let next = (self.checked + 128).min(RECORDS);
            validate_current(
                &self.header,
                facts,
                &object,
                self.checked..next,
                &self.ledger,
            )?;
            self.checked = next;
            return Ok(None);
        }
        self.completed = Some(self.step_sync(&object, binding)?);
        self.file = None;
        Ok(self.completed)
    }
    fn step_sync(
        &self,
        object: &PrivateDirectory,
        binding: &CensusBinding,
    ) -> io::Result<CensusBinding> {
        let file = self.file.as_ref().ok_or_else(invalid)?;
        self.ledger.update(|work| work.full_syncs += 1);
        private::full_sync(file)?;
        self.ledger.update(|work| work.full_syncs += 1);
        object.full_sync()?;
        let identity = checked_identity(file)?;
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        if checked_identity(&object.open_file("original-census")?)? != identity {
            return Err(invalid());
        }
        if identity.bytes() != collector::u64_from_usize(FILE_BYTES) {
            return Err(invalid());
        }
        if binding.kind == 2 && binding.file != Some(identity) {
            return Err(invalid());
        }
        Ok(CensusBinding {
            kind: 2,
            digest: self.selected.ok_or_else(invalid)?,
            file: Some(identity),
        })
    }
    fn step_select(
        &self,
        facts: &impl CensusFacts,
        attempt: &RegisteredAttempt<'_>,
    ) -> io::Result<usize> {
        let object = object(attempt, &self.ledger)?;
        if object.device_inode()? != self.header.object {
            return Err(invalid());
        }
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        match object.open_file("original-census") {
            Ok(_) => return Err(invalid()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let next = (self.selected_checked + 128).min(RECORDS);
        validate_current(
            &self.header,
            facts,
            &object,
            self.selected_checked..next,
            &self.ledger,
        )?;
        Ok(next)
    }
    fn hash(&mut self, facts: &impl CensusFacts) -> io::Result<()> {
        let count = (RECORDS - self.position).min(128);
        self.ledger
            .update(|work| work.proof_operations += collector::u64_from_usize(count));
        let (bytes, next) = encode_pass(&self.header, facts, self.position, &mut self.buffer)?;
        self.ledger
            .update(|work| work.inventory.decoded_bytes += collector::u64_from_usize(bytes));
        self.digest
            .as_mut()
            .ok_or_else(invalid)?
            .update(&self.buffer[..bytes])?;
        self.position = next;
        if next == RECORDS {
            self.selected = Some(self.digest.take().ok_or_else(invalid)?.finish()?);
        }
        Ok(())
    }
    fn write_open(&self, object: &PrivateDirectory, binding: &CensusBinding) -> io::Result<File> {
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        let opened = if binding.kind == 2 {
            object.open_file("original-census")
        } else {
            object.open_file_writable("original-census")
        };
        let file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound && binding.kind == 1 => {
                self.ledger.update(|work| work.canonical.creates += 1);
                object.create_file("original-census")?
            }
            Err(error) => return Err(error),
        };
        let identity = checked_identity(&file)?;
        if identity.bytes() > collector::u64_from_usize(FILE_BYTES)
            || (binding.kind == 2 && binding.file != Some(identity))
        {
            return Err(invalid());
        }
        Ok(file)
    }

    fn write(
        &mut self,
        facts: &impl CensusFacts,
        object: &PrivateDirectory,
        binding: &CensusBinding,
    ) -> io::Result<()> {
        if self.file.is_none() {
            let file = self.write_open(object, binding)?;
            self.identity = Some(checked_identity(&file)?);
            self.file = Some(file);
        }
        let start = self.written;
        let position = if start == 0 {
            0
        } else {
            (start - HEADER_BYTES) / RECORD_BYTES
        };
        self.ledger.update(|work| {
            work.proof_operations += collector::u64_from_usize((RECORDS - position).min(128));
        });
        let (length, _) = encode_pass(&self.header, facts, position, &mut self.buffer)?;
        let file = self.file.as_mut().ok_or_else(invalid)?;
        let identity = checked_identity(file)?;
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        if checked_identity(&object.open_file("original-census")?)? != identity {
            return Err(invalid());
        }
        if Some(identity) != self.identity {
            return Err(invalid());
        }
        let existing = usize::try_from(identity.bytes()).map_err(|_| invalid())?;
        let compare = existing.saturating_sub(start).min(length);
        file.seek(SeekFrom::Start(collector::u64_from_usize(start)))?;
        write_compare(file, &self.buffer[..compare], &self.ledger)?;
        if checked_identity(file)? != identity {
            return Err(invalid());
        }
        if compare < length {
            if binding.kind != 1 || start + compare != existing {
                return Err(invalid());
            }
            self.ledger.update(|work| {
                work.proof_operations += 1;
                work.census_write_operations += 1;
                work.census_write_requested_bytes += collector::u64_from_usize(length - compare);
            });
            let result = file
                .write_all(&self.buffer[compare..length])
                .and_then(|()| checked_identity(file));
            match result {
                Ok(identity) => self.identity = Some(identity),
                Err(error) => {
                    self.file = None;
                    self.identity = None;
                    return Err(error);
                }
            }
        }
        self.identity = Some(checked_identity(file)?);
        self.written += length;
        Ok(())
    }
}
pub(in crate::store) fn object(
    attempt: &RegisteredAttempt<'_>,
    ledger: &PublicationLedger,
) -> io::Result<PrivateDirectory> {
    ledger.update(|work| work.inventory.open_attempts += 1);
    let base = attempt.root.open_directory(match attempt.input.operation {
        crate::transaction::AttemptOperation::Ordinary { .. } => "data",
        crate::transaction::AttemptOperation::Terminal { .. }
        | crate::transaction::AttemptOperation::Finite { .. } => "recovery-data",
    })?;
    ledger.update(|work| work.inventory.open_attempts += 1);
    base.open_directory(&identity_text(attempt.transaction).to_string())
}
fn encode_pass(
    header: &CensusHeader,
    facts: &impl CensusFacts,
    position: usize,
    buffer: &mut [u8],
) -> io::Result<(usize, usize)> {
    let mut bytes = 0;
    if position == 0 {
        header.encode((&mut buffer[..256]).try_into().map_err(|_| invalid())?)?;
        bytes = HEADER_BYTES;
    }
    let next = (position + 128).min(RECORDS);
    for index in position..next {
        let record = facts.record(index)?;
        header.validate_record(index, &record)?;
        record.encode(
            index,
            (&mut buffer[bytes..bytes + RECORD_BYTES])
                .try_into()
                .map_err(|_| invalid())?,
        )?;
        bytes += RECORD_BYTES;
    }
    Ok((bytes, next))
}
fn checked_identity(file: &File) -> io::Result<FileIdentity> {
    if file.metadata()?.nlink() != 1 {
        return Err(invalid());
    }
    FileIdentity::read(file)
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
fn validate_current_original(
    header: &CensusHeader,
    object: &PrivateDirectory,
    ledger: &PublicationLedger,
) -> io::Result<Option<PrivateDirectory>> {
    let original = match header.original {
        Some((placement, inode)) => {
            ledger.update(|work| work.inventory.open_attempts += 1);
            let directory = object.open_directory(match placement {
                super::OriginalPlacement::Pending => "pending",
                super::OriginalPlacement::Ready => "ready",
            })?;
            if directory.device_inode()? != inode {
                return Err(invalid());
            }
            Some(directory)
        }
        None => None,
    };
    Ok(original)
}
fn validate_current(
    header: &CensusHeader,
    facts: &impl CensusFacts,
    object: &PrivateDirectory,
    positions: std::ops::Range<usize>,
    ledger: &PublicationLedger,
) -> io::Result<()> {
    let original = validate_current_original(header, object, ledger)?;
    for name in ["pending", "ready"] {
        let selected = header.original.map(|(placement, _)| match placement {
            super::OriginalPlacement::Pending => "pending",
            super::OriginalPlacement::Ready => "ready",
        });
        if selected != Some(name) {
            ledger.update(|work| work.inventory.open_attempts += 1);
            match object.open_directory(name) {
                Ok(_) => return Err(invalid()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    if header.events.is_none()
        && matches!(
            header.input.operation,
            crate::transaction::AttemptOperation::Ordinary { .. }
                | crate::transaction::AttemptOperation::Finite { .. }
        )
    {
        if let Some(directory) = original.as_ref() {
            ledger.update(|work| work.inventory.open_attempts += 1);
            match directory.open_directory("events") {
                Ok(_) => return Err(invalid()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    let events = if let (Some(inode), Some(directory)) = (header.events, original.as_ref()) {
        ledger.update(|work| work.inventory.open_attempts += 1);
        let events = directory.open_directory("events")?;
        if events.device_inode()? != inode {
            return Err(invalid());
        }
        Some(events)
    } else {
        None
    };
    for position in positions {
        let expected = facts.record(position)?;
        if expected.class == 6 {
            return Err(invalid());
        }
        let (parent, name) = record_name(header, original.as_ref(), events.as_ref(), position);
        ledger.update(|work| {
            work.proof_operations += 1;
            work.inventory.open_attempts += u64::from(parent.is_some());
        });
        let actual = match parent.map(|parent| parent.open_file(&name)) {
            Some(Ok(file)) => Some(checked_identity(&file)?),
            Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Some(Err(error)) => return Err(error),
            None => None,
        };
        if actual != expected.file {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(in crate::store) struct CensusReader {
    header: Option<CensusHeader>,
    facts: Option<super::NormalInventory>,
    position: usize,
    digest: Option<CensusDigest>,
    buffer: Vec<u8>,
    ledger: PublicationLedger,
    _reservation: ByteReservation,
}
impl CensusReader {
    pub(in crate::store) fn new(
        budget: &Arc<ByteBudget>,
        ledger: &PublicationLedger,
    ) -> io::Result<Self> {
        ledger.require_budget(budget)?;
        let reservation = budget
            .reserve(BUFFER_BYTES + 104 + 32 + 96 + size_of::<Self>())
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            header: None,
            facts: None,
            position: 0,
            digest: Some(CensusDigest::new()?),
            buffer: vec![0; BUFFER_BYTES],
            ledger: ledger.clone(),
            _reservation: reservation,
        })
    }
    fn step_header(
        &self,
        attempt: &RegisteredAttempt<'_>,
        object: &PrivateDirectory,
    ) -> io::Result<CensusHeader> {
        CensusHeader::decode(
            self.buffer[..256].try_into().map_err(|_| invalid())?,
            &attempt.input,
            attempt.transaction,
            object.device_inode()?,
        )
    }
    pub(in crate::store) fn step(
        &mut self,
        attempt: &RegisteredAttempt<'_>,
        binding: &CensusBinding,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<Option<super::NormalInventory>> {
        if binding.kind != 2 {
            return Err(invalid());
        }
        let object = object(attempt, &self.ledger)?;
        if let Some(header) = &self.header {
            if object.device_inode()? != header.object {
                return Err(invalid());
            }
        }
        self.ledger.update(|work| work.inventory.open_attempts += 1);
        let mut file = object.open_file("original-census")?;
        let identity = checked_identity(&file)?;
        if Some(identity) != binding.file {
            return Err(invalid());
        }
        let prefix = if self.position == 0 { HEADER_BYTES } else { 0 };
        let next = (self.position + 128).min(RECORDS);
        let length = prefix + (next - self.position) * RECORD_BYTES;
        let offset = if self.position == 0 {
            0
        } else {
            HEADER_BYTES + self.position * RECORD_BYTES
        };
        self.ledger.update(|work| {
            work.proof_operations += collector::u64_from_usize(next - self.position);
            work.inventory.decoded_bytes += collector::u64_from_usize(length);
        });
        file.seek(SeekFrom::Start(collector::u64_from_usize(offset)))?;
        file.read_exact(&mut self.buffer[..length])?;
        if checked_identity(&file)? != identity {
            return Err(invalid());
        }
        if self.header.is_none() {
            let header = self.step_header(attempt, &object)?;
            self.facts = Some(super::NormalInventory::census_empty(&header, budget)?);
            self.header = Some(header);
        }
        self.step_records(prefix, next)?;
        self.digest
            .as_mut()
            .ok_or_else(invalid)?
            .update(&self.buffer[..length])?;
        self.position = next;
        if next < RECORDS {
            return Ok(None);
        }
        if self.digest.take().ok_or_else(invalid)?.finish()? != binding.digest {
            return Err(invalid());
        }
        Ok(Some(self.facts.take().ok_or_else(invalid)?))
    }
    fn step_records(&mut self, prefix: usize, next: usize) -> io::Result<()> {
        // Decode into the retained summary; only full hash admission grants authority.
        for position in self.position..next {
            let start = prefix + (position - self.position) * RECORD_BYTES;
            let record = CensusRecord::decode(
                position,
                self.buffer[start..start + RECORD_BYTES]
                    .try_into()
                    .map_err(|_| invalid())?,
            )?;
            self.header
                .as_ref()
                .ok_or_else(invalid)?
                .validate_record(position, &record)?;
            if record.class == 6 {
                return Err(invalid());
            }
            self.facts
                .as_mut()
                .ok_or_else(invalid)?
                .census_set_record(position, record)?;
        }
        Ok(())
    }
}

/// Only the durable region8 checkpoint permits absence of the completed witness.
pub(in crate::store) fn retire(
    attempt: &RegisteredAttempt<'_>,
    state: &crate::transaction::normal::NormalState,
    ledger: &PublicationLedger,
) -> io::Result<()> {
    if state.phase != collector::recording::bootstrap::codec::Phase::SalvageCommitted
        || state.cleanup.region != 8
    {
        return Err(invalid());
    }
    let binding = &state.census;
    if binding.kind != 2 {
        return Err(invalid());
    }
    let object = object(attempt, ledger)?;
    let row_route = state.row_plan.is_some();
    let mut directory_work = private::DirectoryWork::default();
    let result =
        object.visit_names_counted(if row_route { 3 } else { 2 }, &mut directory_work, |name| {
            let allowed = if row_route {
                matches!(name, "fragment-0" | "fragment-1")
            } else {
                name == "loss"
            };
            if name == "original-census" || allowed {
                Ok(())
            } else {
                Err(invalid())
            }
        });
    ledger.update(|work| {
        work.inventory.directory.entries += directory_work.entries;
        work.inventory.directory.names += directory_work.names;
        work.inventory.directory.read_calls += directory_work.read_calls;
        work.inventory.directory.open_attempts += directory_work.open_attempts;
    });
    result?;
    for name in ["pending", "ready"] {
        ledger.update(|work| work.inventory.open_attempts += 1);
        match object.open_directory(name) {
            Ok(_) => return Err(invalid()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    ledger.update(|work| work.inventory.open_attempts += 1);
    match object.open_file("original-census") {
        Ok(file) => {
            if Some(checked_identity(&file)?) != binding.file {
                return Err(invalid());
            }
            ledger.update(|work| work.cleanup_removals += 1);
            object.remove_file("original-census")?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    ledger.update(|work| work.full_syncs += 1);
    object.full_sync()
}

pub(in crate::store) fn check_bound(
    attempt: &RegisteredAttempt<'_>,
    binding: &CensusBinding,
    ledger: &PublicationLedger,
) -> io::Result<()> {
    if binding.kind != 2 {
        return Err(invalid());
    }
    let object = object(attempt, ledger)?;
    ledger.update(|work| work.inventory.open_attempts += 1);
    let file = object.open_file("original-census")?;
    if Some(checked_identity(&file)?) != binding.file {
        return Err(invalid());
    }
    Ok(())
}

pub(in crate::store) fn cleanup_admission(
    header: &CensusHeader,
    facts: &impl CensusFacts,
    attempt: &RegisteredAttempt<'_>,
    state: &crate::transaction::normal::NormalState,
    position: usize,
    ledger: &PublicationLedger,
) -> io::Result<usize> {
    let object = object(attempt, ledger)?;
    if object.device_inode()? != header.object {
        return Err(invalid());
    }
    let original = cleanup_admission_original(header, &object, state, ledger)?;
    let events = cleanup_admission_events(header, original.as_ref(), state, ledger)?;
    let next = (position + 128).min(RECORDS);
    for position in position..next {
        let record = facts.record(position)?;
        let (parent, name) = record_name(header, original.as_ref(), events.as_ref(), position);
        ledger.update(|work| {
            work.proof_operations += 1;
            work.inventory.open_attempts += u64::from(parent.is_some());
        });
        let actual = match parent.map(|directory| directory.open_file(&name)) {
            Some(Ok(file)) => Some(checked_identity(&file)?),
            Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Some(Err(error)) => return Err(error),
            None => None,
        };
        let (removed, attempted) = cleanup_admission_position(state, position);
        if (removed && actual.is_some())
            || (!removed && actual != record.file && !(attempted && actual.is_none()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unpreserved/conflicting original census identity",
            ));
        }
    }
    Ok(next)
}
fn record_name<'a>(
    header: &CensusHeader,
    original: Option<&'a PrivateDirectory>,
    events: Option<&'a PrivateDirectory>,
    position: usize,
) -> (Option<&'a PrivateDirectory>, String) {
    if position < 11 {
        (
            original,
            crate::store::attempt::TABLE_FILES[position].to_string(),
        )
    } else if position == 11 {
        (original, "metadata".into())
    } else if position == 12 {
        (events, "index".into())
    } else {
        (
            if matches!(
                header.input.operation,
                crate::transaction::AttemptOperation::Terminal { .. }
            ) {
                original
            } else {
                events
            },
            crate::event_recovery::batch_name(position - 13),
        )
    }
}
fn cleanup_admission_position(
    state: &crate::transaction::normal::NormalState,
    position: usize,
) -> (bool, bool) {
    let cursor = state.cleanup;
    if cursor.region == 0 {
        return (
            false,
            state.row_plan.is_some()
                && state.phase == collector::recording::bootstrap::codec::Phase::SalvageCommitted
                && position == 13,
        );
    }
    if state.row_plan.is_some() {
        if position >= 13 {
            let ordinal = collector::u64_from_usize(position - 13);
            return (
                ordinal < u64::from(cursor.ordinal),
                ordinal == u64::from(cursor.ordinal),
            );
        }
        return (
            cursor.directory == 1,
            position == 12 && cursor.ordinal == 4096,
        );
    }
    if position >= 13 {
        let ordinal = collector::u64_from_usize(position - 13);
        return (
            cursor.region > 1 || ordinal < u64::from(cursor.ordinal),
            cursor.region == 1
                && ordinal >= u64::from(cursor.ordinal)
                && ordinal < u64::from(cursor.ordinal).saturating_add(127),
        );
    }
    if position < 11 {
        return (cursor.region > 2, cursor.region == 2);
    }
    if position == 12 {
        return (
            cursor.region == 3 && cursor.directory == 1,
            cursor.region == 3,
        );
    }
    (false, cursor.region == 3 && cursor.directory == 1)
}
fn cleanup_admission_original(
    header: &CensusHeader,
    object: &PrivateDirectory,
    state: &crate::transaction::normal::NormalState,
    ledger: &PublicationLedger,
) -> io::Result<Option<PrivateDirectory>> {
    let mut selected = None;
    for (name, placement) in [
        ("pending", super::OriginalPlacement::Pending),
        ("ready", super::OriginalPlacement::Ready),
    ] {
        ledger.update(|work| work.inventory.open_attempts += 1);
        match object.open_directory(name) {
            Ok(directory) => {
                if header.original != Some((placement, directory.device_inode()?)) {
                    return Err(invalid());
                }
                selected = Some(directory);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if selected.is_none()
        && header.original.is_some()
        && !((state.cleanup.region == 3 || state.row_plan.is_some())
            && state.cleanup.directory == 1)
    {
        return Err(invalid());
    }
    Ok(selected)
}
fn cleanup_admission_events(
    header: &CensusHeader,
    original: Option<&PrivateDirectory>,
    state: &crate::transaction::normal::NormalState,
    ledger: &PublicationLedger,
) -> io::Result<Option<PrivateDirectory>> {
    if matches!(
        header.input.operation,
        crate::transaction::AttemptOperation::Terminal { .. }
    ) {
        return Ok(None);
    }
    let Some(original) = original else {
        return Ok(None);
    };
    ledger.update(|work| work.inventory.open_attempts += 1);
    match original.open_directory("events") {
        Ok(events) => {
            if Some(events.device_inode()?) != header.events {
                return Err(invalid());
            }
            Ok(Some(events))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let allowed = if state.row_plan.is_some() {
                state.cleanup.ordinal == 4096
            } else {
                state.cleanup.region == 3
            };
            if header.events.is_some() && !allowed {
                return Err(invalid());
            }
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn write_compare(file: &mut File, expected: &[u8], ledger: &PublicationLedger) -> io::Result<()> {
    // The comparison scratch shares the writer's fixed charged workspace.
    let mut actual = [0; 96];
    for offset in (0..expected.len()).step_by(96) {
        let count = (expected.len() - offset).min(96);
        ledger.update(|work| {
            work.proof_operations += 1;
            work.inventory.decoded_bytes += collector::u64_from_usize(count);
        });
        file.read_exact(&mut actual[..count])?;
        if actual[..count] != expected[offset..offset + count] {
            return Err(invalid());
        }
    }
    Ok(())
}
