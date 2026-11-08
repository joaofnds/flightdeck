//! The saved readings of a recording, found through its canonical namespaces, so that what a
//! lookup reads follows what it asks for and never how much the recording holds.
use super::metadata::{Body, CanonicalMetadata};
use super::tree::{
    CanonicalCursor, CanonicalKey, Direction, Namespace, ReadStep, ReadWork, SeekRange, invalid,
};
use crate::Manifest;
use crate::event_recovery::Recovery;
use crate::resources::ByteBudget;
use collector::recording::bootstrap::{Classification, classify};
use collector::recording::private::PrivateDirectory;
use collector::ring::{Identity, RunIdentity};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(feature = "fixtures")]
pub mod fixture;

/// The readings of one page, the most one pass over the namespace returns.
pub const PAGE_READINGS_MAX: usize = 128;

/// The steps one lookup takes. Each returns a reading or spends a pass of 4,096 nodes, and a route
/// is at most 124 nodes deep, so a namespace that needs more than this is not one the writer made.
const LOOKUP_STEPS_MAX: usize = 1024;
const _: () = assert!(PAGE_READINGS_MAX < LOOKUP_STEPS_MAX);

/// The windows one clock range may touch. A window holds at least one reading of its range, so this
/// is more than an hour of a window for every second of it from a run started every minute.
pub const RANGE_WINDOWS_MAX: usize = 4096;
/// The pages one clock range reads, more than a second's readings of every window it may touch.
const RANGE_PAGES_MAX: usize = 1 << 12;
/// How many names one search for a damaged window's readings walks.
const READING_NAMES_MAX: usize = 1 << 16;

const NAMESPACE_STATE_BYTES: usize = 256;
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

/// One saved reading whose clock was unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownReading {
    pub run: RunIdentity,
    pub publication: u64,
}

/// A bounded part of the readings whose clock was unknown, by run, boot and publication.
#[derive(Debug)]
pub struct UnknownPage {
    pub readings: Vec<UnknownReading>,
    /// The reading the next page continues after, none at the end.
    pub next: Option<UnknownReading>,
}

/// A saved object of events that belongs to no minute: what a run's source held past its last
/// minute, or rows saved when the minute they were for could not be. Neither completes a minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventObject {
    pub kind: EventObjectKind,
    /// The run it belongs to, the events it covers, the rows it holds of them and what was lost.
    pub recovery: Recovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventObjectKind {
    Terminal,
    Fragment,
}

/// A bounded part of a recording's event objects, the terminals and then the fragments.
#[derive(Debug)]
pub struct EventObjectPage {
    pub objects: Vec<EventObject>,
    /// Where the next page continues, none once every object has been listed. A page may be empty
    /// and still have a next one.
    pub next: Option<EventObjectPosition>,
}

/// Where a page of event objects ended.
#[derive(Clone, Copy, Debug)]
pub struct EventObjectPosition {
    namespace: Namespace,
    after: Option<CanonicalKey>,
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

impl UnknownReading {
    /// The name this reading has in the unknown namespace.
    pub(crate) fn key(&self) -> io::Result<CanonicalKey> {
        let mut bytes = [0; 48];
        let source = CanonicalKey::source(Namespace::Ordinary, &self.run, self.publication)?;
        bytes[..SOURCE_BYTES].copy_from_slice(&source.bytes[..SOURCE_BYTES]);

        CanonicalKey::from_bytes(Namespace::Unknown, &bytes)
    }

