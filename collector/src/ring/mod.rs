//! The shared-memory ring the collector hands each tick to, which other processes of the same
//! user map read-only and read at their own pace. One writer publishes each tick into the
//! next slot and never waits for a reader. A reader copies a slot and checks its sequence word
//! again, as a single-writer seqlock does, so it drops a slot overwritten during the copy
//! instead of reading it torn, and learns from the publication numbers which it missed.
//!
//! Process events go to a log of their own beside the slots, so a tick with many events
//! overwrites no other tick's. Each record of the log is a seqlock of its own, numbered by
//! event, and each slot names the events of its tick.

mod battery;
mod disks;
mod event;
mod identity;
mod layout;
mod object;
mod tick;
mod view;
mod wake;

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering, fence};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use event::{load_event, store_event};
pub use identity::{
    DictionaryUsage, IdentityAvailability, IdentityObservation, IdentityPublication,
    IdentityUnavailable, PROGRAM_PATH_BYTES_MAX, ProcessCreation, ProcessIdentity, ProgramBuffers,
    ProgramBytes, ProgramBytesError, ProgramReference, UsageAvailability,
};
use layout::{
    EVENT_SEQUENCE, HEADER_EVENT_FIRST, HEADER_EVENT_NEWEST, HEADER_PUBLICATION_FIRST,
    HEADER_PUBLICATION_NEWEST, HEADER_RUN_START_NS, Header, Held, Layout, SLOT_SEQUENCE,
    store_header,
};
pub use object::{
    GENERATION_BUFFER_SIZE, GENERATION_SCAN_MAX, GenerationName, generation_name,
    previous_generation, unlink_generation,
};
use object::{
    LockedName, Mapping, NUMBER_MAX, PROT_READ, PROT_WRITE, RingNames, create_lock, create_object,
    create_previous, create_unlink, create_unlink_older, open_lock_file, open_ring, place_for_user,
    ring_name,
};
pub use tick::{Dropped, IOReportReadings, Readings, SmcReadings, Tick};
pub use view::{RingReader, TickBuffers};

/// Explicit opt-in dictionary bounds; default writers continue using layout 3.
#[derive(Debug, Clone, Copy)]
pub struct DictionaryCapacity {
    pub entries: u64,
    pub arena_bytes: u64,
}

impl Default for DictionaryCapacity {
    fn default() -> Self {
        Self {
            entries: 65_536,
            arena_bytes: 64 * 1024 * 1024,
        }
    }
}

/// An opaque identity, independent of clocks and publication numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity(pub [u64; 2]);

/// The durable recording, this collector run and the boot it observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunIdentity {
    pub recording: Identity,
    pub run: Identity,
    pub boot: Identity,
}

/// Measured clocks in nanoseconds: Unix wall time, continuous time and awake time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationTimes {
    pub wall_ns: u64,
    pub continuous_ns: u64,
    pub awake_ns: u64,
}

use crate::kernel::ENOENT;
use crate::tracker::Event;
use crate::u64_from_usize;
use tick::{load_tick, store_tick};
use wake::{Waited, wait, wake_all};

/// A process never reuses a reference owner, even if the same object metadata is reopened.
static REFERENCE_OWNER_NEXT: AtomicU64 = AtomicU64::new(1);

fn next_reference_owner() -> io::Result<u64> {
    REFERENCE_OWNER_NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| io::Error::from(io::ErrorKind::Other))
}

/// How long a reader whose wait macOS cannot take sleeps before it looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Where a ring lives: the lock file its writer holds, which names the ring, and the start of
/// the name of each ring the writer makes.
pub struct Place {
    pub lock_path: PathBuf,
    pub name_prefix: String,
}

impl Place {
    /// The place of the ring `name` of this user, whose lock file is in a directory of this
    /// user's Application Support that only this user may enter. Returns an error where `name`
    /// is no file name, or where that directory lets another user in.
    pub fn for_user(name: &str) -> io::Result<Self> {
        place_for_user(name)
    }
}

/// The room a ring takes, fixed when its writer makes it: the publications it holds before
/// it overwrites the oldest, and the most records of each kind a tick holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub slot_count: u64,
    pub cpu_capacity: u64,
    pub interface_capacity: u64,
    pub core_capacity: u64,
    /// The most active states the GPU or one core has.
    pub state_capacity: u64,
    pub fan_capacity: u64,
    pub process_capacity: u64,
    pub coalition_capacity: u64,
    /// The events the log holds before it overwrites the oldest.
    pub event_capacity: u64,
}

/// What a reader found at its next publication.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    /// The publication it read, which carries the tick of the collector's run. The reader's
    /// `tick` holds the rest of it.
    Publication { publication: u64, tick: u64 },
    /// The publications from `first` up to `end` that the writer overwrote before the reader
    /// read them, which it reads past.
    Missed { first: u64, end: u64 },
    /// No publication past those it has read yet.
    Nothing,
}

/// What a reader found at its next event.
#[derive(Debug, PartialEq)]
pub enum FoundEvent {
    /// The event numbered `number`, which the tick of the collector's run `tick` drained.
    Event {
        number: u64,
        tick: u64,
        event: Event,
    },
    /// The events from `first` up to `end` that the writer overwrote before the reader read
    /// them, which it reads past.
    Missed { first: u64, end: u64 },
    /// No event past those it has read yet.
    Nothing,
}

/// The writer of a ring, which holds the lock file for as long as it lives.
pub struct Writer {
    _lock: File,
    recording_ownership: Option<File>,
    mapping: Mapping,
    layout: Layout,
    /// The newest publication written, or the previous ring's newest before the first.
    publication: u64,
    /// The newest event written, or the previous ring's newest before the first.
    event: u64,
    name: String,
    previous_generation: u64,
    dictionary: Option<identity::DictionaryIndex>,
    reference_owner: u64,
}

