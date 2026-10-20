//! The App ranks of each observation of a sealed window. The persister derives them as the
//! window seals, so the viewer shows a saved second's top Apps without replaying its minute.
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use polars::prelude::{Column, DataFrame};

use super::{ContextRange, Frame, Reading, Session, TICKS_PER_WINDOW_MAX};
use crate::cancellation::{Cancellation, Operation};
use crate::live_app_response::{
    KeyKind, METRICS, Metric, Observation, RANKS_MAX, Rank, RankedApp, Ranking, Ranks, Sample,
};
use collector::ring::RunIdentity;
use persistence::engine::{Engine, Row};
use persistence::query::{TableSource, table_source};
use persistence::schema::{APP_RANKS, Table};
use persistence::worker::{Derivation, Derive};
use persistence::{Error, Result, TableManifest};

/// Ranks each observation of a window as it seals, from the tables the persister wrote for it,
/// by the rule the replay of its minute ranks it.
pub struct SealedRanks;

/// The version of the table's columns. A reader takes no ranks from a table of another version
/// and leaves its window to the replay, so a later build may change the columns and an older
/// one still reads the recording.
const VERSION: u8 = 1;
/// The four rankings of each observation a window holds, full.
const ROWS_MAX: usize = TICKS_PER_WINDOW_MAX * METRICS.len() * RANKS_MAX;
const COLUMNS: &str = "version, publication, run_tick, wall_ns, continuous_ns, awake_ns, \
    app_count, membership_complete, ungrouped_members, metric, unavailable, overflowed, partial, \
    position, app_kind, app_key, amount, rate, measured_members, unavailable_members";

const _: () = {
    assert!(RANKS_MAX < 256);
    assert!(ROWS_MAX < 4096);
    // A row names its ranking by its place among the rankings `Ranks` holds.
    assert!(METRICS.len() == 4);
    assert!(metric_code(METRICS[0]) == 0);
    assert!(metric_code(METRICS[1]) == 1);
    assert!(metric_code(METRICS[2]) == 2);
    assert!(metric_code(METRICS[3]) == 3);
};

impl Derive for SealedRanks {
    fn derive(&self, derivation: &Derivation<'_>) -> Result<Vec<TableManifest>> {
        let ranker = Ranker {
            connection: Engine::new(),
            operation: Arc::new(Cancellation::default()).operation(0)?,
            run: derivation.identity.run,
        };
        let readings = ranker.readings(derivation.pending)?;
        let first = readings.first().ok_or(Error::Bound("sealed ranks ticks"))?;
        let mut projection = Projection {
            previous: None,
            reading: None,
        };
        if let Some(directory) = derivation.predecessor {
            if let Some(publication) = first.observation.stamp.publication.checked_sub(1) {
                let mut before = ranker.readings(directory)?;
                before.retain(|reading| reading.observation.stamp.publication == publication);
                ranker.frames(directory, &before, |reading, frame| {
                    projection.previous = Some(frame);
                    projection.reading = Some(*reading);
                    Ok(())
                })?;
            }
        }

        let mut ranks = Vec::with_capacity(readings.len());
        ranker.frames(derivation.pending, &readings, |reading, frame| {
            let previous = super::sample_predecessor(
                projection.reading.as_ref(),
                projection.previous.as_deref(),
                reading,
                &frame,
            );
            let mut sample = None;
            Sample::capture(&frame, previous, &mut sample)?;
            ranks.push(sample.ok_or(Error::Bound("sealed ranks sample"))?.ranks()?);
            projection.previous = Some(frame);
            projection.reading = Some(*reading);
            Ok(())
        })?;
        let mut frame = sealed_ranks_frame(&ranks)?;

        Ok(vec![persistence::encode_derived(
            APP_RANKS,
            &mut frame,
            derivation.pending,
        )?])
    }
}

/// What ranks a sealing window: a query engine of its own over the window's tables and those of
/// the sealed window before it.
struct Ranker {
    connection: Engine,
    operation: Operation,
    run: RunIdentity,
}

/// The reading ranked last, whose frame gives the next reading's sample its rates.
struct Projection {
    previous: Option<Box<Frame>>,
    reading: Option<Reading>,
}

