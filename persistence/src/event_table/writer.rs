use collector::recording::private::PrivateDirectory;
use std::sync::Arc;

use collector::ring::{
    FoundEvent, IdentityObservation, ProgramBuffers, ProgramLengths, RingReader, RunIdentity,
};

use super::index::Index;
use crate::event_observation::{BATCH_BYTES_MAX, BATCH_ROWS_MAX};
use crate::event_recovery::Batch;
use crate::event_recovery::batch::{self, FramedBatch, PROGRAM_BYTES_MAX, ROWS_BYTES_MAX, Row};
use crate::owned_identity::Programs;
use crate::window::StagedTick;
use crate::{Error, Losses, NumberedEvent, Result, WindowIdentity};

#[cfg(test)]
mod tests;

pub(crate) struct Writer {
    directory: PrivateDirectory,
    index: Index,
    open_batch: OpenBatch,
    next_event: u64,
    _reservation: crate::resources::ByteReservation,
}

/// The batch being filled, and what all the rows so far say that the index will need.
struct OpenBatch {
    rows: Vec<Row>,
    bytes: usize,
    first: u64,
    rows_total: u64,
    previous: Option<u64>,
}

impl OpenBatch {
    fn full(&self) -> bool {
        self.rows.len() == BATCH_ROWS_MAX || self.bytes + PROGRAM_BYTES_MAX > BATCH_BYTES_MAX
    }

    fn push(&mut self, row: Row) -> Result<()> {
        if self
            .previous
            .is_some_and(|previous| previous >= row.event.number)
        {
            return Err(Error::Bound("event stream order"));
        }
        if self.rows_total == 0 {
            self.first = row.event.number;
        }
        self.bytes += row.name.as_ref().map_or(0, Vec::len) + row.app.as_ref().map_or(0, Vec::len);
        assert!(self.bytes <= BATCH_BYTES_MAX);
        self.rows_total += 1;
        if self.rows_total > collector::u64_from_usize(crate::event_observation::ROWS_MAX) {
            return Err(Error::Bound("event stream rows"));
        }
        self.previous = Some(row.event.number);
        self.rows.push(row);
        Ok(())
    }

    fn framed(&mut self, identities: bool) -> Result<FramedBatch> {
        let framed = batch::framed(&self.rows, identities)?;
        self.rows.clear();
        self.bytes = ROWS_BYTES_MAX;
        Ok(framed)
    }
}

/// What a thread needs to read the same source as `RingReader` without sharing its mapping.
struct PrefixSource {
    name: String,
    identity: Option<RunIdentity>,
    layout_version: u64,
}

impl PrefixSource {
    fn of(reader: &RingReader) -> Self {
        Self {
            name: reader.name().to_owned(),
            identity: reader.identity(),
            layout_version: reader.source_layout_version(),
        }
    }

    fn open(&self) -> Result<RingReader> {
        let reader = RingReader::open_generation(&self.name)?;
        if reader.identity() != self.identity
            || reader.source_layout_version() != self.layout_version
        {
            return Err(Error::Bound("event stream source changed"));
        }
        Ok(reader)
    }
}

