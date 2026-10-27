//! Owned live publication metadata for the shared pure App aggregate.
use collector::processes::Usage;
use collector::ring::{
    IdentityObservation, IdentityUnavailable, ProcessCreation, ProgramBytes, RingReader,
    RunIdentity, Tick,
};
use persistence::{Error, NumberedEvent, Result};

use crate::apps::{self, Stamp, Unavailable};

/// Each selected/predecessor pair stays inside the aggregate's 64 MiB raw-byte budget.
pub const PROGRAM_FRAME_BYTES_MAX: usize = apps::PROGRAM_BYTES_MAX / 2;
/// Native Frame's maximum retained ownership, including every event descriptor and the arena.
pub(crate) const FRAME_BYTES_MAX: usize = size_of::<Frame>()
    + PROGRAM_FRAME_BYTES_MAX
    + apps::PROCESS_INTERVALS_MAX * size_of::<Member>()
    + collector::tracker::EVENTS_PER_TICK_MAX
        * (size_of::<Start>() + size_of::<Exec>() + size_of::<Exit>());
const _: () = assert!(FRAME_BYTES_MAX <= 48 * 1024 * 1024);
/// The aggregate owns no program bytes. Its result Vec is exact-reserved; both ordered
/// trees have at most one node per five entries plus their root in CI's Rust 1.98.
pub(crate) const AGGREGATE_WORK_BYTES_MAX: usize =
    aggregate_work_bytes(apps::PROCESS_INTERVALS_MAX);

const fn aggregate_work_bytes(count: usize) -> usize {
    size_of::<apps::Aggregate<'static>>()
        + count * size_of::<apps::ProcessTotals<'static>>()
        + (count.div_ceil(5) + 1)
            * (11 * (size_of::<apps::AppKey<'static>>() + size_of::<apps::AppTotals>())
                + 11 * size_of::<(i32, Option<u64>)>()
                + 24 * size_of::<usize>()
                + 64)
}
/// Join-view payload bound; the shared aggregate separately bounds its result/work storage.
pub const TRANSIENT_VIEW_BYTES_MAX: usize = apps::PROCESS_INTERVALS_MAX
    * (2 * size_of::<Option<ProgramBytes<'static>>>()
        + 2 * size_of::<apps::ProcessObservation<'static>>()
        + size_of::<apps::ProcessInterval<'static>>());

/// Exact observation metadata, independent of captured process and program ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    pub recording: [u64; 2],
    pub stamp: Stamp,
    pub run_tick: u64,
    pub wall_ns: Option<u64>,
}

impl Observation {
    /// Whether metadata permits captured predecessor evidence; this does not validate its rows.
    pub(crate) fn can_use_predecessor(&self, previous: &Self) -> bool {
        if previous.recording != self.recording {
            return false;
        }

        if previous.stamp.boot != self.stamp.boot {
            return false;
        }

        if previous.stamp.run != self.stamp.run {
            return false;
        }

        if previous.stamp.publication.checked_add(1) != Some(self.stamp.publication) {
            return false;
        }

        let (Some(before), Some(after)) = (previous.stamp.continuous_ns, self.stamp.continuous_ns)
        else {
            return false;
        };

        before < after
    }

    /// Whether that predecessor also establishes a consecutive App counter interval.
    pub(crate) fn takes_rates_from(&self, previous: &Self) -> bool {
        if !self.can_use_predecessor(previous) {
            return false;
        }

        if previous.run_tick.checked_add(1) != Some(self.run_tick) {
            return false;
        }

        if previous.wall_ns.is_none() {
            return false;
        }

        let (Some(before), Some(after)) = (previous.stamp.awake_ns, self.stamp.awake_ns) else {
            return false;
        };

        before <= after
    }
}

#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct Frame {
    identity: RunIdentity,
    stamp: Stamp,
    run_tick: u64,
    wall_ns: Option<u64>,
    members: Vec<Member>,
    starts: Vec<Start>,
    execs: Vec<Exec>,
    exits: Vec<Exit>,
    arena: Vec<u8>,
    pub usage_refused: u32,
    pub usage_gone: u32,
}

