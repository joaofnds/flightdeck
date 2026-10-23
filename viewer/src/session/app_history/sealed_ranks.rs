//! The App ranks of each observation of a sealed window. The persister derives them as the
//! window seals, so the viewer shows a saved second's top Apps without replaying its minute.
use std::ops::Range;
use std::sync::Arc;

use super::sealed::{Kept, kind_of, observed_in};
use super::{ContextRange, Session, TICKS_PER_WINDOW_MAX};
use crate::live_app_response::{METRICS, Observation, RANKS_MAX, Rank, RankedApp, Ranking, Ranks};
use collector::ring::RunIdentity;
use persistence::app_tables::{RANKS_COLUMNS, RANKS_VERSION};
use persistence::engine::Row;
use persistence::schema::APP_RANKS;
use persistence::{Error, Result};

/// The five rankings of each observation a window holds, full.
const ROWS_MAX: usize = TICKS_PER_WINDOW_MAX * METRICS.len() * RANKS_MAX;

const _: () = {
    assert!(RANKS_MAX < 256);
    assert!(ROWS_MAX < 4096);
};

impl Session {
    /// Gives `consume` the ranks the windows of `range` keep of their observations in it, from
    /// the table the persister derived as each window sealed. A window sealed without the table,
    /// or with a version of it this build does not read, gives none.
    pub(crate) fn visit_sealed_ranks(
        &self,
        range: ContextRange,
        mut consume: impl FnMut(Ranks) -> Result<()>,
    ) -> Result<()> {
        self.visit_derived(
            range,
            APP_RANKS,
            |kept| {
                let mut ranks = Vec::with_capacity(TICKS_PER_WINDOW_MAX);
                self.visit_sealed_ranks_read(kept, &mut ranks)?;
                Ok(ranks)
            },
            |_window, ranks| {
                for observation in ranks {
                    if observed_in(range, &observation.selected) {
                        consume(observation)?;
                    }
                }
                Ok(())
            },
        )
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
        if Row::new(&version, 0)?.get::<u8>(0)? != RANKS_VERSION {
            return Ok(());
        }

        let limit = ROWS_MAX + 1;
        let columns = RANKS_COLUMNS.join(", ");
        let frame = self.connection.query(&format!(
            "SELECT {columns} FROM {source} ORDER BY publication, metric, position LIMIT {limit}"
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
        if row.version != RANKS_VERSION {
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

#[cfg(test)]
#[path = "sealed_ranks_tests.rs"]
mod tests;
