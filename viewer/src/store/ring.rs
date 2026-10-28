//! The store's ring adapter. It follows the newest run the recording registers and derives a
//! second and its Apps from each tick the run's ring publishes.

use std::io::ErrorKind;
use std::mem;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use collector::recording::{Registry, RegistryBuffer, read_identity};
use collector::ring::{
    Found, FoundEvent, IdentityObservation, RingReader, RunIdentity, Shape, Tick,
};
use collector::tracker::{EVENTS_PER_TICK_MAX, Event};
use persistence::{Error, NumberedEvent};

use super::apps::TickApps;
use super::native::{Input, OwnedFrame, PrepareError, Prepared};
use super::{Failure, MachineSecond, readings};
use crate::cancellation::Operation;
use crate::live_app_response::Sample;
use crate::live_apps::committed::Endpoint;
use crate::live_apps::{Frame, Publication, PublicationEvents};
use crate::recording::MachineMinute;
#[path = "native_workspace.rs"]
mod native_workspace;

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod handoff_tests;

/// The ticks one read takes in, well above the ticks a ring publishes between two steps.
pub(super) const TICKS_PER_READ_MAX: usize = 256;

/// The steps between two looks at the runs registry for a newer run than the one the store
/// follows, so a step between two of the ring's ticks reads no file.
pub(super) const FOLLOW_STEPS: u32 = 10;

/// The largest ring the store maps.
const MAPPED_BYTES_MAX: usize = 1 << 30;

pub(super) struct Ring {
    root: PathBuf,
    budget: Option<Arc<super::ByteBudget>>,
    runs: PathBuf,
    registry: RegistryBuffer,
    source: Option<Source>,
    terminal: Option<super::recording::Terminal>,
    /// A second read from the ring that the store could not take yet, and whether the App rule
    /// derived its Apps, which the source's sample holds, or why it refused them.
    pending: Option<(MachineSecond, Result<(), String>)>,
    /// The steps since the store last looked at the runs registry, and what it found there.
    unlooked_steps: u32,
    followed: Result<(), Failure>,
    registered: bool,
    /// Whether the ring the store follows published a tick since the store last looked.
    published: bool,
    delta: Option<Delta>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Delta {
    Covered,
    Overrun,
}

enum EventCapture {
    Ready,
    TooMany,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Status {
    pub identity: RunIdentity,
    pub completed: bool,
    pub losses: persistence::Losses,
}

/// The ring of one run, and the room to take its ticks in.
struct Source {
    reader: RingReader,
    identity: RunIdentity,
    complete: bool,
    tick: Tick,
    /// The identities of the processes of the tick just read, where the ring holds them.
    identities: Vec<IdentityObservation>,
    /// The events of the tick just read, with their identities where the ring holds them.
    events: Vec<(u64, u64, Event)>,
    event_identities: Vec<IdentityObservation>,
    /// The tick read last and the one before it, each as a machine minute of one tick, so a
    /// second from the ring is derived as one from a sealed minute is.
    current: MachineMinute,
    previous: MachineMinute,
    /// The App rule's frame of the tick read last and of the one before it the rule took.
    frame: Option<Box<Frame>>,
    previous_frame: Option<Box<Frame>>,
    previous_endpoint: Option<Endpoint>,
    /// The App rule's sample of the tick read last.
    sample: Option<Box<Sample>>,
    native_pending: Option<Prepared>,
    process_names: Option<super::payload::ProcessNames>,
    native_frame: Option<OwnedFrame>,
    native_events_ready: bool,
    native_workspace: Option<super::ByteReservation>,
    /// Saved minute capacities exceeding the two existing native machine buffers.
    terminal_workspace: Option<super::ByteReservation>,
    machine_workspace_bytes: usize,
    losses: persistence::Losses,
}

impl Ring {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            budget: None,
            runs: root.join("runs"),
            root,
            registry: [0; _],
            source: None,
            terminal: None,
            pending: None,
            unlooked_steps: FOLLOW_STEPS,
            followed: Ok(()),
            registered: false,
            published: false,
            delta: None,
        }
    }

