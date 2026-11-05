use collector::recording::private::PrivateDirectory;
use std::sync::Arc;

use collector::ring::{
    FoundEvent, IdentityObservation, ProgramBuffers, ProgramLengths, RingReader,
};

use super::index::Index;
use crate::event_observation::{BATCH_BYTES_MAX, BATCH_ROWS_MAX};
use crate::event_recovery::Batch;
use crate::event_recovery::batch::{self, PROGRAM_BYTES_MAX, ROWS_BYTES_MAX, Row};
use crate::owned_identity::Programs;
use crate::window::StagedTick;
use crate::{Error, Losses, NumberedEvent, Result, WindowIdentity};

pub(crate) struct Writer {
    directory: PrivateDirectory,
    index: Index,
    rows: Vec<Row>,
    bytes: usize,
    next_event: u64,
    _reservation: crate::resources::ByteReservation,
}

impl Writer {
    pub(crate) fn new(
        directory: PrivateDirectory,
        identity: WindowIdentity,
        first: u64,
        identities: bool,
        memory_limit: u64,
    ) -> Result<Self> {
        let budget = Arc::new(crate::resources::ByteBudget::new(
            usize::try_from(memory_limit).map_err(|_| Error::Bound("event stream memory"))?,
        ));

        Self::charged(directory, identity, first, identities, &budget)
    }

