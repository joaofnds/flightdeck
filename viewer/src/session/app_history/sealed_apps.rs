//! What the persister derives of the Apps of each observation of a window as it seals, so the
//! viewer shows a saved second's Apps without replaying its minute.
use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use super::{Frame, Reading};
use crate::cancellation::{Cancellation, Operation};
use crate::live_app_response::Sample;
use collector::ring::RunIdentity;
use persistence::engine::Engine;
use persistence::query::{TableSource, table_source};
use persistence::schema::{APP_PARTS, Table};
use persistence::worker::{Derivation, Derive};
use persistence::{Error, Result, TableManifest, Window};

/// What the samples a derivation holds for its window's App parts may take.
const SAMPLES_BYTES_MAX: u64 = 256 * 1024 * 1024;
/// What the frames of the window's processes and events may take together, which the
/// derivation queries in memory once it has read every file it needs.
const STAGED_FRAMES_BYTES_MAX: u64 = persistence::engine::PARQUET_EXPANDED_BYTES_MAX;

/// The window staged for its seal, the engine's read of one of its tables or the window's
/// frames it queries after its reads, the samples held for its parts, the table built from them
/// and the buffer it is written through fit the persister's working bound together.
const _: () = {
    assert!(STAGED_FRAMES_BYTES_MAX <= persistence::engine::PARQUET_EXPANDED_BYTES_MAX);
    assert!(
        persistence::STAGED_BYTES_MAX
            + persistence::engine::PARQUET_EXPANDED_BYTES_MAX
            + super::sealed_parts::DECODED_BYTES_MAX
            + SAMPLES_BYTES_MAX
            + persistence::WRITE_BUFFER_BYTES
            <= persistence::resources::WORKING_BYTES_MAX
    );
};

/// Samples each observation of a window as it seals, from the window the persister staged and
/// wrote, by the rule the replay of its minute samples it, and keeps the Apps' ranks and every
/// App's part.
#[derive(Default)]
pub struct SealedApps {
    /// The last observation the derivation sampled, which the next window's first sample takes
    /// its rates from without replaying the window that holds it.
    last: RefCell<Option<Sampled>>,
}

/// An observation a derivation sampled, its frame, and the run it belongs to.
struct Sampled {
    run: RunIdentity,
    reading: Reading,
    frame: Box<Frame>,
}

impl Derive for SealedApps {
    fn derive(&self, derivation: &Derivation<'_>) -> Result<Vec<TableManifest>> {
        let identity = derivation.window.identity;
        let replayer = Replayer {
            connection: Engine::new(),
            operation: Arc::new(Cancellation::default()).operation(0)?,
            run: identity.run,
        };
        let readings = replayer.readings(derivation.pending)?;
        let first = readings.first().ok_or(Error::Bound("sealed Apps ticks"))?;
        let last = self.last.take();
        let mut projection = Projection {
            previous: None,
            reading: None,
        };
        if let Some(directory) = derivation.predecessor {
            if let Some(publication) = first.observation.stamp.publication.checked_sub(1) {
                projection = replayer.predecessor(directory, publication, last)?;
            }
        }

        // Every file the derivation reads is read by now, so the window's frames take the place
        // of a file's read in the working bound.
        let sources = replayer.staged(derivation.window)?;

        let mut ranks = Vec::with_capacity(readings.len());
        let mut samples: Result<Vec<Box<Sample>>> = Ok(Vec::with_capacity(readings.len()));
        let mut samples_bytes = 0;
        replayer.frames(&sources, &readings, |reading, frame| {
            let previous = super::sample_predecessor(
                projection.reading.as_ref(),
                projection.previous.as_deref(),
                reading,
                &frame,
            );
            let mut sample = None;
            Sample::capture(&frame, previous, &mut sample)?;
            let sample = sample.ok_or(Error::Bound("sealed Apps sample"))?;
            ranks.push(sample.ranks()?);
            samples_bytes += collector::u64_from_usize(sample.bytes());
            if samples_bytes > SAMPLES_BYTES_MAX {
                samples = Err(Error::Bound("sealed parts samples"));
            }
            if let Ok(held) = &mut samples {
                held.push(sample);
            }
            projection.previous = Some(frame);
            projection.reading = Some(*reading);
            Ok(())
        })?;
        if let Some(frame) = projection.previous {
            if let Some(reading) = projection.reading {
                self.last.replace(Some(Sampled {
                    run: identity.run,
                    reading,
                    frame,
                }));
            }
        }

        let mut tables = vec![super::sealed_ranks::encode(&ranks, derivation.pending)?];
        let parts =
            samples.and_then(|samples| super::sealed_parts::encode(&samples, derivation.pending));
        tables.extend(derive_parts(parts, derivation)?);

        Ok(tables)
    }
}