impl Ranker {
    fn readings(&self, directory: &Path) -> Result<Vec<Reading>> {
        let ticks = self.source(directory, Table::Ticks)?;

        // A reading's window only batches the replay's readings, which ranking a window needs not.
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
                    .ok_or(Error::Bound("sealed ranks frames"))?;
                if frame.stamp() != &reading.observation.stamp {
                    return Err(Error::Bound("sealed ranks frame association"));
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

/// One row of the table: one ranking of an observation, and the App at `position` in it when it
/// ranks any.
struct RankRow<'ranks> {
    ranks: &'ranks Ranks,
    ranking: &'ranks Ranking,
    position: Option<u8>,
}

impl<'ranks> RankRow<'ranks> {
    fn rank(&self) -> Option<&'ranks Rank> {
        self.position
            .map(|position| &self.ranking.entries[usize::from(position)])
    }
}

macro_rules! rank_columns {
    ($rows:ident; $($name:literal => $value:expr),+ $(,)?) => {
        vec![$(
            Column::new($name.into(), $rows.iter().map($value).collect::<Vec<_>>())
        ),+]
    };
}

fn sealed_ranks_frame(ranks: &[Ranks]) -> Result<DataFrame> {
    let rows = sealed_ranks_rows(ranks)?;
    let columns = rank_columns!(rows;
        "version" => |_| VERSION,
        "publication" => |row| row.ranks.selected.stamp.publication,
        "run_tick" => |row| row.ranks.selected.run_tick,
        "wall_ns" => |row| row.ranks.selected.wall_ns,
        "continuous_ns" => |row| row.ranks.selected.stamp.continuous_ns,
        "awake_ns" => |row| row.ranks.selected.stamp.awake_ns,
        "app_count" => |row| row.ranks.app_count,
        "membership_complete" => |row| row.ranks.membership_complete,
        "ungrouped_members" => |row| row.ranks.ungrouped_members,
        "metric" => |row| metric_code(row.ranking.metric),
        "unavailable" => |row| row.ranking.unavailable,
        "overflowed" => |row| row.ranking.overflowed,
        "partial" => |row| row.ranking.partial,
        "position" => |row| row.position,
        "app_kind" => |row| row.rank().map(|rank| kind_code(rank.app.kind)),
        "app_key" => |row| row.rank().map(|rank| rank.app.bytes.as_slice()),
        "amount" => |row| row.rank().map(|rank| rank.amount),
        "rate" => |row| row.rank().and_then(|rank| rank.rate),
        "measured_members" => |row| row.rank().map(|rank| rank.measured_members),
        "unavailable_members" => |row| row.rank().map(|rank| rank.unavailable_members),
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}

/// A row for each App each ranking ranks, and one for each ranking that ranks none.
fn sealed_ranks_rows(ranks: &[Ranks]) -> Result<Vec<RankRow<'_>>> {
    if ranks.len() > TICKS_PER_WINDOW_MAX {
        return Err(Error::Bound("sealed ranks observations"));
    }

    let mut rows = Vec::with_capacity(ROWS_MAX);
    for observation in ranks {
        for ranking in &observation.metrics {
            assert!(ranking.entries.len() <= RANKS_MAX);
            if ranking.entries.is_empty() {
                rows.push(RankRow {
                    ranks: observation,
                    ranking,
                    position: None,
                });
            }
            for (position, _rank) in (0_u8..).zip(&ranking.entries) {
                rows.push(RankRow {
                    ranks: observation,
                    ranking,
                    position: Some(position),
                });
            }
        }
    }
    assert!(rows.len() <= ROWS_MAX);

    Ok(rows)
}

impl Session {
    /// Gives `consume` the ranks the windows of `range` keep of their observations in it, from
    /// the table the persister derived as each window sealed. A window sealed without the table,
    /// or with a version of it this build does not read, gives none.
    pub(crate) fn visit_sealed_ranks(
        &self,
        range: ContextRange,
        mut consume: impl FnMut(Ranks) -> Result<()>,
    ) -> Result<()> {
        self.operation.attach(&self.connection)?;
        assert!(self.windows.len() <= super::super::WINDOWS_MAX);
        for (window, manifest) in self.windows.iter().enumerate() {
            self.operation.check()?;
            if let Some(bounds) = manifest.wall_ns {
                if bounds.max < range.first_ns {
                    continue;
                }
                if bounds.min >= range.end_ns {
                    continue;
                }
            }
            let Some(table) = manifest.tables.iter().find(|table| table.name == APP_RANKS) else {
                continue;
            };

            self.validate_commitment(window)?;
            let mut ranks = Vec::with_capacity(TICKS_PER_WINDOW_MAX);
            let kept = Kept {
                path: &self
                    .root
                    .join(&manifest.data_directory)
                    .join(format!("{APP_RANKS}.parquet")),
                run: &manifest.identity.run,
                publications: manifest.publication_first..manifest.publication_end,
                rows: table.rows,
            };
            self.visit_sealed_ranks_read(&kept, &mut ranks)?;
            self.validate_commitment(window)?;

            for observation in ranks {
                let inside = observation
                    .selected
                    .wall_ns
                    .is_some_and(|wall| (range.first_ns..range.end_ns).contains(&wall));
                if inside {
                    consume(observation)?;
                }
            }
        }

        self.operation.check()
    }