impl Writer {
    /// A writer whose working memory comes out of a budget it shares with the rest of the seal.
    pub(crate) fn charged(
        directory: PrivateDirectory,
        identity: WindowIdentity,
        first: u64,
        identities: bool,
        budget: &Arc<crate::resources::ByteBudget>,
    ) -> Result<Self> {
        let reservation = budget
            .reserve(batch::WORKING_BYTES + batch::READ_AHEAD_BYTES)
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
            open_batch: OpenBatch {
                rows: Vec::with_capacity(BATCH_ROWS_MAX),
                bytes: ROWS_BYTES_MAX,
                first,
                rows_total: 0,
                previous: None,
            },
            next_event: first,
            _reservation: reservation,
        })
    }

    /// Continues after the batches an interrupted attempt saved, so their rows stay as they
    /// are and the same events read again are not written twice.
    pub(crate) fn resume(mut self, saved: &[Batch]) -> Self {
        assert_eq!(self.open_batch.rows_total, 0);
        if let (Some(first), Some(last)) = (saved.first(), saved.last()) {
            self.open_batch.first = first.first;
            self.open_batch.rows_total = saved.iter().map(|batch| batch.rows).sum();
            self.open_batch.previous = Some(last.end - 1);
            self.index.end = last.end;
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
            self.open_batch.push(Row {
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

    /// Saves the events before a publication. Reading them and checking their batches takes as long
    /// as encoding and writing those batches, and the most rows a recording retains have a 30 s
    /// recovery bound, so a thread reads ahead of this one, which alone changes files.
    fn prefix(&mut self, reader: &RingReader, end: u64, losses: &mut Losses) -> Result<()> {
        if self.next_event >= end {
            return Ok(());
        }

        let source = PrefixSource::of(reader);
        let start = self.next_event;
        let identities = self.index.identities;
        let open_batch = &mut self.open_batch;
        batch::read_ahead(
            |batches| prefix_read(&source, start..end, identities, open_batch, losses, batches),
            |framed| write_batch(&self.directory, &mut self.index, framed),
        )
    }

    fn room(&mut self, reader: &mut RingReader) -> Result<()> {
        if self.open_batch.full() {
            reader.release_pages()?;
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.open_batch.rows.is_empty() {
            return Ok(());
        }
        let framed = self.open_batch.framed(self.index.identities)?;
        write_batch(&self.directory, &mut self.index, framed)
    }

    pub(crate) fn finish(mut self, reader: &mut RingReader, empty_first: u64) -> Result<Index> {
        reader.release_pages()?;
        self.flush()?;
        self.index.first = if self.open_batch.rows_total == 0 {
            empty_first
        } else {
            self.open_batch.first
        };
        self.index.rows = self.open_batch.rows_total;
        self.index
            .write_file(&mut self.directory.create_file("index")?)?;
        Ok(self.index)
    }
}

fn write_batch(
    directory: &PrivateDirectory,
    index: &mut Index,
    mut framed: FramedBatch,
) -> Result<()> {
    if index.batches.len() == batch::BATCHES_MAX {
        return Err(Error::Bound("event stream batches"));
    }
    let file = directory.create_file(&crate::event_recovery::batch_name(index.batches.len()))?;
    crate::encoder::encode_write_event_batch(&mut framed.frame, file)?;
    index.batches.push(framed.batch);
    Ok(())
}

/// `prefix`'s reading, on its own thread with its own mapping, which it replaces after each batch
/// so the pages it touched stop counting against the process.
fn prefix_read(
    source: &PrefixSource,
    events: std::ops::Range<u64>,
    identities: bool,
    open_batch: &mut OpenBatch,
    losses: &mut Losses,
    batches: &std::sync::mpsc::SyncSender<FramedBatch>,
) -> Result<()> {
    let mut reader = source.open()?;
    reader.seek_event(events.start)?;
    let mut name = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let mut app = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let mut buffers = ProgramBuffers {
        executable_name: &mut name,
        app_path: &mut app,
    };
    for _step in 0..=reader.shape().event_capacity {
        if reader.next_event() >= events.end {
            return Ok(());
        }
        if open_batch.full() {
            let event = reader.next_event();
            reader = source.open()?;
            reader.seek_event(event)?;
            if batches.send(open_batch.framed(identities)?).is_err() {
                return Ok(());
            }
        }
        let mut identity = IdentityObservation::EMPTY;
        let (found, lengths) = if identities {
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
                    identities.then_some(&identity),
                    &buffers,
                    lengths,
                )?;
                open_batch.push(row)?;
            }
            FoundEvent::Missed { first, end: missed } => {
                losses.events += missed.min(events.end).saturating_sub(first);
            }
            FoundEvent::Nothing => return Err(Error::Bound("event stream prefix")),
        }
    }
    Err(Error::Bound("event stream scan"))
}