/// The table of the window's App parts, or none when its samples are more than the derivation
/// holds, its parts are more than the table holds or writing them failed, since its ranks stand
/// without them and the replay of its minute gives its parts.
fn derive_parts(
    parts: Result<TableManifest>,
    derivation: &Derivation<'_>,
) -> Result<Option<TableManifest>> {
    match parts {
        Ok(table) => Ok(Some(table)),
        Err(error) => {
            eprintln!(
                "persister derived no App parts window={}: {error}",
                derivation.window.identity.window
            );
            let path = derivation.pending.join(format!("{APP_PARTS}.parquet"));
            match std::fs::remove_file(path) {
                Ok(()) => Ok(None),
                Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(problem) => Err(problem.into()),
            }
        }
    }
}

/// What replays a sealing window: a query engine of its own over the window's staged processes
/// and events and the tables of the sealed window before it.
struct Replayer {
    connection: Engine,
    operation: Operation,
    run: RunIdentity,
}

/// What the engine names a window's processes and its events as.
struct Sources {
    processes: String,
    events: String,
}

/// The reading sampled last, whose frame gives the next reading's sample its rates.
struct Projection {
    previous: Option<Box<Frame>>,
    reading: Option<Reading>,
}

impl Replayer {
    /// The observation of `publication` in the predecessor's `directory`, from `last` when the
    /// derivation sampled it there, since a frame is the same however its run is replayed.
    fn predecessor(
        &self,
        directory: &Path,
        publication: u64,
        last: Option<Sampled>,
    ) -> Result<Projection> {
        let mut projection = Projection {
            previous: None,
            reading: None,
        };
        if let Some(last) = last {
            if last.run == self.run {
                if last.reading.observation.stamp.publication == publication {
                    projection.previous = Some(last.frame);
                    projection.reading = Some(last.reading);
                    return Ok(projection);
                }
            }
        }

        let mut before = self.readings(directory)?;
        before.retain(|reading| reading.observation.stamp.publication == publication);
        let sources = Sources {
            processes: self.source(directory, Table::Processes)?,
            events: self.source(directory, Table::Events)?,
        };
        self.frames(&sources, &before, |reading, frame| {
            projection.previous = Some(frame);
            projection.reading = Some(*reading);
            Ok(())
        })?;

        Ok(projection)
    }

    fn readings(&self, directory: &Path) -> Result<Vec<Reading>> {
        let ticks = self.source(directory, Table::Ticks)?;

        // A reading's window only batches the replay's readings, which a sealing window needs not.
        super::app_history_readings(&self.connection, &self.operation, &ticks, 0, &self.run)
    }

    /// The window's processes and events as the persister staged them, which hold what the
    /// files it wrote of them hold without a read of those files.
    fn staged(&self, window: &Window) -> Result<Sources> {
        let tables = [Table::Processes, Table::Events];
        let mut bytes = 0;
        for table in tables {
            bytes += persistence::frame_bytes(table, window)?;
        }
        if bytes > STAGED_FRAMES_BYTES_MAX {
            return Err(Error::Bound("sealed Apps staged frames"));
        }

        for table in tables {
            self.connection
                .register(table.name(), persistence::frame(table, window)?)?;
        }
        Ok(Sources {
            processes: table_source(&self.connection, Table::Processes, TableSource::Staged)?,
            events: table_source(&self.connection, Table::Events, TableSource::Staged)?,
        })
    }

    fn frames(
        &self,
        sources: &Sources,
        readings: &[Reading],
        mut consume: impl FnMut(&Reading, Box<Frame>) -> Result<()>,
    ) -> Result<()> {
        let source = crate::live_apps::committed::Sources {
            processes: &sources.processes,
            events: &sources.events,
        }
        .query();
        let endpoints: Vec<_> = readings.iter().map(Reading::endpoint).collect();
        let mut index = 0;
        Frame::committed_many(
            &endpoints,
            &self.connection,
            &source,
            &self.operation,
            |frame| {
                let reading = readings
                    .get(index)
                    .ok_or(Error::Bound("sealed Apps frames"))?;
                if frame.stamp() != &reading.observation.stamp {
                    return Err(Error::Bound("sealed Apps frame association"));
                }
                consume(reading, frame)?;
                index += 1;
                Ok(())
            },
        )?;
        assert_eq!(index, readings.len());

        Ok(())
    }

    fn source(&self, directory: &Path, table: Table) -> Result<String> {
        let path = directory.join(format!("{}.parquet", table.name()));
        table_source(&self.connection, table, TableSource::Parquet(&path))
    }
}