    pub(super) fn with_budget(root: PathBuf, budget: Arc<super::ByteBudget>) -> Self {
        let mut ring = Self::new(root);
        ring.budget = Some(budget);
        ring
    }

    /// Prepares at most one publication so its payload sees the newest accepted App names.
    pub(super) fn read_prepared(
        &mut self,
        budget: &Arc<super::ByteBudget>,
        operation: &Operation,
        previous_apps: Option<&super::AppsSecond>,
        mut push: impl FnMut(&RunIdentity, &mut Prepared) -> bool,
    ) -> Result<super::native::NativeRead, PrepareError> {
        use super::native::NativeRead;
        assert!(
            self.budget
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, budget))
        );
        let Some(source) = &mut self.source else {
            return Ok(NativeRead::Idle);
        };
        assert!(source.native_workspace.is_some());
        operation.check().map_err(PrepareError::Source)?;
        if source.native_pending.is_none() {
            match source.read_tick().map_err(|error| {
                source.complete = false;
                PrepareError::Source(error.into())
            })? {
                Found::Publication { publication, .. } => {
                    self.published = true;
                    source.begin_native(publication)?;
                }
                Found::Missed { .. } => {
                    source.complete = false;
                    if self.delta.is_some() {
                        self.delta = Some(Delta::Overrun);
                    }
                    return Ok(NativeRead::Missed);
                }
                Found::Nothing => return Ok(NativeRead::Idle),
            }
        }
        source.prepare_native(budget, operation, previous_apps)?;
        let prepared = source
            .native_pending
            .as_mut()
            .unwrap_or_else(|| unreachable!("native candidate"));
        prepared.apps.metadata.origin = super::native::Origin::Live(source.losses);
        if !push(&source.identity, prepared) {
            return Ok(NativeRead::Blocked);
        }
        let mut accepted = source
            .native_pending
            .take()
            .unwrap_or_else(|| unreachable!("accepted native candidate"));
        accepted.commit_names(
            source
                .process_names
                .as_mut()
                .unwrap_or_else(|| unreachable!("native captured-name namespace")),
        );
        source.native_frame = accepted.into_frame();
        mem::swap(&mut source.current, &mut source.previous);
        Ok(NativeRead::Accepted)
    }

    pub(super) fn accept(&mut self, terminal: Option<super::recording::Terminal>) {
        assert!(self.source.is_none());
        self.terminal = terminal;
    }

    /// Hands `push` the run, the second and the Apps of each tick the ring published since the
    /// last read, oldest first, until the ring has no more or `push` refuses one, which the next
    /// read hands it again.
    pub(super) fn read(
        &mut self,
        mut push: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>) -> bool,
    ) -> Result<(), Failure> {
        let Some(source) = &mut self.source else {
            return Ok(());
        };

        for _ in 0..TICKS_PER_READ_MAX {
            if self.pending.is_none() {
                match source.read_tick() {
                    Ok(Found::Publication { publication, .. }) => {
                        self.published = true;
                        if let Some(second) = source.second(publication) {
                            self.pending = Some((second, source.apps(publication)));
                        } else {
                            source.complete = false;
                        }
                    }
                    Ok(Found::Missed { .. }) => {
                        source.complete = false;
                        if self.delta.is_some() {
                            self.delta = Some(Delta::Overrun);
                            return Ok(());
                        }
                        continue;
                    }
                    Ok(Found::Nothing) => return Ok(()),
                    Err(error) => {
                        source.complete = false;
                        return Err(Failure::Ring(error.kind()));
                    }
                }
            }

            let Some((second, apps)) = &self.pending else {
                continue;
            };
            let apps = match apps {
                Ok(()) => match source.sample.as_deref() {
                    Some(sample) => TickApps::Derived(sample),
                    None => unreachable!("the App rule leaves a sample where it took the tick"),
                },
                Err(error) => TickApps::Refused(error),
            };
            if !push(&source.identity, second, apps) {
                return Ok(());
            }
            self.pending = None;
        }

        Ok(())
    }

    /// Follows the newest run the recording registers, from the oldest tick its ring holds,
    /// once that ring can be read. Until then the store follows the run it followed before. It
    /// looks at the registry once in `FOLLOW_STEPS` steps and otherwise answers what it found
    /// there, so the steps between two of the ring's ticks read no file. A run starts only once
    /// the collector of the run before it stops, so while the ring the store follows publishes,
    /// the store skips the look unless the last one failed.
    pub(super) fn follow(&mut self) -> Result<(), Failure> {
        if self.unlooked_steps < FOLLOW_STEPS {
            self.unlooked_steps += 1;
            return self.followed;
        }

        self.unlooked_steps = 1;
        let published = mem::replace(&mut self.published, false);
        if published {
            if self.followed.is_ok() {
                return self.followed;
            }
        }

        self.followed = self.follow_newest();
        self.followed
    }

    pub(super) fn caught_up(&mut self) -> Result<bool, Failure> {
        self.caught_up_read(RingReader::publication_newest)
    }

    pub(super) fn caught_up_read(
        &mut self,
        read: impl FnOnce(&RingReader) -> std::io::Result<u64>,
    ) -> Result<bool, Failure> {
        if self.reload_required() {
            return Ok(false);
        }
        if self.pending.is_some() {
            return Ok(false);
        }
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.native_pending.is_some())
        {
            return Ok(false);
        }

        let Some(source) = &mut self.source else {
            return Ok(true);
        };

        let newest = match read(&source.reader) {
            Ok(newest) => newest,
            Err(error) => {
                source.complete = false;
                return Err(Failure::Ring(error.kind()));
            }
        };

        Ok(source.reader.next_publication() > newest)
    }

    pub(super) fn complete(&self, recording: &collector::ring::Identity) -> bool {
        if self.followed.is_err() {
            return false;
        }
        if !self.registered {
            return false;
        }

        let Some(source) = &self.source else {
            return false;
        };
        if source.identity.recording != *recording {
            return false;
        }

        source.complete
    }

    #[cfg(test)]
    pub(super) fn name_observation(&self) -> Option<(usize, String, usize)> {
        self.source
            .as_ref()?
            .process_names
            .as_ref()
            .map(super::payload::ProcessNames::observation)
    }

    pub(super) fn identity(&self) -> Option<RunIdentity> {
        self.source.as_ref().map(|source| source.identity)
    }

    pub(super) fn status(&self) -> Option<Status> {
        self.source.as_ref().map(|source| Status {
            identity: source.identity,
            completed: source.reader.completed(),
            losses: source.losses,
        })
    }

    /// Rechecks the source on show while keeping the current run's reader and rate predecessor.
    pub(super) fn resume(&mut self) -> Result<(), Failure> {
        self.finish_resume();
        self.unlooked_steps = FOLLOW_STEPS;
        self.published = false;
        self.follow()
    }

    /// Reuses a checkpoint only when this generation still retains every unread publication.
    pub(super) fn covers(&mut self, checkpoint: &super::Newest) -> Result<bool, Failure> {
        self.registered = false;
        let Some(source) = &self.source else {
            return Ok(false);
        };
        if source.identity != checkpoint.run {
            return Ok(false);
        }
        let recording =
            read_identity(&self.root).map_err(|error| Failure::Recording(error.kind()))?;
        if recording != checkpoint.run.recording {
            return Err(Failure::ForeignRecording);
        }
        let registry = Registry::read_into(&self.runs, &mut self.registry)
            .map_err(|error| Failure::Registry(error.kind()))?;
        let Some(newest) = registry.runs().last() else {
            return Ok(false);
        };
        let newest = newest.map_err(|error| Failure::Registry(error.kind()))?;
        if newest.identity != source.identity {
            return Ok(false);
        }
        if newest.ring_name != source.reader.name() {
            return Ok(false);
        }
        if source.reader.identity() != Some(source.identity) {
            return Err(Failure::ChangedIdentity);
        }
        let latest = source
            .reader
            .publication_newest()
            .map_err(|error| Failure::Ring(error.kind()))?;
        if latest < checkpoint.publication {
            return Err(Failure::ChangedIdentity);
        }
        let first = latest
            .saturating_sub(source.reader.shape().slot_count)
            .saturating_add(1)
            .max(source.reader.publication_first());
        let cursor = match &self.pending {
            Some((second, _)) => second.publication,
            None => source.native_pending.as_ref().map_or_else(
                || source.reader.next_publication(),
                |prepared| prepared.observation.stamp.publication,
            ),
        };
        if cursor
            != checkpoint
                .publication
                .checked_add(1)
                .ok_or(Failure::TickShape)?
        {
            return Ok(false);
        }
        if cursor < first {
            return Ok(false);
        }
        self.unlooked_steps = 1;
        self.followed = Ok(());
        self.registered = true;
        self.delta = Some(Delta::Covered);
        Ok(true)
    }

    pub(super) fn reload_required(&self) -> bool {
        self.delta == Some(Delta::Overrun)
    }

    pub(super) fn finish_resume(&mut self) {
        self.delta = None;
    }

    /// Rechecks the recording identity and follows the newest registered generation.
    fn follow_newest(&mut self) -> Result<(), Failure> {
        self.registered = false;
        let recording =
            read_identity(&self.root).map_err(|error| Failure::Recording(error.kind()))?;
        if let Some(source) = &self.source {
            if source.identity.recording != recording {
                return Err(Failure::ForeignRecording);
            }
        }

        if self.follow_newest_followed()? {
            self.registered = true;
            return Ok(());
        }
        if !self.caught_up()? {
            return Ok(());
        }

        let registry = Registry::read_into(&self.runs, &mut self.registry)
            .map_err(|error| Failure::Registry(error.kind()))?;
        let Some(newest) = registry.runs().last() else {
            return Ok(());
        };
        let newest = newest.map_err(|error| Failure::Registry(error.kind()))?;
        if newest.identity.recording != recording {
            return Err(Failure::ForeignRecording);
        }

        let reader = match RingReader::open_generation(newest.ring_name) {
            Ok(reader) => reader,
            Err(error) => {
                if error.kind() == ErrorKind::NotFound {
                    return Ok(());
                }
                return Err(Failure::Ring(error.kind()));
            }
        };
        // A ring from before rings carried their run's identity cannot be told from another
        // run's, so the store does not follow it.
        let Some(identity) = reader.identity() else {
            return Ok(());
        };
        if identity != newest.identity {
            return Err(Failure::ChangedIdentity);
        }
        if reader.mapped_size_bytes() > MAPPED_BYTES_MAX {
            return Err(Failure::MappingSize);
        }

        if let Some(source) = &mut self.source {
            if source.identity == identity {
                source.follow(reader)?;
                self.registered = true;
                return Ok(());
            }
        }

        let mut source = Source::open(reader, identity, self.budget.as_ref())?;
        source.accept(&mut self.terminal, self.budget.as_ref())?;
        self.source = Some(source);
        self.registered = true;
        Ok(())
    }

    /// Whether the registry's newest run is the run the store follows.
    fn follow_newest_followed(&mut self) -> Result<bool, Failure> {
        let Some(source) = &self.source else {
            return Ok(false);
        };
        let registry = Registry::read_into(&self.runs, &mut self.registry)
            .map_err(|error| Failure::Registry(error.kind()))?;
        let Some(newest) = registry.runs().last() else {
            return Ok(false);
        };

        let newest = newest.map_err(|error| Failure::Registry(error.kind()))?;
        if newest.identity != source.identity {
            return Ok(false);
        }
        Ok(newest.ring_name == source.reader.name())
    }

    /// Sleeps until the ring publishes past what the store read, for at most `timeout`. Where
    /// the store follows no ring yet, or holds a second it could not take, the ring would not
    /// wake it, so it sleeps all of `timeout`.
    pub(super) fn wait(&self, timeout: Duration) -> Result<(), Failure> {
        let Some(source) = &self.source else {
            thread::sleep(timeout);
            return Ok(());
        };
        if self.pending.is_some() || source.native_pending.is_some() {
            thread::sleep(timeout);
            return Ok(());
        }

        if let Err(error) = source.reader.wait(timeout) {
            thread::sleep(timeout);
            return Err(Failure::Ring(error.kind()));
        }
        Ok(())
    }
}