    /// A writer whose working memory comes out of a budget it shares with the rest of the seal.
    pub(crate) fn charged(
        directory: PrivateDirectory,
        identity: WindowIdentity,
        first: u64,
        identities: bool,
        budget: &Arc<crate::resources::ByteBudget>,
    ) -> Result<Self> {
        let reservation = budget
            .reserve(batch::WORKING_BYTES)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            directory,
            index: Index {
                identity,
                first,
                end: first,
                rows: 0,
                identities,
                batches: Vec::with_capacity(batch::BATCHES_MAX),
            },
            rows: Vec::with_capacity(BATCH_ROWS_MAX),
            bytes: ROWS_BYTES_MAX,
            next_event: first,
            _reservation: reservation,
        })
    }

    /// Continues after the batches an interrupted attempt saved, so their rows stay as they
    /// are and the same events read again are not written twice.
    pub(crate) fn resume(mut self, saved: &[Batch]) -> Self {
        assert_eq!(self.index.rows, 0);
        if let (Some(first), Some(last)) = (saved.first(), saved.last()) {
            self.index.first = first.first;
            self.index.end = last.end;
            self.index.rows = saved.iter().map(|batch| batch.rows).sum();
            self.index.batches.extend_from_slice(saved);
            self.next_event = last.end;
        }
        self
    }

    pub(crate) fn publication(
        &mut self,
        reader: &mut RingReader,
        tick: &StagedTick,
        programs: Option<&Programs>,
        losses: &mut Losses,
    ) -> Result<()> {
        self.prefix(reader, tick.events_first, losses)?;
        let saved_end = self.next_event;
        for (index, event) in tick.events.iter().enumerate() {
            if event.number < saved_end {
                continue;
            }
            self.room(reader)?;
            let identity = tick.identities.as_ref().map(|rows| rows.events[index]);
            let (name, app) = if let Some(program) = identity.and_then(|value| value.program) {
                let programs = programs.ok_or(Error::Bound("event stream programs"))?;
                (
                    Some(programs.name(program).to_vec()),
                    programs.app(program).map(<[u8]>::to_vec),
                )
            } else {
                (None, None)
            };
            self.push(Row {
                publication: Some(tick.observation.publication),
                event: event.clone(),
                identity,
                name,
                app,
            })?;
        }
        self.next_event = self.next_event.max(tick.events_end);
        self.index.end = self.next_event;
        Ok(())
    }

    fn prefix(&mut self, reader: &mut RingReader, end: u64, losses: &mut Losses) -> Result<()> {
        reader.seek_event(self.next_event)?;
        let mut name = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
        let mut app = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
        let mut buffers = ProgramBuffers {
            executable_name: &mut name,
            app_path: &mut app,
        };
        for _step in 0..=reader.shape().event_capacity {
            if reader.next_event() >= end {
                return Ok(());
            }
            self.room(reader)?;
            let mut identity = IdentityObservation::EMPTY;
            let (found, lengths) = if self.index.identities {
                reader.read_event_identity_bytes(&mut identity, &mut buffers)?
            } else {
                (reader.read_event()?, ProgramLengths::default())
            };
            match found {
                FoundEvent::Event {
                    number,
                    tick,
                    event,
                } => {
                    let row = batch::copy_row_read(
                        &NumberedEvent {
                            number,
                            tick,
                            event: &event,
                        },
                        self.index.identities.then_some(&identity),
                        &buffers,
                        lengths,
                    )?;
                    self.push(row)?;
                }
                FoundEvent::Missed { first, end: missed } => {
                    losses.events += missed.min(end).saturating_sub(first);
                }
                FoundEvent::Nothing => return Err(Error::Bound("event stream prefix")),
            }
        }
        Err(Error::Bound("event stream scan"))
    }

    fn room(&mut self, reader: &mut RingReader) -> Result<()> {
        if self.rows.len() == BATCH_ROWS_MAX || self.bytes + PROGRAM_BYTES_MAX > BATCH_BYTES_MAX {
            remap(reader)?;
            self.flush()?;
        }
        Ok(())
    }

    fn push(&mut self, row: Row) -> Result<()> {
        let previous = self
            .rows
            .last()
            .map(|row| row.event.number)
            .or_else(|| self.index.batches.last().map(|batch| batch.end - 1));
        if previous.is_some_and(|previous| previous >= row.event.number) {
            return Err(Error::Bound("event stream order"));
        }
        if self.index.rows == 0 {
            self.index.first = row.event.number;
        }
        self.bytes += row.name.as_ref().map_or(0, Vec::len) + row.app.as_ref().map_or(0, Vec::len);
        assert!(self.bytes <= BATCH_BYTES_MAX);
        self.index.rows += 1;
        if self.index.rows > collector::u64_from_usize(crate::event_observation::ROWS_MAX) {
            return Err(Error::Bound("event stream rows"));
        }
        self.rows.push(row);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        if self.index.batches.len() == batch::BATCHES_MAX {
            return Err(Error::Bound("event stream batches"));
        }
        let mut frame = batch::frame(&self.rows, self.index.identities)?;
        batch::validate_frame(&frame, self.index.identities)?;
        let file = self
            .directory
            .create_file(&crate::event_recovery::batch_name(self.index.batches.len()))?;
        crate::encoder::encode_write_event_batch(&mut frame, file)?;
        self.index.batches.push(Batch {
            first: self.rows[0].event.number,
            end: self.rows[self.rows.len() - 1].event.number + 1,
            rows: collector::u64_from_usize(self.rows.len()),
        });
        self.rows.clear();
        self.bytes = ROWS_BYTES_MAX;
        Ok(())
    }

    pub(crate) fn finish(mut self, reader: &mut RingReader, empty_first: u64) -> Result<Index> {
        remap(reader)?;
        self.flush()?;
        if self.index.rows == 0 {
            self.index.first = empty_first;
        }
        self.index
            .write_file(&mut self.directory.create_file("index")?)?;
        Ok(self.index)
    }
}

// All native references used by staging were copied into owned rows before this remap.
fn remap(reader: &mut RingReader) -> Result<()> {
    let publication = reader.next_publication();
    let event = reader.next_event();
    let mut replacement = RingReader::open_generation(reader.name())?;
    if replacement.identity() != reader.identity()
        || replacement.source_layout_version() != reader.source_layout_version()
    {
        return Err(Error::Bound("event stream source changed"));
    }
    replacement.seek_publication(publication)?;
    replacement.seek_event(event)?;
    if reader.ended() {
        replacement.set_writer_gone();
    }
    *reader = replacement;
    Ok(())
}
