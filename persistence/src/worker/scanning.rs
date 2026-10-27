use super::WindowIdentity;
use super::{Found, Limits, RegisteredRun, Result, RingReader, RunState, Staged, Store, Tick};

pub(super) struct Scanning {
    reader: RingReader,
    tick: Tick,
    limits: Limits,
    closed: bool,
    first: u64,
    remaining: u64,
}

impl Scanning {
    pub(super) fn new(
        mut reader: RingReader,
        registered: &RegisteredRun,
        closed: bool,
    ) -> Result<Self> {
        let limits = Limits::from_shape(&reader.shape())?;
        let mut tick = crate::tick_buffer(&reader.shape())?;
        let closed =
            super::persist_one_closed(&mut reader, &mut tick, registered.collector_pid, closed)?;
        let first = reader.next_publication();
        let remaining = reader.shape().slot_count + 2;

        Ok(Self {
            reader,
            tick,
            limits,
            closed,
            first,
            remaining,
        })
    }

    pub(super) fn steps(&self) -> u64 {
        self.remaining + 1
    }

    pub(super) fn read(
        &mut self,
        store: &Store,
        registered: &RegisteredRun,
        state: &mut RunState,
    ) -> Result<Option<Staged>> {
        if self.remaining == 0 {
            return if state.next_publication > self.first {
                Ok(Some(Staged::Waiting))
            } else {
                Err(crate::Error::Bound("ring recovery scan"))
            };
        }

        self.remaining -= 1;
        match self.reader.read_into(&mut self.tick)? {
            Found::Nothing => {
                super::persist_one_stage_end(&self.reader, registered, self.closed, state).map(Some)
            }
            Found::Missed { first, end } => {
                state.losses.publications += end - first;
                state.next_publication = self.reader.next_publication();

                Ok(None)
            }
            Found::Publication { publication, .. } => {
                let identity = WindowIdentity {
                    run: registered.identity,
                    window: super::window_number(&self.reader, &self.tick)?,
                };
                if let Some(manifest) = store.existing(&identity)? {
                    self.reader.seek_publication(manifest.publication_end)?;
                    state.next_publication = manifest.publication_end;
                    return Ok(None);
                }

                if !super::window_ready(&mut self.reader, &mut self.tick, &identity, self.closed)? {
                    state.next_publication = publication;
                    return Ok(Some(Staged::Waiting));
                }

                self.reader.seek_publication(publication)?;
                super::persist_one_stage_selected(
                    &mut self.reader,
                    &mut self.tick,
                    identity,
                    self.limits,
                    state,
                )
                .map(|window| window.map(Staged::Window))
            }
        }
    }
}