#[cfg_attr(test, derive(Debug, PartialEq))]
struct Member {
    pid: i32,
    creation: Option<ProcessCreation>,
    program: std::result::Result<ProgramSpan, IdentityUnavailable>,
    usage: std::result::Result<Usage, Unavailable>,
    /// The GPU time of the process's GPU clients, none where the tick read none for it.
    gpu_ns: Option<u64>,
    threads: Option<u64>,
}

#[cfg_attr(test, derive(Debug, PartialEq))]
struct Exit {
    number: u64,
    pid: i32,
    creation: Option<ProcessCreation>,
    usage: std::result::Result<Usage, Unavailable>,
    ambiguous: bool,
}

#[cfg_attr(test, derive(Debug, PartialEq))]
struct Start {
    number: u64,
    pid: i32,
    creation: Option<ProcessCreation>,
    program: std::result::Result<ProgramSpan, IdentityUnavailable>,
    ambiguous: bool,
    exec_number: Option<u64>,
}

#[cfg_attr(test, derive(Debug, PartialEq))]
struct Exec {
    number: u64,
    pid: i32,
    creation: Option<ProcessCreation>,
    program: std::result::Result<ProgramSpan, IdentityUnavailable>,
}

pub struct PublicationEvents<'source> {
    pub events: &'source [NumberedEvent<'source>],
    pub identities: &'source [IdentityObservation],
}

/// Keeps the model's original input error separate from join and presentation failures.
#[derive(Debug)]
pub(crate) enum AggregateError {
    Input(apps::Error),
    View(apps::Error),
    Presentation(Error),
}

impl AggregateError {
    pub(crate) fn legacy(self) -> Error {
        match self {
            Self::Input(_) => Error::Bound("live App aggregate input"),
            Self::View(_) => Error::Bound("live App exit interval bound"),
            Self::Presentation(error) => error,
        }
    }

    /// Joint work limits do not invalidate either captured Frame's source input.
    pub(crate) fn retains_input(&self) -> bool {
        match self {
            Self::Input(error) | Self::View(error) => {
                matches!(error, apps::Error::ProcessBound | apps::Error::ProgramBound)
            }
            Self::Presentation(Error::Bound(_)) => true,
            Self::Presentation(Error::Engine(_) | Error::Io(_)) => false,
        }
    }
}

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq))]
struct ProgramSpan {
    start: usize,
    name_end: usize,
    end: usize,
}

impl ProgramSpan {
    fn capture(arena: &mut Vec<u8>, program: &ProgramBytes<'_>) -> Self {
        let app_path = program.app_path.unwrap_or_default();
        let spare = arena.capacity() - arena.len();
        assert!(program.executable_name.len() <= spare);
        assert!(app_path.len() <= spare - program.executable_name.len());

        let start = arena.len();
        arena.extend_from_slice(program.executable_name);
        let name_end = arena.len();
        arena.extend_from_slice(app_path);

        Self {
            start,
            name_end,
            end: arena.len(),
        }
    }
}

/// All references must come from the same successful read of this exact reader handle.
/// Capture before reusing its tick/identity buffers or advancing its publication cursor.
pub struct Publication<'source> {
    pub reader: &'source RingReader,
    pub tick: &'source Tick,
    pub identities: &'source [IdentityObservation],
    pub publication: u64,
}

impl Publication<'_> {
    pub(crate) fn record_work(&self, kind: persistence::work::ingestion::Kind) {
        if !persistence::work::ingestion::is_active() {
            return;
        }

        if let Some(identity) = self.reader.identity() {
            persistence::work::ingestion::record_publication(identity, self.publication, kind);
        } else {
            persistence::work::ingestion::record_content(kind, 0);
        }
    }
}

const _: () = {
    assert!(PROGRAM_FRAME_BYTES_MAX * 2 == apps::PROGRAM_BYTES_MAX);
    assert!(apps::PROCESS_INTERVALS_MAX < 65_536);
};

impl Frame {
    /// Projects only this frame's captured metadata, without another retained owner.
    pub(crate) fn observation(&self) -> Observation {
        Observation {
            recording: self.identity.recording.0,
            stamp: self.stamp,
            run_tick: self.run_tick,
            wall_ns: self.wall_ns,
        }
    }