impl Source {
    fn follow(&mut self, mut reader: RingReader) -> Result<(), Failure> {
        if reader.shape() != self.reader.shape() {
            return Err(Failure::TickShape);
        }

        let next = self.reader.next_publication();
        if reader.publication_first() < next {
            return Err(Failure::ChangedIdentity);
        }

        if next < reader.next_publication() {
            self.complete = false;
        }
        reader
            .seek_publication(next)
            .map_err(|error| Failure::Ring(error.kind()))?;
        assert!(self.native_pending.is_none());
        self.reader = reader;
        self.process_names = None;

        Ok(())
    }

    fn accept(
        &mut self,
        terminal: &mut Option<super::recording::Terminal>,
        budget: Option<&Arc<super::ByteBudget>>,
    ) -> Result<(), Failure> {
        let Some(incoming) = terminal.as_ref() else {
            return Ok(());
        };
        if incoming.run != self.identity {
            drop(terminal.take());
            return Ok(());
        }
        assert_eq!(incoming.machine.ticks.len(), 1);
        let publication = incoming.machine.ticks[0].publication;
        let next = publication.checked_add(1).ok_or(Failure::TickShape)?;
        let complete = next >= self.reader.next_publication();
        let machine = if incoming.machine_reservation.is_some() {
            None
        } else {
            budget
                .map(|budget| {
                    budget
                        .reserve(incoming.machine.heap_bytes())
                        .ok_or(Failure::Budget)
                })
                .transpose()?
        };
        let frame_reservation = if incoming
            .process
            .as_ref()
            .is_some_and(crate::recording::Before::is_admitted)
        {
            None
        } else {
            budget
                .map(|budget| {
                    incoming
                        .process
                        .as_ref()
                        .map(|before| budget.reserve(before.bytes()).ok_or(Failure::Budget))
                        .transpose()
                })
                .transpose()?
                .flatten()
        };
        self.reader
            .seek_publication(next)
            .map_err(|error| Failure::Ring(error.kind()))?;
        let mut terminal = terminal
            .take()
            .unwrap_or_else(|| unreachable!("admitted terminal"));
        self.complete = complete;
        self.previous = terminal.machine;
        self.terminal_workspace = terminal.machine_reservation.take().or(machine);
        self.shrink_terminal_workspace();
        if let Some(mut before) = terminal.process {
            let frame_reservation = before.take_reservation().or(frame_reservation);
            let (endpoint, frame) = before.into_parts();
            self.previous_endpoint = Some(endpoint);
            match frame_reservation {
                Some(reservation) => {
                    self.native_frame = Some(OwnedFrame::from_reserved(frame, reservation));
                }
                None => self.previous_frame = Some(frame),
            }
        }

        Ok(())
    }

