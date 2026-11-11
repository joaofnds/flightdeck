use super::{Found, IdentityObservation, Limits, Losses, Observation, Result, RingReader};
use super::{ReadEvents, Tick, Window, WindowIdentity};

pub(super) struct Staging {
    window: Window,
    events: crate::event_table::writer::Writer,
    identities: Vec<IdentityObservation>,
    captured: bool,
    publications_pending: u64,
}

impl Staging {
    pub(super) fn new(
        reader: &RingReader,
        identity: WindowIdentity,
        limits: Limits,
        events: crate::event_table::writer::Writer,
    ) -> Self {
        let captured = reader.dictionary_capacity().is_some();
        Self {
            window: Window::new(identity, limits),
            events,
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
                // The tick is now in the window, and its pages of the ring would stay charged
                // to this process beside that copy until the window is sealed.
                reader.release_pages()?;
                losses.publications += self.publications_pending;
                self.publications_pending = 0;

                Ok(true)
            }
        }
    }

    /// Whether the ring overwrote where the window starts before any of it was read.
    pub(super) fn lost_its_start(&self) -> bool {
        self.window.tick_count() == 0 && self.publications_pending > 0
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
        let Some(copied) = self.window.ticks.last() else {
            unreachable!("successful publication copy adds one tick");
        };
        self.events
            .publication(reader, copied, self.window.programs.as_deref(), losses)?;

        Ok(())
    }

    pub(super) fn finish(mut self, reader: &mut RingReader, losses: &mut Losses) -> Result<Window> {
        if self.window.tick_count() == 0 {
            losses.publications += self.publications_pending;
        }

        if self.window.tick_count() > 0 {
            let first = self.window.ticks[0].events_first;
            self.window.event_index = Some(self.events.finish(reader, first)?);
        }
        Ok(self.window)
    }
}