    fn visit_sealed_ranks_read(&self, kept: &Kept<'_>, output: &mut Vec<Ranks>) -> Result<()> {
        assert!(output.is_empty());
        let source = self.connection.parquet_source(kept.path)?;
        let version = self
            .connection
            .query(&format!("SELECT version FROM {source} LIMIT 1"))?;
        if version.height() == 0 {
            return Err(Error::Bound("sealed ranks rows"));
        }
        if Row::new(&version, 0)?.get::<u8>(0)? != VERSION {
            return Ok(());
        }

        let limit = ROWS_MAX + 1;
        let frame = self.connection.query(&format!(
            "SELECT {COLUMNS} FROM {source} ORDER BY publication, metric, position LIMIT {limit}"
        ))?;
        if collector::u64_from_usize(frame.height()) != kept.rows {
            return Err(Error::Bound("sealed ranks rows"));
        }
        let mut decoder = Decoder {
            publications: kept.publications.clone(),
            output,
            rankings: 0,
            observation: None,
            ranking: None,
        };
        for index in 0..frame.height() {
            self.operation.check()?;
            decoder.push(sealed_ranks_row(&Row::new(&frame, index)?, kept.run)?)?;
        }

        decoder.close()
    }
}

/// A sealed window's table of ranks, and what its seal recorded of it.
struct Kept<'manifest> {
    path: &'manifest Path,
    run: &'manifest RunIdentity,
    publications: Range<u64>,
    rows: u64,
}

/// One row of the table as read.
struct KeptRow {
    version: u8,
    observation: KeptObservation,
    metric: u8,
    ranking: KeptRanking,
    position: Option<u8>,
    rank: Option<Rank>,
}

/// What every row of one observation repeats.
#[derive(Clone, Copy, PartialEq)]
struct KeptObservation {
    selected: Observation,
    app_count: u32,
    membership_complete: bool,
    ungrouped_members: u32,
}

/// What every row of one ranking repeats.
#[derive(Clone, Copy, PartialEq)]
struct KeptRanking {
    unavailable: u32,
    overflowed: u32,
    partial: u32,
}

fn sealed_ranks_row(row: &Row<'_>, run: &RunIdentity) -> Result<KeptRow> {
    let position: Option<u8> = row.get(13)?;
    let rank = match position {
        None => None,
        Some(_) => Some(Rank {
            app: Arc::new(RankedApp::new(kind_of(row.get(14)?)?, row.get(15)?)?),
            amount: row.get(16)?,
            rate: row.get(17)?,
            measured_members: row.get(18)?,
            unavailable_members: row.get(19)?,
        }),
    };

    Ok(KeptRow {
        version: row.get(0)?,
        observation: KeptObservation {
            selected: Observation {
                recording: run.recording.0,
                stamp: crate::apps::Stamp {
                    boot: run.boot.0,
                    run: run.run.0,
                    publication: row.get(1)?,
                    continuous_ns: row.get(4)?,
                    awake_ns: row.get(5)?,
                },
                run_tick: row.get(2)?,
                wall_ns: row.get(3)?,
            },
            app_count: row.get(6)?,
            membership_complete: row.get(7)?,
            ungrouped_members: row.get(8)?,
        },
        metric: row.get(9)?,
        ranking: KeptRanking {
            unavailable: row.get(10)?,
            overflowed: row.get(11)?,
            partial: row.get(12)?,
        },
        position,
        rank,
    })
}