    /// The run's ring with room for its ticks, allocated once, as the run is followed.
    fn open(
        reader: RingReader,
        identity: RunIdentity,
        budget: Option<&Arc<super::ByteBudget>>,
    ) -> Result<Self, Failure> {
        let shape = reader.shape();
        let native_workspace = budget
            .map(|budget| {
                let bytes = native_workspace::bytes(&shape).map_err(|_| Failure::TickShape)?
                    + size_of::<Self>();
                budget.reserve(bytes).ok_or(Failure::Budget)
            })
            .transpose()?;
        let tick = persistence::tick_buffer(&shape).map_err(|_| Failure::TickShape)?;

        let processes = count(shape.process_capacity)?;
        let complete = reader.next_publication() == reader.publication_first();

        let current = minute_of(&shape)?;
        let previous = minute_of(&shape)?;
        let machine_workspace_bytes = current.heap_bytes() + previous.heap_bytes();
        Ok(Self {
            reader,
            identity,
            complete,
            tick,
            identities: Vec::with_capacity(processes),
            events: Vec::with_capacity(EVENTS_PER_TICK_MAX),
            event_identities: Vec::with_capacity(EVENTS_PER_TICK_MAX),
            current,
            previous,
            frame: None,
            previous_frame: None,
            previous_endpoint: None,
            sample: None,
            native_pending: None,
            process_names: None,
            native_frame: None,
            native_events_ready: false,
            native_workspace,
            terminal_workspace: None,
            machine_workspace_bytes,
            losses: persistence::Losses::default(),
        })
    }