    fn from_key(recording: Identity, key: &CanonicalKey) -> io::Result<Self> {
        if key.bytes[SOURCE_BYTES..48] != [0; 8] {
            return Err(invalid());
        }

        Ok(Self {
            run: RunIdentity {
                recording,
                run: identity(&key.bytes[..16])?,
                boot: identity(&key.bytes[16..32])?,
            },
            publication: u64::from_be_bytes(array(&key.bytes[32..40])?),
        })
    }
}

impl Catalog {
    /// Opens the recording at `path`. A root no writer finished setting up holds no recording yet,
    /// which is `NotFound`, one of another format is `Unsupported`, and a damaged one is
    /// `InvalidData`.
    pub fn open(path: &Path, budget: &Arc<ByteBudget>) -> io::Result<Self> {
        let root = PrivateDirectory::open(path)?;
        let recording = match classify(&root, &mut [0; 4097])? {
            Classification::Initialized(recording) => recording,
            Classification::Corrupt => return Err(io::Error::from(io::ErrorKind::InvalidData)),
            Classification::Fresh | Classification::Initializing(_) => {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            Classification::Unsupported => {
                return Err(io::Error::from(io::ErrorKind::Unsupported));
            }
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
        // One key past the page says whether another page follows it.
        let mut keys = Vec::with_capacity(PAGE_READINGS_MAX + 1);
        catalog_found_keys(&mut cursor, &mut keys, PAGE_READINGS_MAX + 1)?;
        for key in keys.iter().take(PAGE_READINGS_MAX) {
            page.readings
                .push(KnownReading::from_key(self.recording, key)?);
        }
        if keys.len() > PAGE_READINGS_MAX {
            page.next = page.readings.last().copied();
        }
        work.add(cursor.work());

        Ok(page)
    }

    /// The windows holding a reading in `second`, counted from the epoch. A second no clock can
    /// hold is refused, as no window could hold a reading in it.
    pub fn windows_at_second(
        &self,
        second: u64,
        work: &mut ReadWork,
    ) -> crate::Result<Vec<Manifest>> {
        const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
        let first_ns = second
            .checked_mul(NANOSECONDS_PER_SECOND)
            .ok_or(crate::Error::Bound("second"))?;
        let end_ns = first_ns
            .checked_add(NANOSECONDS_PER_SECOND)
            .ok_or(crate::Error::Bound("second"))?;

        self.windows_in(first_ns, end_ns, work)
    }

    /// The windows holding a reading with a clock from `first_ns` up to but not including `end_ns`,
    /// each once, in the order of their first reading in the range. It reads one proof for each,
    /// so what it costs follows the range and never what the recording holds around it.
    pub fn windows_in(
        &self,
        first_ns: u64,
        end_ns: u64,
        work: &mut ReadWork,
    ) -> crate::Result<Vec<Manifest>> {
        let mut windows = Vec::new();
        let mut after = None;
        for _page in 0..RANGE_PAGES_MAX {
            let state = self.namespace_state()?;
            let page = self.known_page(first_ns, end_ns, after, work)?;
            for reading in &page.readings {
                if windows_in_listed(&windows, reading) {
                    continue;
                }
                if windows.len() == RANGE_WINDOWS_MAX {
                    return Err(crate::Error::Bound("catalog range windows"));
                }
                windows.push(self.window_of_listed(
                    &reading.run,
                    reading.publication,
                    &state,
                    work,
                )?);
            }
            let Some(next) = page.next else {
                return Ok(windows);
            };
            after = Some(next);
        }

        Err(crate::Error::Bound("catalog range pages"))
    }

    /// The newest saved window of `run` that starts at or before `publication`, none where the run
    /// saved no such window. It reads one proof, and one more for each gate that owns only a loss.
    pub fn window_at_or_before(
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

        manifest
    }

    /// How many readings have no known clock, none where one page does not reach their end, so a
    /// partial count is never taken for the whole.
    pub fn unknown_count_within_page(&self, work: &mut ReadWork) -> io::Result<Option<u64>> {
        let range = SeekRange {
            first: CanonicalKey::from_bytes(Namespace::Unknown, &[0; 48])?,
            last: CanonicalKey::from_bytes(Namespace::Unknown, &[0xff; 48])?,
            direction: Direction::Forward,
        };
        let mut cursor = CanonicalCursor::begin_owned(
            &self.root,
            self.recording,
            Namespace::Unknown,
            range,
            &self.budget,
        )?;
        for count in 0..=PAGE_READINGS_MAX {
            if catalog_found(&mut cursor)?.is_none() {
                work.add(cursor.work());
                return Ok(Some(u64::try_from(count).map_err(|_| invalid())?));
            }
        }
        work.add(cursor.work());

        Ok(None)
    }

    /// The readings with no known clock, after the reading a previous page's `next` named, if any.
    pub fn unknown_page(
        &self,
        after: Option<UnknownReading>,
        work: &mut ReadWork,
    ) -> io::Result<UnknownPage> {
        let mut page = UnknownPage {
            readings: Vec::with_capacity(PAGE_READINGS_MAX),
            next: None,
        };
        let first = match after {
            Some(after) => match key_successor(Namespace::Unknown, &after.key()?)? {
                Some(first) => first,
                None => return Ok(page),
            },
            None => CanonicalKey::from_bytes(Namespace::Unknown, &[0; 48])?,
        };
        let mut cursor = CanonicalCursor::begin_owned(
            &self.root,
            self.recording,
            Namespace::Unknown,
            SeekRange {
                first,
                last: CanonicalKey::from_bytes(Namespace::Unknown, &[0xff; 48])?,
                direction: Direction::Forward,
            },
            &self.budget,
        )?;
        for _reading in 0..PAGE_READINGS_MAX {
            let Some(key) = catalog_found(&mut cursor)? else {
                work.add(cursor.work());
                return Ok(page);
            };
            page.readings
                .push(UnknownReading::from_key(self.recording, &key)?);
        }
        page.next = catalog_found(&mut cursor)?.and(page.readings.last().copied());
        work.add(cursor.work());

        Ok(page)
    }

    /// The windows none of whose readings has a clock, which no clock range can reach. A reading
    /// with a clock leaves its window to the clock range, so the two lists never share a window. It
    /// reads one proof for each window holding such a reading, so what it costs follows those
    /// windows and never what the recording holds around them.
    pub fn windows_without_clock(&self, work: &mut ReadWork) -> crate::Result<Vec<Manifest>> {
        let mut windows = Vec::new();
        // The readings of one window lie together in this namespace, so one count at a time does.
        let mut counted: Option<(Manifest, u64)> = None;
        let mut after = None;
        for _page in 0..RANGE_PAGES_MAX {
            let state = self.namespace_state()?;
            let page = self.unknown_page(after, work)?;
            for reading in &page.readings {
                if let Some((window, readings)) = &mut counted
                    && window_holds(window, &reading.run, reading.publication)
                {
                    *readings += 1;
                    continue;
                }
                windows_without_clock_keep(&mut windows, counted.take())?;
                let window =
                    self.window_of_listed(&reading.run, reading.publication, &state, work)?;
                counted = Some((window, 1));
            }
            let Some(next) = page.next else {
                windows_without_clock_keep(&mut windows, counted.take())?;
                return Ok(windows);
            };
            after = Some(next);
        }

        Err(crate::Error::Bound("catalog unknown pages"))
    }

    /// The windows of `run`, newest first.
    pub fn windows_of(&self, run: &RunIdentity, work: &mut ReadWork) -> io::Result<Vec<Manifest>> {
        let mut windows = Vec::new();
        let mut publication = u64::MAX;
        for _window in 0..RANGE_WINDOWS_MAX {
            let Some(found) = self.window_at_or_before(run, publication, work)? else {
                return Ok(windows);
            };
            publication = found.publication_first - 1;
            windows.push(found);
        }

        Err(invalid())
    }

    /// The window of a reading a page listed, with `state` the namespace's state from before the
    /// page. A reading's window outlives its links, so a reading with no window is damage, unless
    /// physical removal ran since the page, and then the list has to be read again.
    pub(crate) fn window_of_listed(
        &self,
        run: &RunIdentity,
        publication: u64,
        state: &[u8; NAMESPACE_STATE_BYTES],
        work: &mut ReadWork,
    ) -> io::Result<Manifest> {
        if let Some(window) = self.window_holding(run, publication, work)? {
            return Ok(window);
        }
        if self.namespace_state()? == *state {
            return Err(invalid());
        }

        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }

    pub(crate) fn namespace_state(&self) -> io::Result<[u8; NAMESPACE_STATE_BYTES]> {
        let mut state = [0; NAMESPACE_STATE_BYTES];
        self.root
            .open_file("namespace-state")?
            .read_exact(&mut state)?;

        Ok(state)
    }

    /// What `run` saved of its events on ending and where under the root its batches lie, none
    /// until it has.
    pub(crate) fn terminal_of(
        &self,
        run: &RunIdentity,
        work: &mut ReadWork,
    ) -> io::Result<Option<(crate::event_recovery::Manifest, PathBuf)>> {
        let key = CanonicalKey::source(Namespace::Terminal, run, 0)?;
        let mut cursor = CanonicalCursor::begin_owned(
            &self.root,
            self.recording,
            Namespace::Terminal,
            SeekRange {
                first: key,
                last: key,
                direction: Direction::Forward,
            },
            &self.budget,
        )?;
        let found = catalog_found(&mut cursor)?;
        let Some(found) = found else {
            work.add(cursor.work());
            return Ok(None);
        };
        let mut gate = cursor.open_gate(&found)?;
        let metadata = read_gate(&mut gate)?;
        metadata.validate_key(Namespace::Terminal, &found)?;
        cursor.check_snapshot()?;
        work.add(cursor.work());
        let directory = Path::new(metadata.base.name())
            .join(collector::recording::identity_text(metadata.transaction).to_string())
            .join(metadata.child.name());
        let manifest = match metadata.body {
            Body::Events(manifest) => manifest,
            Body::LossOnly => crate::event_recovery::Manifest {
                recovery: Recovery {
                    run: metadata.run,
                    events_first: metadata.loss.inherited.event_start,
                    events_end: metadata.loss.event_end,
                    rows: 0,
                    losses: metadata.loss.totals()?,
                },
                identities: false,
                batches: Vec::new(),
            },
            Body::Ordinary(_) => return Err(invalid()),
        };

        Ok(Some((manifest, directory)))
    }

    /// Up to `objects_max` event objects, at most a page's worth, after the position a previous
    /// page named. It reads one proof for each, and it needs no window and no registry to reach
    /// them.
    pub fn event_objects_page(
        &self,
        after: Option<&EventObjectPosition>,
        objects_max: usize,
        work: &mut ReadWork,
    ) -> io::Result<EventObjectPage> {
        assert!(objects_max > 0);
        assert!(objects_max <= PAGE_READINGS_MAX);
        let position = after.copied().unwrap_or(EventObjectPosition {
            namespace: Namespace::Terminal,
            after: None,
        });
        let mut page = EventObjectPage {
            objects: Vec::with_capacity(objects_max),
            next: None,
        };
        let Some(range) = event_objects_range(&position)? else {
            page.next = event_objects_next_namespace(position.namespace);
            return Ok(page);
        };
        let mut cursor = CanonicalCursor::begin_owned(
            &self.root,
            self.recording,
            position.namespace,
            range,
            &self.budget,
        )?;
        for _object in 0..objects_max {
            let Some(key) = catalog_found(&mut cursor)? else {
                work.add(cursor.work());
                page.next = event_objects_next_namespace(position.namespace);
                return Ok(page);
            };
            page.objects
                .push(event_object(&mut cursor, position.namespace, &key)?);
            page.next = Some(EventObjectPosition {
                namespace: position.namespace,
                after: Some(key),
            });
        }
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
        let manifest = self.window_at_or_before(run, publication, work)?;

        Ok(manifest.filter(|manifest| {
            (manifest.publication_first..manifest.publication_end).contains(&publication)
        }))
    }
}

/// The first window `run` saved from `publication` on, none where it saved no later window.
pub(crate) fn window_at_or_after(
    root: &PrivateDirectory,
    recording: Identity,
    run: &RunIdentity,
    publication: u64,
    budget: &Arc<ByteBudget>,
) -> io::Result<Option<Manifest>> {
    let range = SeekRange {
        first: CanonicalKey::source(Namespace::Ordinary, run, publication)?,
        last: CanonicalKey::source(Namespace::Ordinary, run, u64::MAX)?,
        direction: Direction::Forward,
    };
    let mut cursor =
        CanonicalCursor::begin_owned(root, recording, Namespace::Ordinary, range, budget)?;

    window_holding_gate(&mut cursor, run)
}

/// The names a window's readings hold in the trees, found there, for a window whose ticks cannot
/// say. Its wall clock bounds reach the readings with a clock and its publications those with none.
/// A wall clock that stepped widens the first, so the search has a bound, past which what it did
/// not reach stays. A window with no bounds keeps out of reach any reading of it that has a clock.
pub(crate) fn window_reading_names(
    root: &PrivateDirectory,
    recording: Identity,
    manifest: &Manifest,
    budget: &Arc<ByteBudget>,
) -> io::Result<Vec<(Namespace, CanonicalKey)>> {
    let run = manifest.identity.run;
    let publications = manifest.publication_first..manifest.publication_end;
    let mut names = Vec::with_capacity(61);
    let wall_range = manifest
        .wall_ns
        .map(|wall| known_page_range(wall.min, wall.max.saturating_add(1), None))
        .transpose()?
        .flatten();
    if let Some(range) = wall_range {
        let mut cursor =
            CanonicalCursor::begin_owned(root, recording, Namespace::Known, range, budget)?;
        for _reading in 0..READING_NAMES_MAX {
            let Some(key) = catalog_found(&mut cursor)? else {
                break;
            };
            let reading = KnownReading::from_key(recording, &key)?;
            if reading.run == run && publications.contains(&reading.publication) {
                names.push((Namespace::Known, key));
            }
        }
    }

    let unknown_at = |publication| UnknownReading { run, publication }.key();
    let range = SeekRange {
        first: unknown_at(manifest.publication_first)?,
        last: unknown_at(manifest.publication_end.saturating_sub(1))?,
        direction: Direction::Forward,
    };
    let mut cursor =
        CanonicalCursor::begin_owned(root, recording, Namespace::Unknown, range, budget)?;
    for _reading in 0..READING_NAMES_MAX {
        let Some(key) = catalog_found(&mut cursor)? else {
            break;
        };
        names.push((Namespace::Unknown, key));
    }

    Ok(names)
}

/// Keeps the window just counted if `readings`, its readings with no clock, are all it saved.
fn windows_without_clock_keep(
    windows: &mut Vec<Manifest>,
    counted: Option<(Manifest, u64)>,
) -> crate::Result<()> {
    let Some((window, readings)) = counted else {
        return Ok(());
    };
    let saved = window
        .tables
        .iter()
        .find(|table| table.name == "ticks")
        .ok_or_else(invalid)?
        .rows;
    if readings == saved {
        if windows.len() == RANGE_WINDOWS_MAX {
            return Err(crate::Error::Bound("catalog windows without a clock"));
        }
        windows.push(window);
    }

    Ok(())
}

fn windows_in_listed(windows: &[Manifest], reading: &KnownReading) -> bool {
    windows
        .iter()
        .any(|manifest| window_holds(manifest, &reading.run, reading.publication))
}

fn window_holds(manifest: &Manifest, run: &RunIdentity, publication: u64) -> bool {
    manifest.identity.run == *run
        && (manifest.publication_first..manifest.publication_end).contains(&publication)
}

/// The manifest behind the first gate `cursor` finds that owns a window, none where it finds no
/// such gate. A gate that owns only a loss holds no reading, so the window before it is the answer.
fn window_holding_gate(
    cursor: &mut CanonicalCursor<'_, 'static>,
    run: &RunIdentity,
) -> io::Result<Option<Manifest>> {
    for _gate in 0..LOOKUP_STEPS_MAX {
        let Some(key) = catalog_found(cursor)? else {
            return Ok(None);
        };
        let mut gate = cursor.open_gate(&key)?;
        let metadata = read_gate(&mut gate)?;
        if metadata.run != *run {
            return Err(invalid());
        }
        metadata.validate_key(Namespace::Ordinary, &key)?;
        cursor.check_snapshot()?;
        if let Body::Ordinary(manifest) = metadata.body {
            return Ok(Some(manifest));
        }
    }

    Err(invalid())
}

/// Reads the proof a gate links. A gate names a proof only once it is whole, so a short one is
/// damage and not a write still going on.
fn read_gate(gate: &mut std::fs::File) -> io::Result<CanonicalMetadata> {
    CanonicalMetadata::read(gate).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            invalid()
        } else {
            error
        }
    })
}

