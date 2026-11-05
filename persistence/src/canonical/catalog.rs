//! The saved readings of a recording, found through its canonical namespaces, so that what a
//! lookup reads follows what it asks for and never how much the recording holds.
use super::metadata::{Body, CanonicalMetadata};
use super::tree::{
    CanonicalCursor, CanonicalKey, Direction, Namespace, ReadStep, ReadWork, SeekRange, invalid,
};
use crate::Manifest;
use crate::resources::ByteBudget;
use collector::recording::bootstrap::{Classification, classify};
use collector::recording::private::PrivateDirectory;
use collector::ring::{Identity, RunIdentity};
use std::io;
use std::path::Path;
use std::sync::Arc;

/// The readings of one page, the most one pass over the namespace returns.
pub const PAGE_READINGS_MAX: usize = 128;

/// The steps one lookup takes. Each returns a reading or spends a pass of 4,096 nodes, and a route
/// is at most 124 nodes deep, so a namespace that needs more than this is not one the writer made.
const LOOKUP_STEPS_MAX: usize = 1024;
const _: () = assert!(PAGE_READINGS_MAX < LOOKUP_STEPS_MAX);

const NS_PER_HOUR: u64 = 3_600_000_000_000;

const KNOWN_SOURCE_OFFSET: usize = 12;
const KNOWN_PADDING_OFFSET: usize = KNOWN_SOURCE_OFFSET + SOURCE_BYTES;
const SOURCE_BYTES: usize = 40;
const _: () = assert!(KNOWN_PADDING_OFFSET + 8 == 60);

/// A recording's saved readings. It reads what its lookups name, and it never creates, locks or
/// repairs anything.
pub struct Catalog {
    root: PrivateDirectory,
    recording: Identity,
    budget: Arc<ByteBudget>,
}

/// One saved reading whose clock was known, with its full identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownReading {
    pub wall_ns: u64,
    pub run: RunIdentity,
    pub publication: u64,
}

/// A bounded part of a known range, in clock order and then by run, boot and publication.
#[derive(Debug)]
pub struct KnownPage {
    pub readings: Vec<KnownReading>,
    /// The reading the next page continues after, none at the end of the range.
    pub next: Option<KnownReading>,
}

impl KnownReading {
    /// The name this reading has in the known namespace.
    pub(crate) fn key(&self) -> io::Result<CanonicalKey> {
        let mut bytes = [0; 60];
        let hour = u32::try_from(self.wall_ns / NS_PER_HOUR).map_err(|_| invalid())?;
        bytes[..4].copy_from_slice(&hour.to_be_bytes());
        bytes[4..12].copy_from_slice(&(self.wall_ns % NS_PER_HOUR).to_be_bytes());
        let source = CanonicalKey::source(Namespace::Ordinary, &self.run, self.publication)?;
        bytes[KNOWN_SOURCE_OFFSET..KNOWN_PADDING_OFFSET]
            .copy_from_slice(&source.bytes[..SOURCE_BYTES]);

        CanonicalKey::from_bytes(Namespace::Known, &bytes)
    }

    fn from_key(recording: Identity, key: &CanonicalKey) -> io::Result<Self> {
        let hour = u64::from(u32::from_be_bytes(array(&key.bytes[..4])?));
        let within = u64::from_be_bytes(array(&key.bytes[4..12])?);
        if within >= NS_PER_HOUR || key.bytes[KNOWN_PADDING_OFFSET..60] != [0; 8] {
            return Err(invalid());
        }
        let wall_ns = hour
            .checked_mul(NS_PER_HOUR)
            .and_then(|whole| whole.checked_add(within))
            .ok_or_else(invalid)?;
        let source = &key.bytes[KNOWN_SOURCE_OFFSET..KNOWN_PADDING_OFFSET];

        Ok(Self {
            wall_ns,
            run: RunIdentity {
                recording,
                run: identity(&source[..16])?,
                boot: identity(&source[16..32])?,
            },
            publication: u64::from_be_bytes(array(&source[32..40])?),
        })
    }
}