/// The exclusive place lock acquired before recording metadata may be changed.
pub(crate) struct LockedPlace<'place> {
    place: &'place Place,
    lock: File,
}

impl<'place> LockedPlace<'place> {
    pub(crate) fn acquire(place: &'place Place) -> io::Result<Self> {
        Ok(Self {
            place,
            lock: create_lock(&place.lock_path)?,
        })
    }

    pub(crate) fn create_recording(
        self,
        shape: Shape,
        identity: &RunIdentity,
        start_continuous_ns: u64,
    ) -> io::Result<Writer> {
        Writer::create_inner(
            self,
            shape,
            WriterOrigin::Identified(identity, start_continuous_ns),
        )
    }

    pub(crate) fn create_with_identity(
        self,
        shape: Shape,
        capacity: DictionaryCapacity,
        identity: &RunIdentity,
        start_continuous_ns: u64,
    ) -> io::Result<Writer> {
        let layout = Layout::identity(shape, capacity)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;

        Writer::create_layout(
            self,
            layout,
            WriterOrigin::Identified(identity, start_continuous_ns),
        )
    }

    pub(crate) fn create_with_battery(
        self,
        shape: Shape,
        capacity: DictionaryCapacity,
        identity: &RunIdentity,
        start_continuous_ns: u64,
    ) -> io::Result<Writer> {
        let layout = Layout::identity_battery(shape, capacity)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;

        Writer::create_layout(
            self,
            layout,
            WriterOrigin::Identified(identity, start_continuous_ns),
        )
    }

    pub(crate) fn create_with_disks(
        self,
        shape: Shape,
        capacity: DictionaryCapacity,
        identity: &RunIdentity,
        start_continuous_ns: u64,
    ) -> io::Result<Writer> {
        let layout = Layout::identity_disks(shape, capacity)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;

        Writer::create_layout(
            self,
            layout,
            WriterOrigin::Identified(identity, start_continuous_ns),
        )
    }

    pub(crate) fn create_unregistered(self, shape: Shape, start_ns: u64) -> io::Result<Writer> {
        Writer::create_inner(self, shape, WriterOrigin::Unregistered(start_ns))
    }
}

#[derive(Clone, Copy)]
enum WriterOrigin<'identity> {
    Legacy,
    Identified(&'identity RunIdentity, u64),
    Unregistered(u64),
}

/// Optional source snapshots share the publication sequence of the original tick.
#[derive(Default)]
pub struct SupplementalReadings<'a> {
    pub battery: Option<&'a crate::battery::BatteryReading>,
    pub disks: Option<&'a crate::disks::DiskSnapshot>,
}

impl Writer {
    /// Takes the lock of `place` and makes a ring of `shape` in it, under the generation past
    /// the ring the lock file names, whose publication numbers it continues. Returns an error
    /// where another writer holds the lock.
    pub fn create(place: &Place, shape: Shape) -> io::Result<Self> {
        Self::create_inner(LockedPlace::acquire(place)?, shape, WriterOrigin::Legacy)
    }

    /// Creates a run without reclaiming any older generation. The owner registers and
    /// explicitly reclaims retained generations after accounting for their loss or seal.
    pub fn create_recording(
        place: &Place,
        shape: Shape,
        identity: &RunIdentity,
        run_start_continuous_ns: u64,
    ) -> io::Result<Self> {
        LockedPlace::acquire(place)?.create_recording(shape, identity, run_start_continuous_ns)
    }

    /// Creates an opt-in identity ring; ordinary create methods continue writing v3.
    pub fn create_with_identity(
        place: &Place,
        shape: Shape,
        capacity: DictionaryCapacity,
        identity: &RunIdentity,
        start_continuous_ns: u64,
    ) -> io::Result<Self> {
        let layout = Layout::identity(shape, capacity)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;

        Self::create_layout(
            LockedPlace::acquire(place)?,
            layout,
            WriterOrigin::Identified(identity, start_continuous_ns),
        )
    }

    /// Creates a V4 identity ring with the optional version-one battery tail.
    pub fn create_with_battery(
        place: &Place,
        shape: Shape,
        capacity: DictionaryCapacity,
        identity: &RunIdentity,
        start_continuous_ns: u64,
    ) -> io::Result<Self> {
        LockedPlace::acquire(place)?.create_with_battery(
            shape,
            capacity,
            identity,
            start_continuous_ns,
        )
    }

    /// Creates the unchanged identity/Battery prefix followed by bounded disk slots.
    pub fn create_with_disks(
        place: &Place,
        shape: Shape,
        capacity: DictionaryCapacity,
        identity: &RunIdentity,
        start_continuous_ns: u64,
    ) -> io::Result<Self> {
        LockedPlace::acquire(place)?.create_with_disks(
            shape,
            capacity,
            identity,
            start_continuous_ns,
        )
    }

    /// Exact reserved mapping size, planned before creating or touching the ring.
    pub fn identity_size_bytes(shape: Shape, capacity: DictionaryCapacity) -> io::Result<u64> {
        let layout = Layout::identity(shape, capacity)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;

        Ok(u64_from_usize(layout.size_bytes()))
    }

    /// Exact startup reservation, including the optional battery header and every slot.
    pub fn battery_size_bytes(shape: Shape, capacity: DictionaryCapacity) -> io::Result<u64> {
        let layout = Layout::identity_battery(shape, capacity)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;

        Ok(u64_from_usize(layout.size_bytes()))
    }

    pub fn disks_size_bytes(shape: Shape, capacity: DictionaryCapacity) -> io::Result<u64> {
        let layout = Layout::identity_disks(shape, capacity)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;

        Ok(u64_from_usize(layout.size_bytes()))
    }

    /// Whether startup reserved the optional battery records for this writer.
    #[must_use]
    pub fn battery_supported(&self) -> bool {
        self.layout.battery.is_some()
    }

