//! The store's ring adapter. It follows the newest run the recording registers and derives a
//! second and its Apps from each tick the run's ring publishes.

use std::io::ErrorKind;
use std::mem;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use collector::recording::{Registry, RegistryBuffer, read_identity};
use collector::ring::{
    Found, FoundEvent, IdentityObservation, RingReader, RunIdentity, Shape, Tick,
};
use collector::tracker::{EVENTS_PER_TICK_MAX, Event};
use persistence::{Error, NumberedEvent};

use super::apps::TickApps;
use super::{Failure, MachineSecond, readings};
use crate::live_app_response::Sample;
use crate::live_apps::committed::Endpoint;
use crate::live_apps::{Frame, Publication, PublicationEvents};
use crate::recording::MachineMinute;

/// The ticks one read takes in, well above the ticks a ring publishes between two steps.
const TICKS_PER_READ_MAX: usize = 256;

/// The steps between two looks at the runs registry for a newer run than the one the store
/// follows, so a step between two of the ring's ticks reads no file.
pub(super) const FOLLOW_STEPS: u32 = 10;

/// The largest ring the store maps.
const MAPPED_BYTES_MAX: usize = 1 << 30;

pub(super) struct Ring {
    root: PathBuf,
    runs: PathBuf,
    registry: RegistryBuffer,
    source: Option<Source>,
    /// A second read from the ring that the store could not take yet, and whether the App rule
    /// derived its Apps, which the source's sample holds, or why it refused them.
    pending: Option<(MachineSecond, Result<(), String>)>,
    /// The steps since the store last looked at the runs registry, and what it found there.
    unlooked_steps: u32,
    followed: Result<(), Failure>,
    /// Whether the ring the store follows published a tick since the store last looked.
    published: bool,
}

/// The ring of one run, and the room to take its ticks in.
struct Source {
    reader: RingReader,
    identity: RunIdentity,
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
}

impl Ring {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            runs: root.join("runs"),
            root,
            registry: [0; _],
            source: None,
            pending: None,
            unlooked_steps: FOLLOW_STEPS,
            followed: Ok(()),
            published: false,
        }
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
                        }
                    }
                    Ok(Found::Missed { .. }) => continue,
                    Ok(Found::Nothing) => return Ok(()),
                    Err(error) => return Err(Failure::Ring(error.kind())),
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

    /// Looks at the registry and follows its newest run, as `follow` describes. The store checked
    /// the recording's identity when it began following its run, so while that run is the newest
    /// it reads the registry alone.
    fn follow_newest(&mut self) -> Result<(), Failure> {
        if self.follow_newest_followed()? {
            return Ok(());
        }

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
        Ok(newest.identity == source.identity)
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

        let processes = count(shape.process_capacity)?;

        Ok(Self {
            reader,
            identity,
            tick,
            identities: Vec::with_capacity(processes),
            events: Vec::with_capacity(EVENTS_PER_TICK_MAX),
            event_identities: Vec::with_capacity(EVENTS_PER_TICK_MAX),
            current: minute_of(&shape)?,
            previous: minute_of(&shape)?,
            frame: None,
            previous_frame: None,
            previous_endpoint: None,
            sample: None,
        })
    }

    /// Reads the ring's next tick, with its processes' identities where the ring holds them.
    fn read_tick(&mut self) -> std::io::Result<Found> {
        if self.reader.dictionary_capacity().is_some() {
            self.reader
                .read_identity_into(&mut self.tick, &mut self.identities)
        } else {
            self.identities.clear();
            self.reader.read_into(&mut self.tick)
        }
    }

    /// Runs the App rule on the tick just read, the ring's publication `publication`, with its
    /// rates since the tick before it the rule took, and leaves its sample in `sample`, or says
    /// why the rule refused the tick.
    fn apps(&mut self, publication: u64) -> Result<(), String> {
        if let Err(error) = self.apps_capture(publication) {
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
            self.previous_endpoint
                .as_ref()
                .is_some_and(|previous| frame.takes_rates_from(previous))
        });
        Sample::capture(frame, previous, &mut self.sample)
    }

    /// Reads the events of the tick just read into `events`, with their identities where the
    /// ring holds them. An event the ring no longer holds is left out.
    fn apps_capture_events(&mut self) -> persistence::Result<()> {
        self.events.clear();
        self.event_identities.clear();
        if self.tick.events.is_empty() {
            return Ok(());
        }

        self.reader.seek_event(self.tick.events.start)?;
        let identities = self.reader.dictionary_capacity().is_some();
        for _ in 0..=EVENTS_PER_TICK_MAX {
            if self.reader.next_event() >= self.tick.events.end {
                return Ok(());
            }

            let mut identity = IdentityObservation::EMPTY;
            let found = if identities {
                self.reader.read_event_identity(&mut identity)?
            } else {
                self.reader.read_event()?
            };
            match found {
                FoundEvent::Nothing => return Ok(()),
                FoundEvent::Missed { .. } => {}
                FoundEvent::Event {
                    number,
                    tick,
                    event,
                } => {
                    if self.events.len() == EVENTS_PER_TICK_MAX {
                        return Err(Error::Bound("store tick events"));
                    }
                    self.events.push((number, tick, event));
                    if identities {
                        self.event_identities.push(identity);
                    }
                }
            }
        }

        Err(Error::Bound("store tick events"))
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
