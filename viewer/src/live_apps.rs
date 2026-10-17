//! Owned live publication metadata for the shared pure App aggregate.
use collector::processes::Usage;
use collector::ring::{
    IdentityObservation, IdentityUnavailable, ProcessCreation, ProgramBytes, RingReader,
    RunIdentity, Tick,
};
use persistence::{Error, Result};

use crate::apps::{self, Stamp, Unavailable};

/// Each selected/predecessor pair stays inside the aggregate's 64 MiB raw-byte budget.
pub const PROGRAM_FRAME_BYTES_MAX: usize = apps::PROGRAM_BYTES_MAX / 2;
/// Join-view payload bound; the shared aggregate separately bounds its result/work storage.
pub const TRANSIENT_VIEW_BYTES_MAX: usize = apps::PROCESS_INTERVALS_MAX
    * (2 * size_of::<Option<ProgramBytes<'static>>>()
        + 2 * size_of::<apps::ProcessObservation<'static>>()
        + size_of::<apps::ProcessInterval<'static>>());

pub struct Frame {
    identity: RunIdentity,
    stamp: Stamp,
    run_tick: u64,
    wall_ns: Option<u64>,
    members: Vec<Member>,
    arena: Vec<u8>,
    pub usage_refused: u32,
    pub usage_gone: u32,
}

struct Member {
    pid: i32,
    creation: Option<ProcessCreation>,
    program: std::result::Result<ProgramSpan, IdentityUnavailable>,
    usage: std::result::Result<Usage, Unavailable>,
}

#[derive(Clone, Copy)]
struct ProgramSpan {
    start: usize,
    name_end: usize,
    end: usize,
}

/// All references must come from the same successful read of this exact reader handle.
/// Capture before reusing its tick/identity buffers or advancing its publication cursor.
pub struct Publication<'source> {
    pub reader: &'source RingReader,
    pub tick: &'source Tick,
    pub identities: &'source [IdentityObservation],
    pub publication: u64,
}

const _: () = {
    assert!(PROGRAM_FRAME_BYTES_MAX * 2 == apps::PROGRAM_BYTES_MAX);
    assert!(apps::PROCESS_INTERVALS_MAX < 65_536);
};

impl Frame {
    /// Owns copied source bytes and numeric readings. A refusal leaves output empty.
    pub fn capture(source: &Publication<'_>, output: &mut Option<Box<Self>>) -> Result<()> {
        *output = None;
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
            arena: Vec::with_capacity(capture::program_bytes(source)?),
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

        Ok(())
    }

    /// Charges owned vector capacities and the frame itself to the live observation cache.
    #[must_use]
    pub fn bytes(&self) -> usize {
        size_of::<Self>() + self.members.capacity() * size_of::<Member>() + self.arena.capacity()
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
    /// Events cannot currently prove creation/final-exit association, so membership is uncertain.
    pub fn with_aggregate(
        &self,
        previous: Option<&Self>,
        consume: impl FnOnce(&apps::Aggregate<'_>) -> Result<()>,
    ) -> Result<()> {
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
#[cfg(test)]
mod tests;
mod views;