    fn begin_native(&mut self, publication: u64) -> Result<(), PrepareError> {
        if self.tick.times.is_none() {
            self.losses.unknown = true;
        }
        self.current.clear();
        self.current.push_tick(publication, &self.tick);
        self.shrink_terminal_workspace();
        self.native_pending = Some(Prepared::new(&Publication {
            reader: &self.reader,
            tick: &self.tick,
            identities: &self.identities,
            publication,
        })?);
        self.native_events_ready = false;
        Ok(())
    }

    fn shrink_terminal_workspace(&mut self) {
        if let Some(reservation) = &mut self.terminal_workspace {
            let bytes = (self.current.heap_bytes() + self.previous.heap_bytes())
                .saturating_sub(self.machine_workspace_bytes);
            reservation.shrink_to(bytes);
        }
    }

    fn prepare_native_names(
        &mut self,
        budget: &Arc<super::ByteBudget>,
    ) -> Result<(), PrepareError> {
        if let Some(names) = &self.process_names {
            if !Arc::ptr_eq(names.budget(), budget) {
                return Err(PrepareError::Source(Error::Bound("name budget owner")));
            }

            return Ok(());
        }

        let dictionary = self
            .reader
            .dictionary_capacity()
            .map_or(0, |capacity| capacity.entries);
        let dictionary = usize::try_from(dictionary)
            .map_err(|_| PrepareError::Source(Error::Bound("native name count")))?;
        let inherited = self
            .native_frame
            .as_ref()
            .map_or(0, |frame| frame.get().program_count());
        let count = dictionary
            .checked_add(inherited)
            .ok_or(PrepareError::Source(Error::Bound("native name count")))?;
        self.process_names = Some(
            super::payload::ProcessNames::new(count, budget)
                .map_err(|_| PrepareError::Admission(super::native::Admission::NameIndex))?,
        );
        Ok(())
    }

