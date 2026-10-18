//! One bounded word representation shared by ring slots and saved tick text.

use super::{DISKS_MAX, DiskReading, DiskSnapshot, Error, ScanState};
use crate::u64_from_usize;

pub const MAGIC: u64 = u64::from_be_bytes(*b"FDDISK01");
pub const HEADER_WORDS: usize = 4;
pub const RECORD_WORDS: usize = 15;
pub const WORDS_MAX: usize = HEADER_WORDS + DISKS_MAX * RECORD_WORDS;
const FAILED: u64 = 1 << 8;
const FLAGS: u64 = (1 << 9) - 1;

const _: () = assert!(WORDS_MAX == 244);
const _: () = assert!(WORDS_MAX * 16 < 4096);

pub fn encode(snapshot: &DiskSnapshot, words: &mut [u64]) -> Result<usize, Error> {
    validate(snapshot)?;
    let count = HEADER_WORDS + snapshot.count * RECORD_WORDS;
    if words.len() < count {
        return Err(Error::Capacity);
    }

    words.fill(0);
    words[0] = MAGIC;
    words[1] = snapshot.source_continuous_ns;
    words[2] = match snapshot.state {
        ScanState::Complete => 0,
        ScanState::Failed => 1,
        ScanState::Exhausted => 2,
        ScanState::Partial => 3,
    };
    words[3] = u64_from_usize(snapshot.count);
    for (index, record) in snapshot.records[..snapshot.count].iter().enumerate() {
        let start = HEADER_WORDS + index * RECORD_WORDS;
        encode_record(record, &mut words[start..start + RECORD_WORDS]);
    }

    Ok(count)
}

pub fn decode(words: &[u64], snapshot: &mut DiskSnapshot) -> Result<(), Error> {
    *snapshot = DiskSnapshot::default();
    if words.len() < HEADER_WORDS {
        return Err(Error::InvalidSnapshot);
    }

    if words[0] != MAGIC {
        return Err(Error::InvalidSnapshot);
    }

    let count = usize::try_from(words[3]).map_err(|_| Error::InvalidSnapshot)?;
    if count > DISKS_MAX {
        return Err(Error::InvalidSnapshot);
    }

    if words.len() != HEADER_WORDS + count * RECORD_WORDS {
        return Err(Error::InvalidSnapshot);
    }

    snapshot.source_continuous_ns = words[1];
    snapshot.state = match words[2] {
        0 => ScanState::Complete,
        1 => ScanState::Failed,
        2 => ScanState::Exhausted,
        3 => ScanState::Partial,
        _ => return Err(Error::InvalidSnapshot),
    };
    snapshot.count = count;
    for (index, record) in snapshot.records[..count].iter_mut().enumerate() {
        let start = HEADER_WORDS + index * RECORD_WORDS;
        decode_record(&words[start..start + RECORD_WORDS], record)?;
    }

    validate(snapshot)
}

pub(crate) fn validate(snapshot: &DiskSnapshot) -> Result<(), Error> {
    if snapshot.count > DISKS_MAX {
        return Err(Error::InvalidSnapshot);
    }

    if snapshot.state == ScanState::Failed {
        if snapshot.count != 0 {
            return Err(Error::InvalidSnapshot);
        }
    }

    for (index, record) in snapshot.records[..snapshot.count].iter().enumerate() {
        validate_record(record)?;
        if snapshot.state == ScanState::Complete {
            if record.failed {
                return Err(Error::InvalidSnapshot);
            }
        }

        for previous in &snapshot.records[..index] {
            if previous.driver_id == record.driver_id {
                return Err(Error::InvalidSnapshot);
            }

            if previous.media_id == record.media_id {
                return Err(Error::InvalidSnapshot);
            }
        }
    }
    for record in &snapshot.records[snapshot.count..] {
        if *record != DiskReading::default() {
            return Err(Error::InvalidSnapshot);
        }
    }

    Ok(())
}

fn validate_record(record: &DiskReading) -> Result<(), Error> {
    if record.driver_id == 0 {
        return Err(Error::InvalidSnapshot);
    }

    if record.media_id == 0 {
        return Err(Error::InvalidSnapshot);
    }

    let Some(end) = record.name.iter().position(|byte| *byte == 0) else {
        return Err(Error::InvalidSnapshot);
    };

    if end == 0 {
        return Err(Error::InvalidSnapshot);
    }

    if !record.name[end..].iter().all(|byte| *byte == 0) {
        return Err(Error::InvalidSnapshot);
    }

    if record.name[..end]
        .iter()
        .any(|byte| !byte.is_ascii_graphic())
    {
        return Err(Error::InvalidSnapshot);
    }

    if record.failed {
        if counters(record).iter().any(Option::is_some) {
            return Err(Error::InvalidSnapshot);
        }
    }

    Ok(())
}

fn encode_record(record: &DiskReading, words: &mut [u64]) {
    words[0] = record.driver_id;
    words[1] = record.media_id;
    words[2] = if record.failed { FAILED } else { 0 };
    for index in 0..4 {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(&record.name[index * 8..index * 8 + 8]);
        words[index + 3] = u64::from_le_bytes(bytes);
    }
    for (index, value) in counters(record).iter().enumerate() {
        if let Some(value) = value {
            words[2] |= 1 << index;
            words[index + 7] = *value;
        }
    }
}

fn decode_record(words: &[u64], record: &mut DiskReading) -> Result<(), Error> {
    if words[2] & !FLAGS != 0 {
        return Err(Error::InvalidSnapshot);
    }

    for index in 0..8 {
        if words[2] & (1 << index) == 0 {
            if words[index + 7] != 0 {
                return Err(Error::InvalidSnapshot);
            }
        }
    }

    let field = |index: usize| (words[2] & (1 << index) != 0).then_some(words[index + 7]);
    *record = DiskReading {
        driver_id: words[0],
        media_id: words[1],
        failed: words[2] & FAILED != 0,
        read_bytes: field(0),
        write_bytes: field(1),
        read_operations: field(2),
        write_operations: field(3),
        read_errors: field(4),
        write_errors: field(5),
        read_time_ns: field(6),
        write_time_ns: field(7),
        ..DiskReading::default()
    };
    for index in 0..4 {
        record.name[index * 8..index * 8 + 8].copy_from_slice(&words[index + 3].to_le_bytes());
    }

    Ok(())
}

fn counters(record: &DiskReading) -> [Option<u64>; 8] {
    [
        record.read_bytes,
        record.write_bytes,
        record.read_operations,
        record.write_operations,
        record.read_errors,
        record.write_errors,
        record.read_time_ns,
        record.write_time_ns,
    ]
}