impl Catalog {
    /// Opens the recording at `path`. A recording of another format is `Unsupported`.
    pub fn open(path: &Path, budget: &Arc<ByteBudget>) -> io::Result<Self> {
        let root = PrivateDirectory::open(path)?;
        let Classification::Initialized(recording) = classify(&root, &mut [0; 4097])? else {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        };

        Ok(Self {
            root,
            recording,
            budget: Arc::clone(budget),
        })
    }

    #[must_use]
    pub fn recording(&self) -> Identity {
        self.recording
    }

    /// The saved reading with the newest clock, or none when no reading has one.
    pub fn newest_known(&self, work: &mut ReadWork) -> io::Result<Option<KnownReading>> {
        self.known_end(Direction::Reverse, work)
    }

    /// The saved reading with the oldest clock, or none when no reading has one.
    pub fn oldest_known(&self, work: &mut ReadWork) -> io::Result<Option<KnownReading>> {
        self.known_end(Direction::Forward, work)
    }

    fn known_end(
        &self,
        direction: Direction,
        work: &mut ReadWork,
    ) -> io::Result<Option<KnownReading>> {
        let range = SeekRange {
            first: CanonicalKey::from_bytes(Namespace::Known, &[0; 60])?,
            last: CanonicalKey::from_bytes(Namespace::Known, &[0xff; 60])?,
            direction,
        };
        let mut cursor = self.known_cursor(range)?;
        let found = catalog_found(&mut cursor)?;
        work.add(cursor.work());

        found
            .map(|key| KnownReading::from_key(self.recording, &key))
            .transpose()
    }

    /// The readings with a clock from `first_ns` up to but not including `end_ns`, after the
    /// reading a previous page's `next` named, if any.
    pub fn known_page(
        &self,
        first_ns: u64,
        end_ns: u64,
        after: Option<KnownReading>,
        work: &mut ReadWork,
    ) -> io::Result<KnownPage> {
        let mut page = KnownPage {
            readings: Vec::with_capacity(PAGE_READINGS_MAX),
            next: None,
        };
        let Some(range) = known_page_range(first_ns, end_ns, after)? else {
            return Ok(page);
        };
        let mut cursor = self.known_cursor(range)?;
        for _reading in 0..PAGE_READINGS_MAX {
            let Some(key) = catalog_found(&mut cursor)? else {
                work.add(cursor.work());
                return Ok(page);
            };
            page.readings
                .push(KnownReading::from_key(self.recording, &key)?);
        }
        page.next = catalog_found(&mut cursor)?.and(page.readings.last().copied());
        work.add(cursor.work());

        Ok(page)
    }

    fn known_cursor(&self, range: SeekRange) -> io::Result<CanonicalCursor<'_, 'static>> {
        CanonicalCursor::begin_owned(
            &self.root,
            self.recording,
            Namespace::Known,
            range,
            &self.budget,
        )
    }

    /// The manifest of the saved window of `run` that holds `publication`, or none where the
    /// recording saved no minute holding it. It reads that window's proof and no other, and it
    /// leaves the window's files to whoever reads them.
    pub fn window_holding(
        &self,
        run: &RunIdentity,
        publication: u64,
        work: &mut ReadWork,
    ) -> io::Result<Option<Manifest>> {
        if run.recording != self.recording {
            return Err(invalid());
        }
        let range = SeekRange {
            first: CanonicalKey::source(Namespace::Ordinary, run, 0)?,
            last: CanonicalKey::source(Namespace::Ordinary, run, publication)?,
            direction: Direction::Reverse,
        };
        let mut cursor = CanonicalCursor::begin_owned(
            &self.root,
            self.recording,
            Namespace::Ordinary,
            range,
            &self.budget,
        )?;
        let manifest = window_holding_gate(&mut cursor, run);
        work.add(cursor.work());

        Ok(manifest?.filter(|manifest| {
            (manifest.publication_first..manifest.publication_end).contains(&publication)
        }))
    }
}