    /// Owns copied source bytes and numeric readings. A refusal leaves output empty.
    pub fn capture(source: &Publication<'_>, output: &mut Option<Box<Self>>) -> Result<()> {
        *output = None;
        source.record_work(persistence::work::ingestion::Kind::FrameCaptureAttempt);
        validate(source)?;
        let identity = source
            .reader
            .identity()
            .ok_or(Error::Bound("live App run identity"))?;
        let count = if source.reader.dictionary_capacity().is_some() {
            source.identities.len()
        } else {
            source.tick.processes.len()
        };
        let mut frame = Box::new(Self {
            identity,
            stamp: Stamp {
                boot: identity.boot.0,
                run: identity.run.0,
                publication: source.publication,
                continuous_ns: source.tick.times.map(|times| times.continuous_ns),
                awake_ns: source.tick.times.map(|times| times.awake_ns),
            },
            run_tick: source.tick.run_tick,
            wall_ns: source.tick.times.map(|times| times.wall_ns),
            members: Vec::with_capacity(count),
            starts: Vec::new(),
            execs: Vec::new(),
            exits: Vec::new(),
            arena: Vec::with_capacity(capture::program_bytes(source, source.identities)?),
            usage_refused: source.tick.processes_refused,
            usage_gone: source.tick.processes_gone,
        });
        capture::members(source, &mut frame)?;
        frame
            .members
            .sort_by_key(|member| (member.pid, member.creation.map(|value| value.unique_id)));
        if frame
            .members
            .windows(2)
            .any(|pair| pair[0].pid == pair[1].pid)
        {
            return Err(Error::Bound("duplicate live App PID"));
        }
        assert_eq!(frame.members.len(), count);
        assert!(frame.arena.len() <= PROGRAM_FRAME_BYTES_MAX);
        *output = Some(frame);
        source.record_work(persistence::work::ingestion::Kind::FrameCapture);

        Ok(())
    }

    /// Upper capacity charge for this exact publication, before Frame allocates any storage.
    pub(crate) fn capture_bytes(
        source: &Publication<'_>,
        events: &PublicationEvents<'_>,
    ) -> Result<usize> {
        validate(source)?;
        if events.events.len() > collector::tracker::EVENTS_PER_TICK_MAX {
            return Err(Error::Bound("live App exit event bound"));
        }
        let identified = source.reader.dictionary_capacity().is_some();
        let count = if identified {
            source.identities.len()
        } else {
            source.tick.processes.len()
        };
        let mut bytes = size_of::<Self>() + count * size_of::<Member>();
        if identified {
            if events.events.len() != events.identities.len() {
                return Err(Error::Bound("live App exit identity count"));
            }
            let mut arena = capture::program_bytes(source, source.identities)?;
            let mut programs = count;
            for (numbered, identity) in events.events.iter().zip(events.identities) {
                match numbered.event {
                    collector::tracker::Event::Exit { .. } => bytes += size_of::<Exit>(),
                    collector::tracker::Event::Start { .. } => bytes += size_of::<Start>(),
                    collector::tracker::Event::Exec { .. } => bytes += size_of::<Exec>(),
                }
                if matches!(
                    numbered.event,
                    collector::tracker::Event::Start { .. }
                        | collector::tracker::Event::Exec { .. }
                ) {
                    programs += 1;
                    arena = arena
                        .checked_add(capture::program_bytes(
                            source,
                            std::slice::from_ref(identity),
                        )?)
                        .ok_or(Error::Bound("live App program byte bound"))?;
                }
            }
            if arena > PROGRAM_FRAME_BYTES_MAX {
                return Err(Error::Bound("live App program byte bound"));
            }
            if programs > apps::PROCESS_INTERVALS_MAX {
                return Err(Error::Bound("live App program interval bound"));
            }
            bytes += arena;
        }
        Ok(bytes)
    }

    /// Existing join/aggregate capacities sized to these two admitted Frames.
    pub(crate) fn aggregate_work_bytes(&self, previous: Option<&Self>) -> usize {
        let previous = previous.unwrap_or(self);
        let programs = self.members.len()
            + self.starts.len()
            + self.execs.len()
            + previous.members.len()
            + previous.starts.len()
            + previous.execs.len();
        let selected = (self.members.len() + self.exits.len()).min(apps::PROCESS_INTERVALS_MAX);
        let bytes = programs * size_of::<Option<ProgramBytes<'_>>>()
            + (selected + previous.members.len()) * size_of::<apps::ProcessObservation<'_>>()
            + selected * size_of::<apps::ProcessInterval<'_>>()
            + aggregate_work_bytes(selected);
        assert!(bytes <= TRANSIENT_VIEW_BYTES_MAX + AGGREGATE_WORK_BYTES_MAX);
        bytes
    }

