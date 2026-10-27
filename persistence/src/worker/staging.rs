use super::{Found, IdentityObservation, Limits, Losses, Observation, Result, RingReader};
use super::{ReadEvents, Tick, Window, WindowIdentity};

pub(super) struct Staging {
    window: Window,
    identities: Vec<IdentityObservation>,
    captured: bool,
    publications_pending: u64,
}

impl Staging {
    pub(super) fn new(reader: &RingReader, identity: WindowIdentity, limits: Limits) -> Self {
        let captured = reader.dictionary_capacity().is_some();

        Self {
            window: Window::new(identity, limits),
            identities: Vec::with_capacity(if captured { limits.processes } else { 0 }),
            captured,
            publications_pending: 0,
        }
    }

    pub(super) fn read(
        &mut self,
        reader: &mut RingReader,
        tick: &mut Tick,
        losses: &mut Losses,
    ) -> Result<bool> {
        let found = if self.captured {
            reader.read_identity_into(tick, &mut self.identities)?
        } else {
            reader.read_into(tick)?
        };

        match found {
            Found::Nothing => Ok(false),
            Found::Missed { first, end } => {
                self.publications_pending += end - first;
                Ok(true)
            }
            Found::Publication { publication, .. } => {
                if super::window_number(reader, tick)? != self.window.identity.window {
                    reader.seek_publication(publication)?;
                    return Ok(false);
                }

                self.read_publication(reader, tick, publication, losses)?;
                losses.publications += self.publications_pending;
                self.publications_pending = 0;

                Ok(true)
            }
        }
    }

    fn read_publication(
        &mut self,
        reader: &mut RingReader,
        tick: &Tick,
        publication: u64,
        losses: &mut Losses,
    ) -> Result<()> {
        let mut events = ReadEvents {
            records: Vec::new(),
            identities: Vec::new(),
        };
        let mut copied_losses = Losses::default();
        super::read_events(reader, tick, &mut copied_losses, &mut events)?;
        let numbered = super::stage_window_numbered(&events);
        let observation = Observation {
            publication,
            wall_ns: tick.times.map(|times| times.wall_ns),
            continuous_ns: tick.times.map(|times| times.continuous_ns),
            awake_ns: tick.times.map(|times| times.awake_ns),
        };

        if self.captured {
            self.window.push_tick_with_identity(
                &observation,
                tick,
                &numbered,
                &crate::IdentitySource {
                    reader,
                    processes: &self.identities,
                    events: &events.identities,
                },
            )?;
        } else {
            self.window.push_tick(&observation, tick, &numbered)?;
        }

        losses.events += copied_losses.events;

        Ok(())
    }

    pub(super) fn finish(self, losses: &mut Losses) -> Window {
        if self.window.tick_count() == 0 {
            losses.publications += self.publications_pending;
        }

        self.window
    }
}
