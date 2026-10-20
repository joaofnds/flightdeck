//! The App ranks of each observation of a sealed window. The persister derives them as the
//! window seals, so the viewer shows a saved second's top Apps without replaying its minute.
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use polars::prelude::DataFrame;

use super::sealed::{Kept, columns, kind_code, kind_of, observed_in};
use super::{ContextRange, Session, TICKS_PER_WINDOW_MAX};
use crate::live_app_response::{
    METRICS, Metric, Observation, RANKS_MAX, Rank, RankedApp, Ranking, Ranks,
};
use collector::ring::RunIdentity;
use persistence::engine::Row;
use persistence::schema::APP_RANKS;
use persistence::{Error, Result, TableManifest};

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

/// Writes the ranks of a window's observations into its pending directory, in the table the
/// window seals with.
pub(super) fn encode(ranks: &[Ranks], pending: &Path) -> Result<TableManifest> {
    let mut frame = sealed_ranks_frame(ranks)?;

    persistence::encode_derived(APP_RANKS, &mut frame, pending)
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

fn sealed_ranks_frame(ranks: &[Ranks]) -> Result<DataFrame> {
    let rows = sealed_ranks_rows(ranks)?;
    let columns = columns!(rows;
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
        self.visit_derived(
            range,
            APP_RANKS,
            |kept| {
                let mut ranks = Vec::with_capacity(TICKS_PER_WINDOW_MAX);
                self.visit_sealed_ranks_read(kept, &mut ranks)?;
                Ok(ranks)
            },
            |_window, observation| {
                if observed_in(range, &observation.selected) {
                    consume(observation)?;
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

#[cfg(test)]
#[path = "sealed_ranks_tests.rs"]
mod tests;
