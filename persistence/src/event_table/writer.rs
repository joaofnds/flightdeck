use collector::recording::private::PrivateDirectory;
use std::path::Path;
use std::sync::Arc;

use collector::ring::{FoundEvent, IdentityObservation, RingReader};

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
        pending: &Path,
        identity: WindowIdentity,
        first: u64,
        identities: bool,
        memory_limit: u64,
    ) -> Result<Self> {
        let budget = Arc::new(crate::resources::ByteBudget::new(
            usize::try_from(memory_limit).map_err(|_| Error::Bound("event stream memory"))?,
        ));
        let reservation = budget
            .reserve(batch::WORKING_BYTES)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        let directory = PrivateDirectory::open(pending)?.create_directory("events")?;
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

    pub(crate) fn publication(
        &mut self,
        reader: &mut RingReader,
        tick: &StagedTick,
        programs: Option<&Programs>,
        losses: &mut Losses,
    ) -> Result<()> {
        self.prefix(reader, tick.events_first, losses)?;
        for (index, event) in tick.events.iter().enumerate() {
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
        self.next_event = tick.events_end;
        self.index.end = tick.events_end;
        Ok(())
    }

    fn prefix(&mut self, reader: &mut RingReader, end: u64, losses: &mut Losses) -> Result<()> {
        reader.seek_event(self.next_event)?;
        for _step in 0..=reader.shape().event_capacity {
            if reader.next_event() >= end {
                return Ok(());
            }
            self.room(reader)?;
            let mut identity = IdentityObservation::EMPTY;
            let found = if self.index.identities {
                reader.read_event_identity(&mut identity)?
            } else {
                reader.read_event()?
            };
            match found {
                FoundEvent::Event {
                    number,
                    tick,
                    event,
                } => {
                    let row = batch::copy_row(
                        reader,
                        &NumberedEvent {
                            number,
                            tick,
                            event: &event,
                        },
                        self.index.identities.then_some(&identity),
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
        crate::encoder::encode_write(&mut frame, file)?;
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
    *reader = replacement;
    Ok(())
}