    #[must_use]
    pub fn disks_supported(&self) -> bool {
        self.layout.disks.is_some()
    }

    /// Interns retained bytes without allocation. Known keys survive dictionary exhaustion.
    /// A successful output belongs only to this writer's `publish_with_identity` calls.
    /// Failure leaves the caller's output unchanged.
    pub fn intern_program(
        &mut self,
        bytes: &ProgramBytes,
        program: &mut Option<ProgramReference>,
    ) -> Result<(), IdentityAvailability> {
        let Some(dictionary) = self.dictionary.as_mut() else {
            return Err(IdentityAvailability::Unsupported);
        };

        dictionary.intern(self.mapping.words(), &self.layout, bytes, program)
    }

    /// Reports bounded dictionary occupancy without allocating.
    pub fn dictionary_usage(&self) -> io::Result<Option<DictionaryUsage>> {
        identity::dictionary_usage(self.mapping.words(), &self.layout)
    }

    /// Validates all references before publishing events or the tick.
    pub fn publish_with_identity(
        &mut self,
        readings: &Readings,
        times: Option<&ObservationTimes>,
        identities: &IdentityPublication,
    ) -> io::Result<()> {
        identity::validate_publication(
            self.mapping.words(),
            &self.layout,
            readings,
            identities,
            self.reference_owner,
        )?;

        self.publish_inner(
            readings,
            times,
            Some(identities),
            &SupplementalReadings::default(),
        );

        Ok(())
    }

    /// Publishes the battery cache under the same sequence as the numeric and identity rows.
    pub fn publish_with_battery(
        &mut self,
        readings: &Readings,
        times: Option<&ObservationTimes>,
        identities: &IdentityPublication,
        battery: Option<&crate::battery::BatteryReading>,
    ) -> io::Result<()> {
        if self.layout.battery.is_none() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        identity::validate_publication(
            self.mapping.words(),
            &self.layout,
            readings,
            identities,
            self.reference_owner,
        )?;

        self.publish_inner(
            readings,
            times,
            Some(identities),
            &SupplementalReadings {
                battery,
                disks: None,
            },
        );
        Ok(())
    }

    /// Validates every disk before advancing the tick and event publication sequences.
    pub fn publish_with_disks(
        &mut self,
        readings: &Readings,
        times: Option<&ObservationTimes>,
        identities: &IdentityPublication,
        supplemental: &SupplementalReadings,
    ) -> io::Result<()> {
        if self.layout.disks.is_none() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }

        if let Some(disks) = supplemental.disks {
            crate::disks::encoding::validate(disks)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        }
        identity::validate_publication(
            self.mapping.words(),
            &self.layout,
            readings,
            identities,
            self.reference_owner,
        )?;
        self.publish_inner(readings, times, Some(identities), supplemental);

