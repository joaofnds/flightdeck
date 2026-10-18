//! Fixed disk records follow the exact complete Battery extent.

use std::io::ErrorKind;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::disks::{DiskSnapshot, encoding};
use crate::u64_from_usize;

pub(super) const MAGIC: u64 = u64::from_be_bytes(*b"FDDKTL01");
pub(super) const HEADER_WORDS: usize = 4;
const VERSION: u64 = 1;

#[derive(Debug, Clone)]
pub(super) struct DisksLayout {
    pub(super) start: usize,
    slot_count: usize,
    end: usize,
}

pub(super) struct DisksExtents {
    pub(super) start_words: usize,
    pub(super) size_bytes: usize,
}

impl DisksLayout {
    pub(super) fn plan(start: usize, slot_count: u64) -> Option<Self> {
        if slot_count == 0 {
            return None;
        }

        let slot_count = usize::try_from(slot_count).ok()?;
        let end = start
            .checked_add(HEADER_WORDS)?
            .checked_add(slot_count.checked_mul(encoding::WORDS_MAX)?)?;
        i64::try_from(end.checked_mul(size_of::<u64>())?).ok()?;

        Some(Self {
            start,
            slot_count,
            end,
        })
    }

    pub(super) fn word_count(&self) -> usize {
        self.end
    }

    pub(super) fn record(&self, publication: u64) -> Range<usize> {
        assert!(publication > 0);

        let index = (publication - 1) % u64_from_usize(self.slot_count);
        let index = match usize::try_from(index) {
            Ok(index) => index,
            Err(error) => unreachable!("{error}"),
        };
        let start = self.start + HEADER_WORDS + index * encoding::WORDS_MAX;

        start..start + encoding::WORDS_MAX
    }

    pub(super) fn store_header(&self, words: &[AtomicU64]) {
        let values = [
            MAGIC,
            VERSION,
            u64_from_usize(self.slot_count),
            u64_from_usize(encoding::WORDS_MAX),
        ];
        for (index, value) in values.iter().enumerate() {
            words[self.start + index].store(*value, Ordering::Relaxed);
        }
    }

    pub(super) fn admit(
        header: &[AtomicU64],
        slot_count: u64,
        extents: &DisksExtents,
    ) -> Option<Self> {
        if header.len() < HEADER_WORDS {
            return None;
        }

        let expected = [
            MAGIC,
            VERSION,
            slot_count,
            u64_from_usize(encoding::WORDS_MAX),
        ];
        for (word, expected) in header.iter().zip(expected) {
            if word.load(Ordering::Relaxed) != expected {
                return None;
            }
        }

        let layout = Self::plan(extents.start_words, slot_count)?;
        if layout.end.checked_mul(size_of::<u64>())? > extents.size_bytes {
            return None;
        }

        Some(layout)
    }
}

pub(super) fn store(words: &[AtomicU64], snapshot: Option<&DiskSnapshot>) {
    assert_eq!(words.len(), encoding::WORDS_MAX);
    let mut encoded = [0; encoding::WORDS_MAX];
    if let Some(snapshot) = snapshot {
        match encoding::encode(snapshot, &mut encoded) {
            Ok(count) => assert!(count <= encoding::WORDS_MAX),
            Err(error) => unreachable!("validated disk snapshot changed: {error:?}"),
        }
    }
    for (word, value) in words.iter().zip(encoded) {
        word.store(value, Ordering::Relaxed);
    }
}

pub(super) fn load(
    words: &[AtomicU64],
    snapshot: &mut Option<DiskSnapshot>,
) -> Result<(), ErrorKind> {
    assert_eq!(words.len(), encoding::WORDS_MAX);
    *snapshot = None;
    let encoded: [u64; encoding::WORDS_MAX] =
        std::array::from_fn(|index| words[index].load(Ordering::Relaxed));
    if encoded.iter().all(|word| *word == 0) {
        return Ok(());
    }

    let count = usize::try_from(encoded[3]).map_err(|_| ErrorKind::InvalidData)?;
    if count > crate::disks::DISKS_MAX {
        return Err(ErrorKind::InvalidData);
    }

    let used = encoding::HEADER_WORDS + count * encoding::RECORD_WORDS;
    if encoded[used..].iter().any(|word| *word != 0) {
        return Err(ErrorKind::InvalidData);
    }

    *snapshot = Some(DiskSnapshot::default());
    if let Some(snapshot) = snapshot.as_mut() {
        encoding::decode(&encoded[..used], snapshot).map_err(|_| ErrorKind::InvalidData)?;
    } else {
        unreachable!("disk snapshot was initialized in place");
    }

    Ok(())
}