    fn prepare_native(
        &mut self,
        budget: &Arc<super::ByteBudget>,
        operation: &Operation,
        previous_apps: Option<&super::AppsSecond>,
    ) -> Result<(), PrepareError> {
        if self.native_frame.is_none() {
            self.native_frame = OwnedFrame::adopt(&mut self.previous_frame, budget)?;
        }
        self.prepare_native_names(budget)?;
        if !self.native_events_ready {
            match self.capture_events().map_err(PrepareError::Source)? {
                EventCapture::Ready => {}
                EventCapture::TooMany => {
                    self.events.clear();
                    self.event_identities.clear();
                    self.native_pending
                        .as_mut()
                        .unwrap_or_else(|| unreachable!("candidate"))
                        .refuse_events();
                }
            }
            self.native_events_ready = true;
        }
        let prepared = self
            .native_pending
            .as_mut()
            .unwrap_or_else(|| unreachable!("native candidate"));
        if prepared.ready() {
            return operation.check().map_err(PrepareError::Source);
        }
        let numbered: Vec<_> = self
            .events
            .iter()
            .map(|(number, tick, event)| NumberedEvent {
                number: *number,
                tick: *tick,
                event,
            })
            .collect();
        prepared.resume(
            &Input {
                names: self
                    .process_names
                    .as_ref()
                    .unwrap_or_else(|| unreachable!("admitted native name namespace")),
                source: Publication {
                    reader: &self.reader,
                    tick: &self.tick,
                    identities: &self.identities,
                    publication: prepared.observation.stamp.publication,
                },
                events: PublicationEvents {
                    events: &numbered,
                    identities: &self.event_identities,
                },
                current: &self.current,
                previous: &self.previous,
                previous_frame: self.native_frame.as_ref().map(OwnedFrame::get),
                previous_apps,
            },
            budget,
            operation,
        )
    }

