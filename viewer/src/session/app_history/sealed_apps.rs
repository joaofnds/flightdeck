//! What the persister derives of the Apps of each observation of a window as it seals, so the
//! viewer shows a saved second's Apps without replaying its minute.
use std::path::Path;
use std::sync::Arc;

use super::{Frame, Reading};
use crate::cancellation::{Cancellation, Operation};
use crate::live_app_response::Sample;
use collector::ring::RunIdentity;
use persistence::engine::Engine;
use persistence::query::{TableSource, table_source};
use persistence::schema::Table;
use persistence::worker::{Derivation, Derive};
use persistence::{Error, Result, TableManifest};

/// Samples each observation of a window as it seals, from the tables the persister wrote for
/// it, by the rule the replay of its minute samples it, and keeps the Apps' ranks and every
/// App's part.
pub struct SealedApps;

impl Derive for SealedApps {
    fn derive(&self, derivation: &Derivation<'_>) -> Result<Vec<TableManifest>> {
        let replayer = Replayer {
            connection: Engine::new(),
            operation: Arc::new(Cancellation::default()).operation(0)?,
            run: derivation.identity.run,
        };
        let readings = replayer.readings(derivation.pending)?;
        let first = readings.first().ok_or(Error::Bound("sealed Apps ticks"))?;
        let mut projection = Projection {
            previous: None,
            reading: None,
        };
        if let Some(directory) = derivation.predecessor {
            if let Some(publication) = first.observation.stamp.publication.checked_sub(1) {
                let mut before = replayer.readings(directory)?;
                before.retain(|reading| reading.observation.stamp.publication == publication);
                replayer.frames(directory, &before, |reading, frame| {
                    projection.previous = Some(frame);
                    projection.reading = Some(*reading);
                    Ok(())
                })?;
            }
        }

        let mut samples = Vec::with_capacity(readings.len());
        replayer.frames(derivation.pending, &readings, |reading, frame| {
            let previous = super::sample_predecessor(
                projection.reading.as_ref(),
                projection.previous.as_deref(),
                reading,
                &frame,
            );
            let mut sample = None;
            Sample::capture(&frame, previous, &mut sample)?;
            samples.push(sample.ok_or(Error::Bound("sealed Apps sample"))?);
            projection.previous = Some(frame);
            projection.reading = Some(*reading);
            Ok(())
        })?;

        let ranks = samples
            .iter()
            .map(|sample| sample.ranks())
            .collect::<Result<Vec<_>>>()?;

        Ok(vec![
            super::sealed_ranks::encode(&ranks, derivation.pending)?,
            super::sealed_parts::encode(&samples, derivation.pending)?,
        ])
    }
}

/// What replays a sealing window: a query engine of its own over the window's tables and those
/// of the sealed window before it.
struct Replayer {
    connection: Engine,
    operation: Operation,
    run: RunIdentity,
}

/// The reading sampled last, whose frame gives the next reading's sample its rates.
struct Projection {
    previous: Option<Box<Frame>>,
    reading: Option<Reading>,
}

impl Replayer {
    fn readings(&self, directory: &Path) -> Result<Vec<Reading>> {
        let ticks = self.source(directory, Table::Ticks)?;

        // A reading's window only batches the replay's readings, which a sealing window needs not.
        super::app_history_readings(&self.connection, &self.operation, &ticks, 0, &self.run)
    }

    fn frames(
        &self,
        directory: &Path,
        readings: &[Reading],
        mut consume: impl FnMut(&Reading, Box<Frame>) -> Result<()>,
    ) -> Result<()> {
        let processes = self.source(directory, Table::Processes)?;
        let events = self.source(directory, Table::Events)?;
        let source = crate::live_apps::committed::Sources {
            processes: &processes,
            events: &events,
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