        Ok(())
    }

    pub(crate) fn reclaim_unregistered(&self) {
        let first = self.generation().saturating_sub(GENERATION_SCAN_MAX).max(1);
        let mut buffer = [0; GENERATION_BUFFER_SIZE];
        for generation in first..self.generation() {
            if generation == self.previous_generation {
                continue;
            }
            let name = match generation_name(self.name(), generation, &mut buffer) {
                Ok(name) => name,
                Err(error) => {
                    eprintln!("ring: retention name refused: {error}");
                    continue;
                }
            };
            match RingReader::open_generation(name) {
                Ok(reader) => {
                    if reader.identity().is_some() {
                        continue;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    eprintln!("ring: preserving undecodable generation {generation}: {error}");
                    continue;
                }
            }
            eprintln!(
                "recording retention eviction generation={generation}; unsealed extent unknown"
            );
            if let Err(error) = unlink_generation(name) {
                eprintln!("ring: retention eviction refused: {error}");
            }
        }
    }

    fn create_inner(
        locked: LockedPlace<'_>,
        shape: Shape,
        origin: WriterOrigin,
    ) -> io::Result<Self> {
        let Some(layout) = Layout::new(shape) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a ring of shape {shape:?}"),
            ));
        };

        Self::create_layout(locked, layout, origin)
    }

    fn create_layout_dictionary(
        layout: &Layout,
        reference_owner: u64,
        dictionary: &mut Option<identity::DictionaryIndex>,
    ) {
        if layout.version != layout::LAYOUT_VERSION_IDENTITY {
            return;
        }

        *dictionary = Some(identity::DictionaryIndex {
            entries: Vec::new(),
            owner: 0,
        });
        if let Some(dictionary) = dictionary.as_mut() {
            dictionary.initialize(layout.dictionary.capacity, reference_owner);
        }
    }

    fn create_layout(
        locked: LockedPlace<'_>,
        layout: Layout,
        origin: WriterOrigin,
    ) -> io::Result<Self> {
        let LockedPlace { place, lock } = locked;
        let reference_owner = next_reference_owner()?;
        let mut dictionary = None;

        Self::create_layout_dictionary(&layout, reference_owner, &mut dictionary);

        let run_start_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let run_start_ns = u64::try_from(run_start_ns).map_err(io::Error::other)?;

        let previous = create_previous(&place.name_prefix, &lock)?;

        let names = RingNames {
            prefix: &place.name_prefix,
            token: previous.token,
        };
        // A writer stopped after making a ring under a token it just drew would leave a ring
        // nothing can name, so the lock file names the token first. Generation 0 is no ring,
        // and a reader takes it as a ring that is gone.
        if previous.generation == 0 {
            create_name(&lock, &ring_name(&names, 0)?)?;
        }

        let created = create_object(&names, previous.generation, &layout)?;

        // A failed start removes its unnamed ring, or it would consume future generations.
        let mapping = Mapping::new(&created.object, layout.word_count(), PROT_READ | PROT_WRITE)
            .inspect_err(|_| create_unlink(&created.name))?;

        let header = Header {
            generation: created.generation,
            publication_first: previous.publication_newest + 1,
            run_start_ns,
            event_first: previous.event_newest + 1,
        };
        store_header(mapping.words(), &layout, &header);
        store_origin(mapping.words(), &origin);
        store_supplemental_headers(mapping.words(), &layout);

        create_name(&lock, &created.name).inspect_err(|_| create_unlink(&created.name))?;
        // Only once the lock file names the new ring, so a reader that reads the lock file from
        // now on never finds the ring it names gone. One that read it just before finds the
        // empty ring gone and looks again.
        if let WriterOrigin::Legacy = origin {
            create_unlink_older(&names, previous.generation);
        }

        Ok(Self {
            _lock: lock,
            recording_ownership: None,
            mapping,
            layout,
            publication: previous.publication_newest,
            event: previous.event_newest,
            name: created.name,
            previous_generation: previous.generation,
            dictionary,
            reference_owner,
        })
    }

    pub(crate) fn retain_recording_ownership(&mut self, ownership: File) {
        assert!(self.recording_ownership.is_none());
        self.recording_ownership = Some(ownership);
    }

    /// Publishes `readings` in the next slot, overwriting the oldest publication once every
    /// slot holds one, and its events in the log, overwriting the oldest events once every
    /// record holds one. An identity writer uses `publish_with_identity` instead.
    pub fn publish(&mut self, readings: &Readings) {
        self.publish_inner(readings, None, None, &SupplementalReadings::default());
    }

    /// Publishes measured clocks in the default v3 layout.
    pub fn publish_observed(&mut self, readings: &Readings, times: &ObservationTimes) {
        self.publish_inner(
            readings,
            Some(times),
            None,
            &SupplementalReadings::default(),
        );
    }

    fn publish_inner(
        &mut self,
        readings: &Readings,
        times: Option<&ObservationTimes>,
        identities: Option<&IdentityPublication>,
        supplemental: &SupplementalReadings,
    ) {
        assert_eq!(identities.is_some(), self.dictionary.is_some());
        // A tick's events fit in the log, so a reader can read every one of them.
        assert!(u64_from_usize(readings.events.len()) <= self.layout.shape.event_capacity);

        let words = self.mapping.words();
        let events_first = self.event + 1;
        for (index, event) in readings.events.iter().enumerate() {
            let number = self.event + 1;
            let record = &words[self.layout.event(number)];
            record[EVENT_SEQUENCE].store(2 * number - 1, Ordering::Relaxed);
            fence(Ordering::Release);
            store_event(&record[..layout::EVENT_WORDS], readings.run_tick, event);
            if let Some(identities) = identities {
                identity::store_event_identity(record, &identities.events[index]);
            }
            record[EVENT_SEQUENCE].store(2 * number, Ordering::Release);
            self.event = number;
        }
        words[HEADER_EVENT_NEWEST].store(self.event, Ordering::Release);

        let publication = self.publication + 1;
        let slot = &words[self.layout.slot(publication)];
        slot[SLOT_SEQUENCE].store(2 * publication - 1, Ordering::Relaxed);
        fence(Ordering::Release);
        store_tick(
            slot,
            &self.layout,
            readings,
            events_first..self.event + 1,
            times,
        );
        if let Some(identities) = identities {
            identity::store_processes(slot, &self.layout, identities.processes);
        }
        if let Some(layout) = &self.layout.battery {
            battery::store(&words[layout.record(publication)], supplemental.battery);
        }
        if let Some(layout) = &self.layout.disks {
            disks::store(&words[layout.record(publication)], supplemental.disks);
        }
        slot[SLOT_SEQUENCE].store(2 * publication, Ordering::Release);

        words[HEADER_PUBLICATION_NEWEST].store(publication, Ordering::Release);
        self.publication = publication;
        // A refused wake costs the readers it missed one wait at most, so the writer goes on.
        if let Err(errno) = wake_all(&words[HEADER_PUBLICATION_NEWEST]) {
            eprintln!("ring: waking readers failed with errno {errno}");
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.mapping.words()[layout::HEADER_GENERATION].load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn identity(&self) -> Option<RunIdentity> {
        read_identity(self.mapping.words(), &self.layout)
    }

    /// Marks a known orderly end; a crash leaves this false.
    pub fn finish(&mut self) {
        self.mapping.words()[layout::HEADER_COMPLETED].store(1, Ordering::Release);
    }
}

fn store_origin(words: &[AtomicU64], origin: &WriterOrigin) {
    let continuous_ns = match origin {
        WriterOrigin::Legacy => {
            words[layout::HEADER_ORIGIN_STATE].store(layout::ORIGIN_UNAVAILABLE, Ordering::Relaxed);
            return;
        }
        WriterOrigin::Identified(identity, continuous_ns) => {
            store_identity(words, identity);
            *continuous_ns
        }
        WriterOrigin::Unregistered(continuous_ns) => {
            words[layout::HEADER_ORIGIN_STATE]
                .store(layout::ORIGIN_UNREGISTERED, Ordering::Relaxed);
            *continuous_ns
        }
    };
    words[layout::HEADER_RUN_START_CONTINUOUS_NS].store(continuous_ns, Ordering::Relaxed);
}

fn store_identity(words: &[AtomicU64], identity: &RunIdentity) {
    for (index, value) in [identity.recording.0, identity.run.0, identity.boot.0]
        .into_iter()
        .flatten()
        .enumerate()
    {
        words[layout::HEADER_IDENTITY + index].store(value, Ordering::Relaxed);
    }
    words[layout::HEADER_ORIGIN_STATE].store(layout::ORIGIN_IDENTIFIED, Ordering::Relaxed);
}

fn read_identity(words: &[AtomicU64], layout: &Layout) -> Option<RunIdentity> {
    if layout.version < layout::LAYOUT_VERSION_OBSERVED {
        return None;
    }
    if words[layout::HEADER_ORIGIN_STATE].load(Ordering::Relaxed) != layout::ORIGIN_IDENTIFIED {
        return None;
    }
    let identity = |offset: usize| {
        Identity([
            words[layout::HEADER_IDENTITY + offset].load(Ordering::Relaxed),
            words[layout::HEADER_IDENTITY + offset + 1].load(Ordering::Relaxed),
        ])
    };
    Some(RunIdentity {
        recording: identity(0),
        run: identity(layout::IDENTITY_WORDS),
        boot: identity(2 * layout::IDENTITY_WORDS),
    })
}

/// A reader of a ring, which maps it read-only.
pub struct Reader {
    opened: Opened,
    tick: LoadedTick,
}

/// A ring a reader opened, before it has room for the ring's ticks.
struct Opened {
    name: LockedName,
    mapping: Mapping,
    publications: Cursor,
    run_start_ns: u64,
    events: Cursor,
    layout: Layout,
    reference_owner: u64,
}

impl Opened {
    /// The ring the lock file at `lock_path` names, with the reader's place at the oldest
    /// publication and event it holds, and no room for its ticks, which `follow` keeps.
    fn open_named(lock_path: &Path) -> io::Result<Opened> {
        let name = LockedName::read(&open_lock_file(lock_path)?)?;
        if name.as_str().is_empty() {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }

        Self::open_name(name)
    }

    fn open_name(name: LockedName) -> io::Result<Opened> {
        let (mapping, layout) = open_ring(name.as_str())?;
        let words = mapping.words();
        let first = words[HEADER_PUBLICATION_FIRST].load(Ordering::Relaxed);
        let newest = words[HEADER_PUBLICATION_NEWEST].load(Ordering::Relaxed);
        let run_start_ns = words[HEADER_RUN_START_NS].load(Ordering::Relaxed);
        let event_first = words[HEADER_EVENT_FIRST].load(Ordering::Relaxed);
        let event_newest = words[HEADER_EVENT_NEWEST].load(Ordering::Relaxed);
        open_named_check(&Numbering { first, newest })?;
        open_named_check(&Numbering {
            first: event_first,
            newest: event_newest,
        })?;

        Ok(Opened {
            name,
            publications: Cursor::new(first, layout.publications_held(newest)),
            run_start_ns,
            events: Cursor::new(event_first, layout.events_held(event_newest)),
            mapping,
            layout,
            reference_owner: next_reference_owner()?,
        })
    }

    fn read_into(&mut self, tick: &mut Tick) -> io::Result<Found> {
        self.read_into_identity(tick, None)
    }

    fn read_into_identity(
        &mut self,
        tick: &mut Tick,
        mut identities: Option<&mut Vec<IdentityObservation>>,
    ) -> io::Result<Found> {
        if let Some(identities) = identities.as_ref() {
            if identities.capacity() < self.layout.processes.capacity {
                return Err(io::Error::from(io::ErrorKind::Unsupported));
            }
        }
        if !tick.fits(&self.layout) {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let words = self.mapping.words();
        let newest = read_newest(&words[HEADER_PUBLICATION_NEWEST])?;
        let publication = match self
            .publications
            .step(self.layout.publications_held(newest))
        {
            Step::Nothing => return Ok(Found::Nothing),
            Step::Missed { first, end } => return Ok(Found::Missed { first, end }),
            Step::Read(publication) => publication,
        };
        let slot = &words[self.layout.slot(publication)];
        let loaded = read_tick(
            slot,
            publication,
            tick,
            |tick| {
                if !load_tick(slot, &self.layout, tick) {
                    return false;
                }
                if let Some(identities) = identities.as_mut() {
                    if identity::load_processes(
                        &identity::IdentityRecord {
                            words,
                            record: slot,
                        },
                        &self.layout,
                        tick,
                        identities,
                        self.reference_owner,
                    )
                    .is_err()
                    {
                        return false;
                    }
                }
                load_disks(words, &self.layout, publication, tick)
            },
            || match &self.layout.battery {
                Some(layout) => battery::load(&words[layout.record(publication)]),
                None => Ok(None),
            },
        )?;
        if loaded.is_some() {
            Ok(Found::Publication {
                publication,
                tick: tick.run_tick,
            })
        } else {
            Ok(Found::Missed {
                first: publication,
                end: publication + 1,
            })
        }
    }

    fn read_event(&mut self) -> io::Result<FoundEvent> {
        let words = self.mapping.words();
        let newest = read_newest(&words[HEADER_EVENT_NEWEST])?;
        let number = match self.events.step(self.layout.events_held(newest)) {
            Step::Nothing => return Ok(FoundEvent::Nothing),
            Step::Missed { first, end } => return Ok(FoundEvent::Missed { first, end }),
            Step::Read(number) => number,
        };
        let record = &words[self.layout.event(number)];
        match read_finished(record, EVENT_SEQUENCE, number, || {
            load_event(&record[..layout::EVENT_WORDS])
        })? {
            Some((tick, event)) => Ok(FoundEvent::Event {
                number,
                tick,
                event,
            }),
            None => Ok(FoundEvent::Missed {
                first: number,
                end: number + 1,
            }),
        }
    }
}

fn store_supplemental_headers(words: &[AtomicU64], layout: &Layout) {
    if let Some(battery) = &layout.battery {
        battery.store_header(words);
    }
    if let Some(disks) = &layout.disks {
        disks.store_header(words);
    }
}

fn load_disks(words: &[AtomicU64], layout: &Layout, publication: u64, tick: &mut Tick) -> bool {
    if let Some(layout) = &layout.disks {
        disks::load(&words[layout.record(publication)], &mut tick.disks).is_ok()
    } else {
        tick.disks = None;
        true
    }
}

/// Loads the base and optional battery before the publication's final sequence check.
fn read_tick(
    slot: &[AtomicU64],
    publication: u64,
    tick: &mut Tick,
    load_base: impl FnOnce(&mut Tick) -> bool,
    load_battery: impl FnOnce() -> Result<Option<crate::battery::BatteryReading>, io::ErrorKind>,
) -> io::Result<Option<()>> {
    read_finished(slot, SLOT_SEQUENCE, publication, || {
        if !load_base(tick) {
            return None;
        }
        tick.battery = match load_battery() {
            Ok(reading) => reading,
            Err(_) => return None,
        };
        Some(())
    })
}

impl Reader {
    /// Opens the ring the lock file at `lock_path` names, starting at the oldest publication
    /// it holds.
    pub fn open(lock_path: &Path) -> io::Result<Self> {
        let opened = Opened::open_named(lock_path)?;
        Ok(Self {
            tick: LoadedTick::new(&opened.layout),
            opened,
        })
    }

    /// Whether the lock file at `lock_path` names another ring than the reader's, as it does
    /// once the writer restarted, so the reader opens the newer ring after it has read every
    /// publication of its own. A lock file the writer is part-way through writing, or one
    /// naming generation 0, which a writer names before it makes its first ring, names no
    /// ring yet, which is no move.
    pub fn moved(&self, lock_path: &Path) -> io::Result<bool> {
        let locked = LockedName::read(&open_lock_file(lock_path)?)?;
        let name = locked.as_str();
        if name.is_empty() {
            return Ok(false);
        }

        if name.ends_with(".0") {
            return Ok(false);
        }

        Ok(name != self.opened.name.as_str())
    }

    /// Opens the ring the lock file at `lock_path` names in place of the reader's own, once
    /// the writer restarted and the reader has read every publication its own ring holds and
    /// the events of the last, and returns whether it did. The reader reads on from where it
    /// stood, so the next reads return what the newer writer overwrote first as missed. A newer
    /// ring whose ticks have other capacities is refused with `io::ErrorKind::Unsupported`.
    pub fn follow(&mut self, lock_path: &Path) -> io::Result<bool> {
        if !self.moved(lock_path)? {
            return Ok(false);
        }

        // The writer that left this ring may have published once more before it stopped.
        let words = self.opened.mapping.words();
        if self.opened.publications.next <= words[HEADER_PUBLICATION_NEWEST].load(Ordering::Relaxed)
        {
            return Ok(false);
        }

        // Only the events a publication names are the reader's to read before it moves. A
        // writer that died between logging a tick's events and publishing its slot left
        // events no publication names, which the reader learns as missed on the newer ring.
        if self.opened.events.next < self.tick.last.events.end {
            return Ok(false);
        }

        let mut newer = match Opened::open_named(lock_path) {
            Ok(newer) => newer,
            // A writer started after the lock file was read, and removed the empty ring it
            // named, so the reader looks again on its next follow.
            Err(error) if error.raw_os_error() == Some(ENOENT) => return Ok(false),
            Err(error) => return Err(error),
        };

        // The room for ticks is sized to the capacities of a tick, so the reader keeps its own
        // and allocates nothing only when the newer ring's ticks have the same capacities. A
        // ring whose ticks hold more or fewer is refused as Unsupported, which allocates no
        // message, and a caller that wants it opens it afresh. Slot and event counts size
        // nothing the reader holds, so they may differ.
        let tick_shape = Shape {
            slot_count: newer.layout.shape.slot_count,
            event_capacity: newer.layout.shape.event_capacity,
            ..self.opened.layout.shape
        };
        if tick_shape != newer.layout.shape {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }

        newer.publications.carry(&self.opened.publications);
        newer.events.carry(&self.opened.events);
        self.opened = newer;
        Ok(true)
    }

    /// The tick of the publication the reader read last.
    #[must_use]
    pub fn tick(&self) -> &Tick {
        &self.tick.last
    }

    /// When the writer's run started, in nanoseconds since 1970 by the wall clock.
    #[must_use]
    pub fn run_start_ns(&self) -> u64 {
        self.opened.run_start_ns
    }

    /// Sleeps until the writer publishes past the publications the reader has read, for at
    /// most `timeout`, and returns at once where it already has. A reader whose wait macOS
    /// cannot take sleeps `POLL_INTERVAL` instead, or `timeout` where that is shorter.
    pub fn wait(&self, timeout: Duration) -> io::Result<()> {
        assert!(!timeout.is_zero());

        let newest_word = &self.opened.mapping.words()[HEADER_PUBLICATION_NEWEST];
        let newest = newest_word.load(Ordering::Relaxed);
        if self.opened.publications.next <= newest {
            return Ok(());
        }

        if wait(newest_word, newest, timeout)? == Waited::Unsupported {
            std::thread::sleep(timeout.min(POLL_INTERVAL));
        }
        Ok(())
    }

    /// Reads the next publication, or the range of those the writer overwrote first.
    pub fn read(&mut self) -> io::Result<Found> {
        let found = self.opened.read_into(&mut self.tick.next)?;
        if matches!(found, Found::Publication { .. }) {
            std::mem::swap(&mut self.tick.last, &mut self.tick.next);
        }
        Ok(found)
    }

    /// The number of the event the reader reads next, so it can read the events of one tick
    /// and no further.
    #[must_use]
    pub fn next_event(&self) -> u64 {
        self.opened.events.next
    }

    /// Reads the next event, or the range of those the writer overwrote first.
    pub fn read_event(&mut self) -> io::Result<FoundEvent> {
        self.opened.read_event()
    }
}

/// Writes `name` into the lock file. Writing the name before cutting the file to its length
/// never leaves the lock file empty, as cutting first would for a writer stopped between the
/// two, which would make the next writer number from 1.
fn create_name(lock: &File, name: &str) -> io::Result<()> {
    lock.write_all_at(name.as_bytes(), 0)?;
    lock.set_len(u64_from_usize(name.len()))
}

/// The newest number the header word `newest_word` holds, with what the writer wrote before
/// it made visible, or an error where it is past any number a writer stores.
fn read_newest(newest_word: &AtomicU64) -> io::Result<u64> {
    let newest = newest_word.load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    if newest > NUMBER_MAX {
        return Err(number_past_max(newest));
    }

    Ok(newest)
}

/// Where a reader stands in numbers the writer hands out from 1 and overwrites oldest first,
/// as its publications and its events.
struct Cursor {
    /// The first number of the ring, past the previous ring's.
    first: u64,
    /// The number the reader reads next.
    next: u64,
}

/// Refuses the numbers a header names in `numbering`, where no writer hands them
/// out: a numbering that starts at 0, or a number past `NUMBER_MAX`, which only another
/// program of this user could have written.
fn open_named_check(numbering: &Numbering) -> io::Result<()> {
    let Numbering { first, newest } = *numbering;
    if first == 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }

    if first > NUMBER_MAX {
        return Err(number_past_max(first));
    }

    if newest > NUMBER_MAX {
        return Err(number_past_max(newest));
    }

    Ok(())
}

/// The first and newest number a ring's header names for its publications or its events,
/// held apart by name since the two would swap unnoticed as arguments.
#[derive(Clone, Copy)]
struct Numbering {
    first: u64,
    newest: u64,
}

/// The error of a ring whose header names `number`, past any a writer hands out.
fn number_past_max(number: u64) -> io::Error {
    assert!(number > NUMBER_MAX);
    io::Error::from(io::ErrorKind::InvalidData)
}

/// What a reader does next with a cursor's numbers.
enum Step {
    /// Waits, since the writer has written no number past those read.
    Nothing,
    /// Reads past the numbers from `first` up to `end`, which the writer overwrote.
    Missed { first: u64, end: u64 },
    /// Reads the number it holds.
    Read(u64),
}

impl Cursor {
    /// A cursor at the oldest number the ring holds, or at `first` where the ring has not
    /// written that far.
    fn new(first: u64, held: Held) -> Self {
        Self {
            first,
            next: first.max(held.oldest),
        }
    }

    /// Takes the position of `older`, a cursor over the previous ring, where the numbers
    /// this ring continues start at or past it. A ring whose numbers start before it begins
    /// a numbering of its own, as after a lock file nobody could read, so the cursor starts
    /// at that numbering's first and the next step reports what the ring no longer holds.
    fn carry(&mut self, older: &Self) {
        self.next = older.next.min(self.first);
    }

    /// The next step once the writer holds the numbers `held`, moving the cursor past what
    /// the step covers.
    fn step(&mut self, held: Held) -> Step {
        if self.next > held.newest {
            return Step::Nothing;
        }

        let oldest = self.first.max(held.oldest);
        if self.next < oldest {
            let first = self.next;
            self.next = oldest;
            return Step::Missed { first, end: oldest };
        }

        let number = self.next;
        self.next += 1;
        Step::Read(number)
    }
}

/// The tick a reader read last, and the room it loads the next tick into, so a load the
/// writer overwrites leaves the last tick whole rather than part of a later one.
struct LoadedTick {
    last: Tick,
    next: Tick,
}

impl LoadedTick {
    fn new(layout: &Layout) -> Self {
        Self {
            last: Tick::new(layout),
            next: Tick::new(layout),
        }
    }

    /// Loads the tick the slot `slot` holds for `publication` in place of the last, and
    /// returns whether it did, which it did not where the writer overwrote the slot with a
    /// later publication before or during the load.
    #[cfg(test)]
    fn read_slot(
        &mut self,
        slot: &[AtomicU64],
        layout: &Layout,
        publication: u64,
    ) -> io::Result<bool> {
        let loaded = read_finished(slot, SLOT_SEQUENCE, publication, || {
            load_tick(slot, layout, &mut self.next).then_some(())
        })?;
        if loaded.is_some() {
            std::mem::swap(&mut self.last, &mut self.next);
        }
        Ok(loaded.is_some())
    }
}

/// What `load` makes of the words `record` holds for the number `number`, under the sequence
/// word at `sequence_index`, or `None` where the writer overwrote the record with a later
/// number before or during the load. The header said the writer finished `number`, so a
/// record behind it, or one that holds it and makes nothing, is a broken ring.
fn read_finished<T>(
    record: &[AtomicU64],
    sequence_index: usize,
    number: u64,
    load: impl FnOnce() -> Option<T>,
) -> io::Result<Option<T>> {
    assert!(number > 0);

    // The reader refuses a header naming a newer number, so the sequence cannot overflow.
    assert!(number <= NUMBER_MAX);

    let finished = 2 * number;
    let sequence = record[sequence_index].load(Ordering::Relaxed);
    fence(Ordering::Acquire);
    let loaded = load();
    fence(Ordering::Acquire);
    let sequence_after = record[sequence_index].load(Ordering::Relaxed);

    if sequence < finished {
        // A record only moves on to later numbers.
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }

    if sequence == finished {
        if sequence_after == sequence {
            if loaded.is_none() {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }

            return Ok(loaded);
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Memory;

    /// A layout of one slot with room for one record of each kind.
    fn layout() -> Layout {
        Layout::new(Shape {
            slot_count: 1,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        })
        .unwrap()
    }

    /// A slot of `layout()` that holds a tick of `run_tick` under the sequence `sequence`.
    fn slot_holding(sequence: u64, run_tick: u64) -> Vec<AtomicU64> {
        let layout = layout();
        let slot: Vec<AtomicU64> = (0..layout.slot_words).map(|_| AtomicU64::new(0)).collect();
        let readings = Readings {
            run_tick,
            cpus: &[],
            memory: &Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        };
        store_tick(&slot, &layout, &readings, 1..1, None);
        slot[SLOT_SEQUENCE].store(sequence, Ordering::Relaxed);
        slot
    }

    fn read(slot: &[AtomicU64], publication: u64) -> io::Result<Option<u64>> {
        let mut tick = LoadedTick::new(&layout());
        let read = tick.read_slot(slot, &layout(), publication)?;
        Ok(read.then_some(tick.last.run_tick))
    }

    #[test]
    fn a_dropped_slot_leaves_the_last_tick_read() {
        // Goal: after a slot the writer overwrote, the reader still holds the tick it read
        // last, never words of the later publication.
        // Method: read publication 3 from a finished slot of tick 40, then publication 4 from
        // a slot the writer is rewriting with tick 99, and look at the tick held.
        let finished = slot_holding(6, 40);
        let rewritten = slot_holding(13, 99);
        let mut tick = LoadedTick::new(&layout());
        assert!(tick.read_slot(&finished, &layout(), 3).unwrap());

        let read = tick.read_slot(&rewritten, &layout(), 4).unwrap();

        assert!(!read);
        assert_eq!(40, tick.last.run_tick);
    }

    #[test]
    fn reads_the_tick_of_a_slot_its_writer_finished() {
        // Goal: a finished slot gives its tick.
        // Method: read publication 3 from a slot whose sequence is twice 3.
        let slot = slot_holding(6, 40);

        assert_eq!(Some(40), read(&slot, 3).unwrap());
    }

    #[test]
    fn drops_a_slot_its_writer_is_writing_a_later_publication_into() {
        // Goal: a reader never reads a slot the writer is part-way through.
        // Method: read publication 3 from a slot of four whose writer started publication 7,
        // which leaves its sequence at twice 7 less 1.
        let slot = slot_holding(13, 40);

        assert_eq!(None, read(&slot, 3).unwrap());
    }

    #[test]
    fn drops_a_slot_its_writer_finished_a_later_publication_in() {
        // Goal: a slot overwritten before the reader came to it is missed, never read as the
        // publication the reader asked for.
        // Method: read publication 3 from a slot that holds publication 7.
        let slot = slot_holding(14, 40);

        assert_eq!(None, read(&slot, 3).unwrap());
    }

    #[test]
    fn drops_a_slot_its_writer_rewrote_during_the_load() {
        // Goal: a slot the writer starts rewriting while a reader loads it is dropped, never
        // returned with words of two publications, however the two threads are paced.
        // Method: read publication 3 from a slot finished for it, with a load that moves the
        // slot's sequence to twice 5 less 1, as a writer a lap ahead in a ring of two slots
        // leaves it when it starts publication 5.
        let slot = slot_holding(6, 40);

        let loaded = read_finished(&slot, SLOT_SEQUENCE, 3, || {
            slot[SLOT_SEQUENCE].store(9, Ordering::Relaxed);
            Some(())
        });

        assert_eq!(None, loaded.unwrap());
    }

    #[test]
    fn refuses_a_slot_behind_the_publication_the_header_finished() {
        // Goal: a slot behind a publication the header calls finished is a broken ring, which
        // the reader reports rather than reading.
        // Method: read publication 3 from a slot that holds publication 2.
        let slot = slot_holding(4, 40);

        assert_eq!(
            io::ErrorKind::InvalidData,
            read(&slot, 3).unwrap_err().kind()
        );
    }

    #[test]
    fn refuses_a_finished_slot_whose_words_make_no_tick() {
        // Goal: a slot the writer finished that holds no tick is a broken ring, which the
        // reader reports rather than reading.
        // Method: read publication 3 from a finished slot that says it holds two processes in
        // room for one.
        let slot = slot_holding(6, 40);
        slot[layout::SLOT_PROCESS_COUNT].store(2, Ordering::Relaxed);

        assert_eq!(
            io::ErrorKind::InvalidData,
            read(&slot, 3).unwrap_err().kind()
        );
    }

    #[test]
    fn layout_two_readings_keep_their_values_and_have_unknown_exact_clocks() {
        // Goal: old measured readings survive without invented observation timestamps.
        // Method: remove layout-three extension words from a published slot and decode it.
        let current = layout();
        let legacy = Layout::versioned(current.shape, layout::LAYOUT_VERSION_LEGACY).unwrap();
        let memory = Memory {
            total_bytes: 987_654_321,
            ..Memory::default()
        };
        let process = legacy_process();
        let readings = Readings {
            run_tick: 73,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[process],
            processes_refused: 2,
            processes_gone: 3,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        };
        let mut slot = slot_holding(2, 73);
        store_tick(&slot, &current, &readings, 1..1, None);
        slot.drain(layout::SLOT_WALL_NS..layout::SLOT_FIXED_WORDS);
        let mut tick = Tick::new(&legacy);

        assert!(load_tick(&slot, &legacy, &mut tick));

        assert_eq!(73, tick.run_tick);
        assert_eq!(None, tick.times);
        assert_eq!(memory, tick.memory);
        assert_eq!(&[process], tick.processes.as_slice());
        assert_eq!(2, tick.processes_refused);
        assert_eq!(3, tick.processes_gone);
    }

    fn legacy_process() -> crate::processes::Process {
        use crate::processes::{Process, Task, Usage};
        Process {
            pid: 42,
            usage: Usage {
                awake_ns_at_start: 100,
                user_ns: 101,
                system_ns: 102,
                energy_nj: 103,
                instructions: 104,
                super_instructions: 105,
                cycles: 106,
                idle_wakeups: 107,
                interrupt_wakeups: 108,
                pageins: 109,
                resident_bytes: 110,
                footprint_bytes: 111,
                disk_read_bytes: 112,
                disk_written_bytes: 113,
            },
            task: Task {
                threads: 2,
                faults: 3,
                context_switches: 4,
                mach_syscalls: 5,
                unix_syscalls: 6,
            },
        }
    }
}

#[cfg(test)]
mod battery_tests;

#[cfg(test)]
mod disks_tests;