/// The namespace range a page of event objects reads, none when nothing can follow its position.
fn event_objects_range(position: &EventObjectPosition) -> io::Result<Option<SeekRange>> {
    let width = position.namespace.width();
    let first = match &position.after {
        Some(key) => match key_successor(position.namespace, key)? {
            Some(first) => first,
            None => return Ok(None),
        },
        None => CanonicalKey::from_bytes(position.namespace, &[0; 60][..width])?,
    };

    Ok(Some(SeekRange {
        first,
        last: CanonicalKey::from_bytes(position.namespace, &[0xff; 60][..width])?,
        direction: Direction::Forward,
    }))
}

/// Where the objects go on once `namespace` has none left, none after the last namespace.
fn event_objects_next_namespace(namespace: Namespace) -> Option<EventObjectPosition> {
    match namespace {
        Namespace::Terminal => Some(EventObjectPosition {
            namespace: Namespace::Fragments,
            after: None,
        }),
        _ => None,
    }
}

/// The object behind the gate at `key`, whose proof was written with the gate's own name.
fn event_object(
    cursor: &mut CanonicalCursor<'_, 'static>,
    namespace: Namespace,
    key: &CanonicalKey,
) -> io::Result<EventObject> {
    let mut gate = cursor.open_gate(key)?;
    let metadata = read_gate(&mut gate)?;
    metadata.validate_key(namespace, key)?;
    cursor.check_snapshot()?;
    let recovery = match metadata.body {
        Body::Events(manifest) => manifest.recovery,
        Body::LossOnly => Recovery {
            run: metadata.run,
            events_first: metadata.loss.inherited.event_start,
            events_end: metadata.loss.event_end,
            rows: 0,
            losses: metadata.loss.totals()?,
        },
        Body::Ordinary(_) => return Err(invalid()),
    };
    let kind = if namespace == Namespace::Terminal {
        EventObjectKind::Terminal
    } else {
        EventObjectKind::Fragment
    };

    Ok(EventObject { kind, recovery })
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

/// Appends to `keys` the keys `cursor` finds, up to `keys_max` in all, in no more steps than
/// `catalog_found` takes to find as many one at a time.
fn catalog_found_keys(
    cursor: &mut CanonicalCursor<'_, 'static>,
    keys: &mut Vec<CanonicalKey>,
    keys_max: usize,
) -> io::Result<()> {
    for _step in 0..LOOKUP_STEPS_MAX * keys_max {
        if keys.len() == keys_max {
            return Ok(());
        }
        if cursor.step_keys(keys, keys_max)? {
            return Ok(());
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
        Some(reading) => match key_successor(Namespace::Known, &reading.key()?)? {
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
fn key_successor(namespace: Namespace, key: &CanonicalKey) -> io::Result<Option<CanonicalKey>> {
    let mut bytes = key.bytes;
    for index in (0..namespace.width()).rev() {
        if bytes[index] != 0xff {
            bytes[index] += 1;
            return CanonicalKey::from_bytes(namespace, &bytes[..namespace.width()]).map(Some);
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