    /// Reads the ring's next tick, with its processes' identities where the ring holds them.
    fn read_tick(&mut self) -> std::io::Result<Found> {
        persistence::work::ingestion::record_publication(
            self.identity,
            self.reader.next_publication(),
            persistence::work::ingestion::Kind::RingReadAttempt,
        );
        let found = if self.reader.dictionary_capacity().is_some() {
            self.reader
                .read_identity_into(&mut self.tick, &mut self.identities)
        } else {
            self.identities.clear();
            self.reader.read_into(&mut self.tick)
        }?;
        if let Found::Publication { publication, .. } = found {
            persistence::work::ingestion::record_publication(
                self.identity,
                publication,
                persistence::work::ingestion::Kind::RingRead,
            );
        }
        if let Found::Missed { first, end } = found {
            self.losses.publications += end - first;
        }

        Ok(found)
    }

    /// Runs the App rule on the tick just read, the ring's publication `publication`, with its
    /// rates since the tick before it the rule took, and leaves its sample in `sample`, or says
    /// why the rule refused the tick.
    fn apps(&mut self, publication: u64) -> Result<(), String> {
        if let Err(error) = self.apps_capture(publication) {
            persistence::work::ingestion::record_publication(
                self.identity,
                publication,
                persistence::work::ingestion::Kind::AttemptFailed,
            );
            self.sample = None;
            self.previous_frame = None;
            self.previous_endpoint = None;
            return Err(error.to_string());
        }

        let frame = self.frame.as_deref().unwrap_or_else(|| unreachable!());
        self.previous_endpoint = Some(Endpoint {
            identity: self.identity,
            stamp: *frame.stamp(),
            run_tick: frame.run_tick(),
            wall_ns: frame.wall_ns(),
            usage_refused: frame.usage_refused,
            usage_gone: frame.usage_gone,
            events_first: self.tick.events.start,
            events_end: self.tick.events.end,
        });
        mem::swap(&mut self.frame, &mut self.previous_frame);
        Ok(())
    }

    /// The App rule's frame of the tick just read, with the programs its departed processes ran,
    /// and the sample of that frame after the frame before it.
    fn apps_capture(&mut self, publication: u64) -> persistence::Result<()> {
        Frame::capture(
            &Publication {
                reader: &self.reader,
                tick: &self.tick,
                identities: &self.identities,
                publication,
            },
            &mut self.frame,
        )?;
        self.apps_capture_events()?;

        let numbered: Vec<NumberedEvent<'_>> = self
            .events
            .iter()
            .map(|(number, tick, event)| NumberedEvent {
                number: *number,
                tick: *tick,
                event,
            })
            .collect();
        let Some(frame) = self.frame.as_deref_mut() else {
            unreachable!("a frame the App rule took holds the tick");
        };
        frame.capture_exits(
            &Publication {
                reader: &self.reader,
                tick: &self.tick,
                identities: &self.identities,
                publication,
            },
            &PublicationEvents {
                events: &numbered,
                identities: &self.event_identities,
            },
        )?;

        let previous = self.previous_frame.as_deref().filter(|_| {
            self.previous_endpoint.as_ref().is_some_and(|previous| {
                frame
                    .observation()
                    .can_use_predecessor(&previous.observation())
            })
        });
        Sample::capture(frame, previous, &mut self.sample)
    }