    /// Charges owned vector capacities and the frame itself to the live observation cache.
    #[must_use]
    pub fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.members.capacity() * size_of::<Member>()
            + self.starts.capacity() * size_of::<Start>()
            + self.execs.capacity() * size_of::<Exec>()
            + self.exits.capacity() * size_of::<Exit>()
            + self.arena.capacity()
    }

    /// Owns Start/Exec program bytes and final usage before event/dictionary buffers are reused.
    pub fn capture_exits(
        &mut self,
        source: &Publication<'_>,
        events: &PublicationEvents<'_>,
    ) -> Result<()> {
        events::capture(self, source, events)
    }

    #[must_use]
    pub fn identity(&self) -> &RunIdentity {
        &self.identity
    }

    #[must_use]
    pub fn stamp(&self) -> &Stamp {
        &self.stamp
    }

    #[must_use]
    pub fn run_tick(&self) -> u64 {
        self.run_tick
    }

    #[must_use]
    pub fn wall_ns(&self) -> Option<u64> {
        self.wall_ns
    }

    #[must_use]
    pub fn process_count(&self) -> usize {
        self.members.len()
    }

    /// Builds bounded transient views, consumed before their backing frames can change.
    /// Start/Exec identity is not complete, so interval membership remains uncertain.
    pub fn with_aggregate(
        &self,
        previous: Option<&Self>,
        consume: impl FnOnce(&apps::Aggregate<'_>) -> Result<()>,
    ) -> Result<()> {
        self.with_aggregate_typed(previous, consume)
            .map_err(AggregateError::legacy)
    }

    /// Validates the same borrowed input as aggregation without deriving App totals.
    pub(crate) fn validate_input_typed(
        &self,
        previous: Option<&Self>,
    ) -> std::result::Result<(), AggregateError> {
        views::with_aggregate_input(self, previous, |interval, processes| {
            apps::validate(interval, processes).map_err(AggregateError::Input)
        })
    }

    pub(crate) fn with_aggregate_typed(
        &self,
        previous: Option<&Self>,
        consume: impl FnOnce(&apps::Aggregate<'_>) -> Result<()>,
    ) -> std::result::Result<(), AggregateError> {
        persistence::work::ingestion::record_publication(
            self.identity,
            self.stamp.publication,
            persistence::work::ingestion::Kind::AppDerivation,
        );
        views::with_aggregate(self, previous, consume)
    }
}

fn validate(source: &Publication<'_>) -> Result<()> {
    if source.publication == 0 {
        return Err(Error::Bound("live App unpublished frame"));
    }

    if source.publication.checked_add(1) != Some(source.reader.next_publication()) {
        return Err(Error::Bound("live App publication association"));
    }
    let capacity = usize::try_from(source.reader.shape().process_capacity)
        .map_err(|_| Error::Bound("live App source capacity"))?;
    if source.tick.processes.len() > capacity {
        return Err(Error::Bound("live App numeric capacity"));
    }
    if source.identities.len() > capacity {
        return Err(Error::Bound("live App identity capacity"));
    }
    if source.tick.processes.len() > apps::PROCESS_INTERVALS_MAX {
        return Err(Error::Bound("live App process bound"));
    }
    if source.identities.len() > apps::PROCESS_INTERVALS_MAX {
        return Err(Error::Bound("live App process bound"));
    }
    if source.reader.dictionary_capacity().is_none() {
        if !source.identities.is_empty() {
            return Err(Error::Bound("legacy live App identity rows"));
        }
    }

    Ok(())
}

mod capture;
pub(crate) mod committed;
mod events;
#[cfg(test)]
mod tests;
mod views;

fn threads(value: i32) -> Option<u64> {
    if value < 0 {
        return None;
    }

    Some(u64::try_from(value).unwrap_or_else(|_| unreachable!("nonnegative i32 fits u64")))
}
