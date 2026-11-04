//! EXEC-11 deterministic fixed original census encoding.
use super::{
    BatchFact, FileIdentity, IndexFact, NormalInventory, OriginalInventory, OriginalPlacement,
};
use crate::transaction::{AttemptInput, AttemptOperation};
use collector::ring::Identity;
use std::io;
use std::os::unix::fs::MetadataExt;
#[path = "census_io.rs"]
pub(in crate::store) mod io_owner;
impl io_owner::CensusFacts for OriginalInventory<'_, '_> {
    fn record(&self, position: usize) -> io::Result<CensusRecord> {
        self.census_record(position)
    }
}
impl io_owner::CensusFacts for NormalInventory {
    fn record(&self, position: usize) -> io::Result<CensusRecord> {
        self.census_record(position)
    }
}
impl OriginalInventory<'_, '_> {
    pub(in crate::store) fn census_header(&self, object: (u64, u64)) -> io::Result<CensusHeader> {
        Ok(CensusHeader {
            input: self.owner().input.clone(),
            transaction: self.owner().transaction,
            object,
            original: Some((
                self.owner().placement,
                self.owner().directory.device_inode()?,
            )),
            events: self
                .events
                .as_ref()
                .map(collector::recording::private::PrivateDirectory::device_inode)
                .transpose()?,
        })
    }
}