/// The manifest behind the first gate `cursor` finds, none where it finds no gate or a gate that
/// owns only a loss.
fn window_holding_gate(
    cursor: &mut CanonicalCursor<'_, 'static>,
    run: &RunIdentity,
) -> io::Result<Option<Manifest>> {
    let Some(key) = catalog_found(cursor)? else {
        return Ok(None);
    };
    let mut gate = cursor.open_gate(&key)?;
    let metadata = CanonicalMetadata::read(&mut gate)?;
    if metadata.run != *run {
        return Err(invalid());
    }
    metadata.validate_key(Namespace::Ordinary, &key)?;
    cursor.check_snapshot()?;

    match metadata.body {
        Body::Ordinary(manifest) => Ok(Some(manifest)),
        Body::LossOnly | Body::Events(_) => Ok(None),
    }
}

/// The next key `cursor` finds, none at the end of its range. Reaching the work limit is not the
/// end of a range, so a namespace that keeps asking for more is refused.
fn catalog_found(cursor: &mut CanonicalCursor<'_, 'static>) -> io::Result<Option<CanonicalKey>> {
    for _step in 0..LOOKUP_STEPS_MAX {
        match cursor.step()? {
            ReadStep::Found(key) => return Ok(Some(key)),
            ReadStep::End => return Ok(None),
            ReadStep::Continue => {}
        }
    }

    Err(invalid())
}

/// The namespace range of a page, none when nothing can follow `after` or the range is empty.
fn known_page_range(
    first_ns: u64,
    end_ns: u64,
    after: Option<KnownReading>,
) -> io::Result<Option<SeekRange>> {
    let Some(last_ns) = end_ns.checked_sub(1).filter(|last| first_ns <= *last) else {
        return Ok(None);
    };
    let first = match after {
        Some(reading) => match known_page_range_successor(&reading.key()?)? {
            Some(key) => key,
            None => return Ok(None),
        },
        None => known_page_range_clock(first_ns, 0)?,
    };
    let last = known_page_range_clock(last_ns, 0xff)?;
    if first > last {
        return Ok(None);
    }

    Ok(Some(SeekRange {
        first,
        last,
        direction: Direction::Forward,
    }))
}

/// The key at a clock whose run, boot and publication bytes are all `fill`.
fn known_page_range_clock(wall_ns: u64, fill: u8) -> io::Result<CanonicalKey> {
    let mut bytes = [fill; 60];
    let hour = u32::try_from(wall_ns / NS_PER_HOUR).map_err(|_| invalid())?;
    bytes[..4].copy_from_slice(&hour.to_be_bytes());
    bytes[4..12].copy_from_slice(&(wall_ns % NS_PER_HOUR).to_be_bytes());

    CanonicalKey::from_bytes(Namespace::Known, &bytes)
}

/// The key straight after `key`, none after the largest.
fn known_page_range_successor(key: &CanonicalKey) -> io::Result<Option<CanonicalKey>> {
    let mut bytes = [0; 60];
    bytes.copy_from_slice(&key.bytes[..60]);
    for index in (0..60).rev() {
        if bytes[index] != 0xff {
            bytes[index] += 1;
            return CanonicalKey::from_bytes(Namespace::Known, &bytes).map(Some);
        }
        bytes[index] = 0;
    }

    Ok(None)
}

fn identity(bytes: &[u8]) -> io::Result<Identity> {
    Ok(Identity([
        u64::from_be_bytes(array(&bytes[..8])?),
        u64::from_be_bytes(array(&bytes[8..16])?),
    ]))
}

fn array<const LENGTH: usize>(bytes: &[u8]) -> io::Result<[u8; LENGTH]> {
    bytes.try_into().map_err(|_| invalid())
}
