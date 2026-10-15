//! Where each part of a ring lies, in words from its start.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use super::Shape;

/// Marks the start of a ring, so a reader never takes another object for one.
pub(super) const MAGIC: u64 = u64::from_be_bytes(*b"FLTDRING");
/// The layout this code writes and reads, which a change to the layout raises.
pub(super) const LAYOUT_VERSION: u64 = 1;

/// The words of the header, which starts the ring.
pub(super) const HEADER_MAGIC: usize = 0;
pub(super) const HEADER_LAYOUT_VERSION: usize = 1;
pub(super) const HEADER_GENERATION: usize = 2;
pub(super) const HEADER_SLOT_COUNT: usize = 3;
pub(super) const HEADER_SLOT_WORDS: usize = 4;
/// The ring's first publication, one past the newest of the ring before it.
pub(super) const HEADER_FIRST_PUBLICATION: usize = 5;
/// The newest publication the writer finished, which before its first is the previous ring's.
pub(super) const HEADER_NEWEST_PUBLICATION: usize = 6;
pub(super) const HEADER_WORDS: usize = 8;

/// The words of a slot. The sequence word holds twice the publication the slot holds once
/// the writer finished it, and one less while the writer writes it.
pub(super) const SLOT_SEQUENCE: usize = 0;
pub(super) const SLOT_RUN_TICK: usize = 1;
pub(super) const SLOT_WORDS: usize = 2;

/// Where each part of a ring lies, in words from its start.
#[derive(Debug, Clone, Copy)]
pub(super) struct Layout {
    shape: Shape,
}

impl Layout {
    pub(super) fn new(shape: Shape) -> Self {
        assert!(shape.slot_count > 0);

        Self { shape }
    }

    pub(super) fn slot_count(self) -> usize {
        match usize::try_from(self.shape.slot_count) {
            Ok(count) => count,
            Err(error) => unreachable!("{error}"),
        }
    }

    pub(super) fn word_count(self) -> usize {
        HEADER_WORDS + self.slot_count() * SLOT_WORDS
    }

    pub(super) fn size_bytes(self) -> usize {
        self.word_count() * size_of::<u64>()
    }

    /// The oldest publication the slots hold once `newest` is published, where every
    /// publication numbered from 1 went into them.
    pub(super) fn oldest_held(self, newest: u64) -> u64 {
        newest.saturating_sub(self.shape.slot_count) + 1
    }

    /// The first word of the slot that holds `publication`.
    pub(super) fn slot_start(self, publication: u64) -> usize {
        assert!(publication > 0);

        let index = (publication - 1) % self.shape.slot_count;
        match usize::try_from(index) {
            Ok(index) => HEADER_WORDS + index * SLOT_WORDS,
            Err(error) => unreachable!("{error}"),
        }
    }
}

pub(super) fn open_layout(header: &[AtomicU64], size_bytes: usize) -> io::Result<Layout> {
    let load = |index: usize| header[index].load(Ordering::Relaxed);
    if load(HEADER_MAGIC) != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a ring"));
    }
    if load(HEADER_LAYOUT_VERSION) != LAYOUT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a ring of layout {}", load(HEADER_LAYOUT_VERSION)),
        ));
    }

    let layout = Layout::new(Shape {
        slot_count: load(HEADER_SLOT_COUNT),
    });
    if layout.size_bytes() > size_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a ring of {size_bytes} bytes, short of its layout"),
        ));
    }
    Ok(layout)
}