/// Rebuilds the ranks of a window's observations from its rows, in the order the table holds
/// them: by publication, then metric in the order `Ranks` holds them, then position.
struct Decoder<'output> {
    publications: Range<u64>,
    output: &'output mut Vec<Ranks>,
    /// The rankings of the last observation `output` holds that rows have opened.
    rankings: usize,
    observation: Option<KeptObservation>,
    ranking: Option<KeptRanking>,
}

impl Decoder<'_> {
    fn push(&mut self, row: KeptRow) -> Result<()> {
        if row.version != VERSION {
            return Err(Error::Bound("sealed ranks version"));
        }

        let publication = row.observation.selected.stamp.publication;
        let opens = self
            .observation
            .is_none_or(|observation| observation.selected.stamp.publication != publication);
        if opens {
            self.close()?;
            self.open(&row.observation)?;
        }
        if self.observation != Some(row.observation) {
            return Err(Error::Bound("sealed ranks observation"));
        }

        let ranks = self
            .output
            .last_mut()
            .ok_or(Error::Bound("sealed ranks observation"))?;
        let metric = usize::from(row.metric);
        if metric == self.rankings {
            if metric == METRICS.len() {
                return Err(Error::Bound("sealed ranks metric"));
            }
            if row.position.is_some_and(|position| position != 0) {
                return Err(Error::Bound("sealed ranks position"));
            }

            let ranking = &mut ranks.metrics[metric];
            ranking.unavailable = row.ranking.unavailable;
            ranking.overflowed = row.ranking.overflowed;
            ranking.partial = row.ranking.partial;
            ranking.entries.extend(row.rank);
            self.ranking = Some(row.ranking);
            self.rankings += 1;
        } else {
            if metric + 1 != self.rankings {
                return Err(Error::Bound("sealed ranks metric"));
            }
            if self.ranking != Some(row.ranking) {
                return Err(Error::Bound("sealed ranks ranking"));
            }

            let ranking = &mut ranks.metrics[metric];
            let count = ranking.entries.len();
            // A ranking of no App is its one row, so a row after it has no place in it.
            if count == 0 {
                return Err(Error::Bound("sealed ranks position"));
            }
            if count == RANKS_MAX {
                return Err(Error::Bound("sealed ranks position"));
            }
            if row.position.map(usize::from) != Some(count) {
                return Err(Error::Bound("sealed ranks position"));
            }
            ranking.entries.extend(row.rank);
        }

        Ok(())
    }

    fn open(&mut self, observation: &KeptObservation) -> Result<()> {
        if !self
            .publications
            .contains(&observation.selected.stamp.publication)
        {
            return Err(Error::Bound("sealed ranks publication"));
        }
        if self.output.len() == TICKS_PER_WINDOW_MAX {
            return Err(Error::Bound("sealed ranks observations"));
        }

        self.output.push(Ranks {
            selected: observation.selected,
            app_count: observation.app_count,
            membership_complete: observation.membership_complete,
            ungrouped_members: observation.ungrouped_members,
            metrics: METRICS.map(Ranking::new),
        });
        self.observation = Some(*observation);
        self.ranking = None;
        self.rankings = 0;

        Ok(())
    }

    /// Ends the last observation, which every ranking must have opened.
    fn close(&self) -> Result<()> {
        if self.observation.is_some() {
            if self.rankings != METRICS.len() {
                return Err(Error::Bound("sealed ranks metrics"));
            }
        }

        Ok(())
    }
}

const fn metric_code(metric: Metric) -> u8 {
    match metric {
        Metric::Cpu => 0,
        Metric::Resident => 1,
        Metric::Power => 2,
        Metric::Disk => 3,
    }
}

fn kind_code(kind: KeyKind) -> u8 {
    match kind {
        KeyKind::Bundle => 0,
        KeyKind::Executable => 1,
    }
}

fn kind_of(code: u8) -> Result<KeyKind> {
    match code {
        0 => Ok(KeyKind::Bundle),
        1 => Ok(KeyKind::Executable),
        _ => Err(Error::Bound("sealed ranks App kind")),
    }
}

#[cfg(test)]
#[path = "sealed_ranks_tests.rs"]
mod tests;