    /// Reads the events of the tick just read into `events`, with their identities where the
    /// ring holds them. An event the ring no longer holds is left out.
    fn apps_capture_events(&mut self) -> persistence::Result<()> {
        match self.capture_events()? {
            EventCapture::Ready => Ok(()),
            EventCapture::TooMany => Err(Error::Bound("store tick events")),
        }
    }

    fn capture_events(&mut self) -> persistence::Result<EventCapture> {
        self.events.clear();
        self.event_identities.clear();
        let mut losses = self.losses;
        if self.tick.events.is_empty() {
            return Ok(EventCapture::Ready);
        }

        self.reader.seek_event(self.tick.events.start)?;
        let identities = self.reader.dictionary_capacity().is_some();
        for _ in 0..=EVENTS_PER_TICK_MAX {
            if self.reader.next_event() >= self.tick.events.end {
                self.losses = losses;
                return Ok(EventCapture::Ready);
            }

            let mut identity = IdentityObservation::EMPTY;
            let found = if identities {
                self.reader.read_event_identity(&mut identity)?
            } else {
                self.reader.read_event()?
            };
            match found {
                FoundEvent::Nothing => {
                    losses.unknown = true;
                    self.losses = losses;
                    return Ok(EventCapture::Ready);
                }
                FoundEvent::Missed { first, end } => {
                    losses.events += end.min(self.tick.events.end) - first;
                }
                FoundEvent::Event {
                    number,
                    tick,
                    event,
                } => {
                    if self.events.len() == EVENTS_PER_TICK_MAX {
                        self.losses = losses;
                        return Ok(EventCapture::TooMany);
                    }
                    self.events.push((number, tick, event));
                    if identities {
                        self.event_identities.push(identity);
                    }
                }
            }
            if self.reader.next_event() >= self.tick.events.end {
                self.losses = losses;
                return Ok(EventCapture::Ready);
            }
        }

        Err(Error::Bound("store tick events"))
    }

    /// The second of the tick just read, the ring's publication `publication`, with its rates
    /// over the interval since the tick read before it.
    fn second(&mut self, publication: u64) -> Option<MachineSecond> {
        persistence::work::ingestion::record_publication(
            self.identity,
            publication,
            persistence::work::ingestion::Kind::MachineReduction,
        );
        self.current.clear();
        self.current.push_tick(publication, &self.tick);

        let Some(reading) = readings(&self.current).next() else {
            unreachable!("a minute holds the tick just pushed");
        };
        let previous = readings(&self.previous).next();
        let second = MachineSecond::of(&reading, previous.as_ref());

        mem::swap(&mut self.current, &mut self.previous);
        second
    }
}

/// A machine minute with room for one tick of a ring of `shape`, so taking a tick in
/// allocates nothing.
fn minute_of(shape: &Shape) -> Result<MachineMinute, Failure> {
    let states = count(shape.state_capacity)?;
    let core_states = count(shape.core_capacity)?
        .checked_mul(states)
        .ok_or(Failure::TickShape)?;

    Ok(MachineMinute {
        ticks: Vec::with_capacity(1),
        cpus: Vec::with_capacity(count(shape.cpu_capacity)?),
        memory: Vec::with_capacity(1),
        interfaces: Vec::with_capacity(count(shape.interface_capacity)?),
        gpu_states: Vec::with_capacity(states),
        core_states: Vec::with_capacity(core_states),
        fans: Vec::with_capacity(count(shape.fan_capacity)?),
    })
}

fn count(capacity: u64) -> Result<usize, Failure> {
    usize::try_from(capacity).map_err(|_| Failure::TickShape)
}