pub(in crate::store) const RECORDS: usize = 4109;
pub(in crate::store) const RECORD_BYTES: usize = 96;
pub(in crate::store) const HEADER_BYTES: usize = 256;
pub(in crate::store) const FILE_BYTES: usize = HEADER_BYTES + RECORDS * RECORD_BYTES;
pub(in crate::store) const BUFFER_BYTES: usize = HEADER_BYTES + 128 * RECORD_BYTES;
const _: () = {
    assert!(FILE_BYTES == 394_720);
    assert!(RECORDS == super::POSITIONS);
    assert!(BUFFER_BYTES == 12544);
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::store) struct CensusRecord {
    pub class: u32,
    pub file: Option<FileIdentity>,
    pub first: u64,
    pub end: u64,
    pub rows: u64,
    pub batches: u32,
}
impl CensusRecord {
    pub(in crate::store) fn encode(&self, position: usize, bytes: &mut [u8; 96]) -> io::Result<()> {
        self.validate(position)?;
        bytes.fill(0);
        put32(bytes, 0, self.class);
        if let Some(file) = &self.file {
            file.census_encode((&mut bytes[8..64]).try_into().map_err(|_| invalid())?)?;
        }
        for (offset, value) in [(64, self.first), (72, self.end), (80, self.rows)] {
            put64(bytes, offset, value);
        }
        put32(bytes, 88, self.batches);
        Ok(())
    }
    pub(in crate::store) fn decode(position: usize, bytes: &[u8; 96]) -> io::Result<Self> {
        let class = get32(bytes, 0)?;
        let record = Self {
            class,
            file: if class == 0 {
                None
            } else {
                Some(FileIdentity::census_decode(
                    bytes[8..64].try_into().map_err(|_| invalid())?,
                )?)
            },
            first: get64(bytes, 64)?,
            end: get64(bytes, 72)?,
            rows: get64(bytes, 80)?,
            batches: get32(bytes, 88)?,
        };
        let mut expected = [0; 96];
        record.encode(position, &mut expected)?;
        if expected != *bytes {
            return Err(invalid());
        }
        Ok(record)
    }
    fn validate(&self, position: usize) -> io::Result<()> {
        if position >= RECORDS
            || self.class > 6
            || (self.class == 0 && *self != Self::default())
            || (self.class > 0 && self.file.is_none())
        {
            return Err(invalid());
        }
        if self.class == 0 {
            return Ok(());
        }
        if (position < 12 && self.class != 1)
            || (position == 12 && !matches!(self.class, 2 | 3))
            || (position > 12 && !matches!(self.class, 4..=6))
        {
            return Err(invalid());
        }
        if matches!(self.class, 1 | 2 | 4 | 6)
            && (self.first != 0 || self.end != 0 || self.rows != 0 || self.batches != 0)
        {
            return Err(invalid());
        }
        if matches!(self.class, 3 | 5)
            && (self.first == 0 || self.end < self.first || self.rows > self.end - self.first)
        {
            return Err(invalid());
        }
        if self.class == 3
            && (self.rows > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
                || self.batches > 4096
                || u64::from(self.batches) > self.rows
                || self.rows > u64::from(self.batches) * 16384
                || (self.rows == 0) != (self.batches == 0))
        {
            return Err(invalid());
        }
        if self.class == 5 && (self.rows == 0 || self.rows > 16384 || self.batches != 0) {
            return Err(invalid());
        }
        Ok(())
    }
}
impl OriginalInventory<'_, '_> {
    pub(in crate::store) fn census_record(&self, position: usize) -> io::Result<CensusRecord> {
        record(&self.files, &self.index, &self.batches, position)
    }
}
impl NormalInventory {
    pub(in crate::store) fn census_record(&self, position: usize) -> io::Result<CensusRecord> {
        record(&self.files, &self.index, &self.batches, position)
    }
    pub(in crate::store) fn census_set_record(
        &mut self,
        position: usize,
        record: CensusRecord,
    ) -> io::Result<()> {
        record.validate(position)?;
        match position {
            0..=11 => {
                self.files[position] = record.file;
            }
            12 => {
                self.index = match record.class {
                    0 => IndexFact::Missing,
                    2 => IndexFact::Malformed {
                        file: record.file.ok_or_else(invalid)?,
                    },
                    3 => IndexFact::Valid {
                        file: record.file.ok_or_else(invalid)?,
                        first: record.first,
                        end: record.end,
                        rows: record.rows,
                        batches: record.batches,
                    },
                    _ => return Err(invalid()),
                };
            }
            _ => {
                self.batches[position - 13] = match record.class {
                    0 => BatchFact::Missing,
                    4 | 6 => BatchFact::Malformed {
                        file: record.file.ok_or_else(invalid)?,
                        framed: record.class == 6,
                    },
                    5 => BatchFact::Complete {
                        file: record.file.ok_or_else(invalid)?,
                        first: record.first,
                        end: record.end,
                        rows: record.rows,
                    },
                    _ => return Err(invalid()),
                };
            }
        }
        Ok(())
    }
}
fn record(
    files: &[Option<FileIdentity>; 12],
    index: &IndexFact,
    batches: &[BatchFact],
    position: usize,
) -> io::Result<CensusRecord> {
    let mut record = CensusRecord::default();
    match position {
        0..=11 => {
            if let Some(file) = files[position] {
                record.class = 1;
                record.file = Some(file);
            }
        }
        12 => match *index {
            IndexFact::Missing => {}
            IndexFact::Malformed { file } => {
                record.class = 2;
                record.file = Some(file);
            }
            IndexFact::Valid {
                file,
                first,
                end,
                rows,
                batches,
            } => {
                record = CensusRecord {
                    class: 3,
                    file: Some(file),
                    first,
                    end,
                    rows,
                    batches,
                };
            }
            IndexFact::Unprobed => return Err(invalid()),
        },
        _ => match *batches.get(position - 13).ok_or_else(invalid)? {
            BatchFact::Missing => {}
            BatchFact::Malformed { file, framed } => {
                record.class = if framed { 6 } else { 4 };
                record.file = Some(file);
            }
            BatchFact::Complete {
                file,
                first,
                end,
                rows,
            } => {
                record = CensusRecord {
                    class: 5,
                    file: Some(file),
                    first,
                    end,
                    rows,
                    batches: 0,
                };
            }
            BatchFact::Unprobed => return Err(invalid()),
        },
    }
    record.validate(position)?;
    Ok(record)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::store) struct CensusHeader {
    pub input: AttemptInput,
    pub transaction: Identity,
    pub object: (u64, u64),
    pub original: Option<(OriginalPlacement, (u64, u64))>,
    pub events: Option<(u64, u64)>,
}
impl CensusHeader {
    pub(in crate::store) fn encode(&self, bytes: &mut [u8; 256]) -> io::Result<()> {
        bytes.fill(0);
        if self.input.is_finite()
            && self
                .original
                .is_some_and(|(placement, _)| placement != OriginalPlacement::Pending)
        {
            return Err(invalid());
        }

        bytes[..8].copy_from_slice(b"FDCENS02");
        for (offset, value) in [
            (8, 1),
            (12, 96),
            (16, 4109),
            (20, u32::from(self.input.captured)),
            (
                24,
                match self.input.operation {
                    AttemptOperation::Ordinary { .. } => 1,
                    AttemptOperation::Terminal { .. } => 2,
                    AttemptOperation::Finite { .. } => 3,
                },
            ),
        ] {
            put32(bytes, offset, value);
        }
        if let Some((placement, inode)) = self.original {
            put32(
                bytes,
                28,
                match placement {
                    OriginalPlacement::Pending => 1,
                    OriginalPlacement::Ready => 2,
                },
            );
            put_pair(bytes, 112, inode);
        } else if self.events.is_some() {
            return Err(invalid());
        }
        for (offset, identity) in [
            (32, self.input.run.recording),
            (48, self.input.run.run),
            (64, self.input.run.boot),
            (80, self.transaction),
        ] {
            put_pair(bytes, offset, (identity.0[0], identity.0[1]));
        }
        put_pair(bytes, 96, self.object);
        if let Some(events) = self.events {
            put_pair(bytes, 128, events);
            put32(bytes, 144, 1);
        }
        crate::transaction::source_binding::encode(&self.input.binding, &mut bytes[152..200])?;
        put64(bytes, 200, self.input.source.layout);
        put32(bytes, 216, 1);
        put64(
            bytes,
            224,
            self.input.source.origin_continuous_ns.ok_or_else(invalid)?,
        );
        self.encode_operation(bytes)
    }
    fn encode_operation(&self, bytes: &mut [u8; 256]) -> io::Result<()> {
        match self.input.operation {
            AttemptOperation::Ordinary {
                window,
                attempted_origin,
            } => {
                put32(bytes, 208, 4095);
                put64(bytes, 232, window);
                put64(bytes, 240, attempted_origin);
            }
            AttemptOperation::Finite { order_floor, .. } => {
                put32(bytes, 208, 512);
                put64(bytes, 232, order_floor);
            }
            AttemptOperation::Terminal { .. } => {
                if self.events.is_some() {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
    pub(in crate::store) fn decode(
        bytes: &[u8; 256],
        input: &AttemptInput,
        transaction: Identity,
        object: (u64, u64),
    ) -> io::Result<Self> {
        let original = match get32(bytes, 28)? {
            0 => None,
            1 => Some((OriginalPlacement::Pending, get_pair(bytes, 112)?)),
            2 => Some((OriginalPlacement::Ready, get_pair(bytes, 112)?)),
            _ => return Err(invalid()),
        };
        let events = match get32(bytes, 144)? {
            0 => None,
            1 => Some(get_pair(bytes, 128)?),
            _ => return Err(invalid()),
        };
        let header = Self {
            input: input.clone(),
            transaction,
            object,
            original,
            events,
        };
        let mut expected = [0; 256];
        header.encode(&mut expected)?;
        if expected != *bytes {
            return Err(invalid());
        }
        Ok(header)
    }
    pub(in crate::store) fn validate_record(
        &self,
        position: usize,
        record: &CensusRecord,
    ) -> io::Result<()> {
        if record.class != 0
            && ((self.input.is_finite() && position < 12)
                || self.original.is_none()
                || (matches!(self.input.operation, AttemptOperation::Terminal { .. })
                    && (position < 11 || position == 12))
                || (matches!(
                    self.input.operation,
                    AttemptOperation::Ordinary { .. } | AttemptOperation::Finite { .. }
                ) && self.events.is_none()
                    && position >= 12))
        {
            return Err(invalid());
        }
        record.validate(position)
    }
}
fn get32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_be_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn get64(bytes: &[u8], offset: usize) -> io::Result<u64> {
    Ok(u64::from_be_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}
fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
}
fn get_pair(bytes: &[u8], offset: usize) -> io::Result<(u64, u64)> {
    Ok((get64(bytes, offset)?, get64(bytes, offset + 8)?))
}
fn put_pair(bytes: &mut [u8], offset: usize, pair: (u64, u64)) {
    put64(bytes, offset, pair.0);
    put64(bytes, offset + 8, pair.1);
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

impl NormalInventory {
    fn census_empty(
        header: &CensusHeader,
        budget: &std::sync::Arc<crate::resources::ByteBudget>,
    ) -> io::Result<Self> {
        let reservation = budget
            .reserve(super::SUMMARY_BYTES)
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            census_header: Some(header.clone()),
            cleanup_original_inode: header.original.map_or((0, 0), |(_, inode)| inode),
            cleanup_events_inode: header.events,
            files: [None; 12],
            batches: vec![BatchFact::Missing; 4096],
            index: IndexFact::Missing,
            // Full timestamp directory witnesses are used only by normal live publication.
            original_identity: None,
            events_identity: None,
            _reservation: reservation,
        })
    }
}

impl NormalInventory {
    pub(in crate::store) fn cleanup_named(
        &self,
        name: &str,
        file: &std::fs::File,
    ) -> io::Result<()> {
        let position =
            if let Some(position) = super::TABLE_FILES.iter().position(|held| *held == name) {
                position
            } else if name == "metadata" {
                11
            } else if name == "index" {
                12
            } else {
                13 + usize::try_from(crate::store::attempt::batch_number(name)?)
                    .map_err(|_| invalid())?
            };
        let record = self.census_record(position)?;
        if file.metadata()?.nlink() != 1 {
            return Err(invalid());
        }
        if record.file != Some(FileIdentity::read(file)?) || record.class == 6 {
            return Err(invalid());
        }
        Ok(())
    }
    pub(in crate::store) fn cleanup_original_directory(
        &self,
        original: &collector::recording::private::PrivateDirectory,
    ) -> io::Result<()> {
        if original.device_inode()? != self.cleanup_original_inode {
            return Err(invalid());
        }
        Ok(())
    }
}

impl NormalInventory {
    pub(in crate::store) fn census_bind_header(&mut self, header: &CensusHeader) {
        self.census_header = Some(header.clone());
    }
    pub(in crate::store) fn census_cleanup_pass(
        &self,
        attempt: &crate::store::RegisteredAttempt<'_>,
        state: &crate::transaction::normal::NormalState,
        position: usize,
        ledger: &crate::store::PublicationLedger,
    ) -> io::Result<usize> {
        io_owner::cleanup_admission(
            self.census_header.as_ref().ok_or_else(invalid)?,
            self,
            attempt,
            state,
            position,
            ledger,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_record_preserves_every_native_identity_word() {
        // Goal: the immutable census retains all identity, size, timestamps and batch facts.
        // Method: independently authored96 bytes, exact round trip and reserved/range mutations.
        let mut literal = [0; 96];
        literal[3] = 5;
        for (offset, value) in [
            (8, 7u64),
            (16, 9),
            (24, 123),
            (40, 456),
            (56, 789),
            (64, 2),
            (72, 5),
            (80, 2),
        ] {
            literal[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
        }
        literal[32..40].copy_from_slice(&(-3i64).to_be_bytes());
        literal[48..56].copy_from_slice(&(-4i64).to_be_bytes());
        let record = CensusRecord::decode(4108, &literal).unwrap();
        let mut encoded = [0; 96];
        record.encode(4108, &mut encoded).unwrap();
        assert_eq!(encoded, literal);
        assert_eq!(record.file.unwrap().inode, 9);
        for offset in [4, 7, 88, 92, 95] {
            let mut bad = literal;
            bad[offset] = 1;
            assert!(CensusRecord::decode(4108, &bad).is_err());
        }
        for value in [-1i64, 1_000_000_000] {
            let mut bad = literal;
            bad[40..48].copy_from_slice(&value.to_be_bytes());
            assert!(CensusRecord::decode(4108, &bad).is_err());
        }
        assert!(CensusRecord::decode(12, &literal).is_err());
        assert!(CensusRecord::decode(4109, &literal).is_err());
        let mut bad = literal;
        bad[80..88].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(CensusRecord::decode(4108, &bad).is_err());
        assert_eq!(
            CensusRecord::decode(0, &[0; 96]).unwrap(),
            CensusRecord::default()
        );
    }
}
