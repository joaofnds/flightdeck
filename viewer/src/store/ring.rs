//! The store's ring adapter. It follows the newest run the recording registers and derives a
//! second from each tick the run's ring publishes.

use std::io::ErrorKind;
use std::mem;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use collector::recording::{Registry, RegistryBuffer, read_identity};
use collector::ring::{Found, RingReader, RunIdentity, Shape, Tick};

use super::{Failure, MachineSecond, readings};
use crate::recording::MachineMinute;

/// The ticks one read takes in, well above the ticks a ring publishes between two steps.
const TICKS_PER_READ_MAX: usize = 256;

/// The largest ring the store maps.
const MAPPED_BYTES_MAX: usize = 1 << 30;

pub(super) struct Ring {
    root: PathBuf,
    runs: PathBuf,
    registry: RegistryBuffer,
    source: Option<Source>,
    /// A second read from the ring that the store could not take yet.
    pending: Option<MachineSecond>,
}

/// The ring of one run, and the room to take its ticks in.
struct Source {
    reader: RingReader,
    identity: RunIdentity,
    tick: Tick,
    /// The tick read last and the one before it, each as a machine minute of one tick, so a
    /// second from the ring is derived as one from a sealed minute is.
    current: MachineMinute,
    previous: MachineMinute,
}

impl Ring {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            runs: root.join("runs"),
            root,
            registry: [0; _],
            source: None,
            pending: None,
        }
    }

    /// Hands `push` the second of each tick the ring published since the last read, oldest
    /// first, until the ring has no more or `push` refuses one, which the next read hands it
    /// again.
    pub(super) fn read(
        &mut self,
        mut push: impl FnMut(&MachineSecond) -> bool,
    ) -> Result<(), Failure> {
        let Some(source) = &mut self.source else {
            return Ok(());
        };

        for _ in 0..TICKS_PER_READ_MAX {
            if self.pending.is_none() {
                match source.reader.read_into(&mut source.tick) {
                    Ok(Found::Publication { publication, .. }) => {
                        self.pending = source.second(publication);
                    }
                    Ok(Found::Missed { .. }) => continue,
                    Ok(Found::Nothing) => return Ok(()),
                    Err(error) => return Err(Failure::Ring(error.kind())),
                }
            }

            let Some(second) = &self.pending else {
                continue;
            };
            if !push(second) {
                return Ok(());
            }
            self.pending = None;
        }

        Ok(())
    }

    /// Follows the newest run the recording registers, from the oldest tick its ring holds,
    /// once that ring can be read. Until then the store follows the run it followed before.
    pub(super) fn follow(&mut self) -> Result<(), Failure> {
        let recording =
            read_identity(&self.root).map_err(|error| Failure::Recording(error.kind()))?;
        let registry = Registry::read_into(&self.runs, &mut self.registry)
            .map_err(|error| Failure::Registry(error.kind()))?;
        let Some(newest) = registry.runs().last() else {
            return Ok(());
        };
        let newest = newest.map_err(|error| Failure::Registry(error.kind()))?;
        if newest.identity.recording != recording {
            return Err(Failure::ForeignRecording);
        }

        if let Some(source) = &self.source {
            if source.identity == newest.identity {
                return Ok(());
            }
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

        self.source = Some(Source::open(reader, identity)?);
        Ok(())
    }

    /// Sleeps until the ring publishes past what the store read, for at most `timeout`. Where
    /// the store follows no ring yet, or holds a second it could not take, the ring would not
    /// wake it, so it sleeps all of `timeout`.
    pub(super) fn wait(&self, timeout: Duration) -> Result<(), Failure> {
        let Some(source) = &self.source else {
            thread::sleep(timeout);
            return Ok(());
        };
        if self.pending.is_some() {
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
    /// The run's ring with room for its ticks, allocated once, as the run is followed.
    fn open(reader: RingReader, identity: RunIdentity) -> Result<Self, Failure> {
        let shape = reader.shape();
        let tick = persistence::tick_buffer(&shape).map_err(|_| Failure::TickShape)?;

        Ok(Self {
            reader,
            identity,
            tick,
            current: minute_of(&shape)?,
            previous: minute_of(&shape)?,
        })
    }

    /// The second of the tick just read, the ring's publication `publication`, with its rates
    /// over the interval since the tick read before it.
    fn second(&mut self, publication: u64) -> Option<MachineSecond> {
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
