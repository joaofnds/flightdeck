use std::collections::BTreeMap;

use persistence::engine::Engine;
use persistence::{Error, Result};

use super::number;
use crate::cancellation::Operation;
use crate::data::{self, Table};

/// The table that sums each performance level's time in each active clock state, the shape
/// the history and the live view both read core states in.
pub(crate) const CORE_LEVEL_STATES: &str = "core_level_states";

/// The sources a window's core states are summed by performance level from.
pub(crate) struct CoreLevelSources<'source> {
    pub core_states: &'source str,
    pub ticks: &'source str,
}

/// The recorded core states summed by performance level, with no rows for a tick that
/// recorded no two levels, since no core can then be placed in a level.
pub(crate) fn core_level_states(
    connection: &Engine,
    sources: &CoreLevelSources<'_>,
    operation: &Operation,
) -> Result<Table> {
    data::query(
        connection,
        &core_level_states_query(sources),
        data::ROWS_MAX,
        operation,
    )
}

/// The query that sums the recorded core states into each performance level's time in each
/// active state, level 0 the fastest. macOS numbers the slower level's cores first, so a core
/// below the slower level's logical CPU count of its tick is in level 1. A state's clock is
/// the one every core of the level recorded for it, and null where one recorded none or they
/// differ. Summing every core's time before the interval keeps a history of fifteen minutes
/// within the viewer's cell bound.
pub(crate) fn core_level_states_query(sources: &CoreLevelSources<'_>) -> String {
    let CoreLevelSources { core_states, ticks } = sources;
    format!(
        "SELECT publication, level, state, SUM(active_ns) AS active_ns, \
         CASE WHEN COUNT(frequency_khz) = COUNT(*) \
         AND MIN(frequency_khz) = MAX(frequency_khz) \
         THEN MIN(frequency_khz) END AS frequency_khz FROM (\
         SELECT states.publication, \
         CASE WHEN states.core < levels.slower THEN 1 ELSE 0 END AS level, \
         states.state, states.active_ns, states.frequency_khz FROM {core_states} AS states \
         JOIN (SELECT publication, machine_performance_level1_logical_cpus AS slower \
         FROM {ticks} WHERE machine_performance_level1_logical_cpus IS NOT NULL) AS levels \
         ON states.publication = levels.publication) \
         GROUP BY publication, level, state ORDER BY publication, level, state"
    )
}

/// The mean clock of each performance level while its cores were active over the interval
/// from `previous` to `current`, in GHz, level 0 first. Each active state's clock is weighted
/// by the time the level's cores spent in it. A level with no active time, or with a state
/// whose clock is unknown, reads nothing.
pub(super) fn level_ghz(current: &Table, previous: &Table) -> Result<[Option<f64>; 2]> {
    let mut levels = [None, None];
    for (level, ghz) in (0_u64..).zip(levels.iter_mut()) {
        let after = level_ghz_states(current, level)?;
        let before = level_ghz_states(previous, level)?;
        if after.len() != before.len() {
            continue;
        }
        *ghz = level_ghz_mean(&after, &before);
    }
    Ok(levels)
}

/// The time `table` holds in each state of `level`, and its clock in kHz, by state index.
fn level_ghz_states(table: &Table, level: u64) -> Result<BTreeMap<u64, (u64, Option<u64>)>> {
    let mut states = BTreeMap::new();
    for row in &table.rows {
        if table.integer(row, "level")? != Some(level) {
            continue;
        }
        let state = table
            .integer(row, "state")?
            .ok_or(Error::Bound("core state index"))?;
        let active_ns = table
            .integer(row, "active_ns")?
            .ok_or(Error::Bound("core state time"))?;
        let frequency_khz = table.integer(row, "frequency_khz")?;
        if states.insert(state, (active_ns, frequency_khz)).is_some() {
            return Err(Error::Bound("duplicate core state"));
        }
    }
    Ok(states)
}

/// The time-weighted mean of the states' clocks in kHz, over the time each state gained from
/// `before` to `after`, in GHz, or none where a state lost time, has no clock, or none was
/// gained.
fn level_ghz_mean(
    after: &BTreeMap<u64, (u64, Option<u64>)>,
    before: &BTreeMap<u64, (u64, Option<u64>)>,
) -> Option<f64> {
    let mut weighted = 0.0;
    let mut total = 0.0;
    for (state, (now, frequency_khz)) in after {
        let (then, _) = before.get(state)?;
        let gained = now.checked_sub(*then)?;
        weighted += number(gained) * number((*frequency_khz)?);
        total += number(gained);
    }
    if total <= 0.0 {
        return None;
    }
    Some(weighted / total / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A core level states table of rows of level, state, active time and clock in kHz.
    fn level_table(rows: &[(u64, u64, u64, Option<u64>)]) -> Table {
        Table {
            columns: [
                "publication",
                "level",
                "state",
                "active_ns",
                "frequency_khz",
            ]
            .map(String::from)
            .to_vec(),
            rows: rows
                .iter()
                .map(|&(level, state, active_ns, frequency_khz)| {
                    let mut row = [1, level, state, active_ns]
                        .map(|value| Some(value.to_string()))
                        .to_vec();
                    row.push(frequency_khz.map(|value| value.to_string()));
                    row
                })
                .collect(),
        }
    }

    #[test]
    fn level_ghz_weights_each_state_by_its_active_time() {
        // Goal: a level reads the mean of its state clocks weighted by the time gained.
        // Method: the fast level gains 1 s at 1 GHz and 3 s at 3 GHz, the slow level 2 s at
        // 2 GHz alone.
        let ghz = |ghz: u64| Some(ghz * 1_000_000);
        let previous = level_table(&[
            (0, 0, 5, ghz(1)),
            (0, 1, 5, ghz(3)),
            (1, 0, 0, ghz(1)),
            (1, 1, 0, ghz(2)),
            (1, 2, 0, ghz(4)),
        ]);
        let current = level_table(&[
            (0, 0, 1_000_000_005, ghz(1)),
            (0, 1, 3_000_000_005, ghz(3)),
            (1, 0, 0, ghz(1)),
            (1, 1, 2_000_000_000, ghz(2)),
            (1, 2, 0, ghz(4)),
        ]);

        let levels = level_ghz(&current, &previous).unwrap();

        assert_eq!(levels, [Some(2.5), Some(2.0)]);
    }

    #[test]
    fn level_ghz_reads_nothing_without_active_time_or_a_known_clock() {
        // Goal: an idle level, and a level with a state of unknown clock, read nothing rather
        // than a mean over part of the level's time.
        // Method: the fast level gains no time, and the slow level gains time in a state the
        // collector knew no clock for.
        let previous = level_table(&[(0, 0, 7, Some(1)), (0, 1, 7, Some(2)), (1, 0, 0, None)]);
        let current = level_table(&[(0, 0, 7, Some(1)), (0, 1, 7, Some(2)), (1, 0, 9, None)]);

        let levels = level_ghz(&current, &previous).unwrap();

        assert_eq!(levels, [None, None]);
    }

    #[test]
    fn level_ghz_reads_nothing_where_a_state_lost_time() {
        // Goal: a counter that went backward reads no clock rather than a wrapped one.
        // Method: the fast level's second state falls from 9 to 8.
        let previous = level_table(&[(0, 0, 0, Some(1_000_000)), (0, 1, 9, Some(2_000_000))]);
        let current = level_table(&[(0, 0, 5, Some(1_000_000)), (0, 1, 8, Some(2_000_000))]);

        let levels = level_ghz(&current, &previous).unwrap();

        assert_eq!(levels, [None, None]);
    }

    #[test]
    fn core_level_states_query_sums_each_level_state_by_its_ticks_levels() {
        // Goal: the query sums every core's time in a state into its level, the cores below
        // the slower level's count in level 1, keeps the clock the level's cores share, and
        // leaves out a tick that recorded no levels.
        // Method: two slow and one fast core over two states in a tick with levels, where the
        // slow cores record different clocks for state 1, and a tick without levels.
        let connection = data::connection();
        let core_states = polars::df!(
            "publication" => [1_u64, 1, 1, 1, 1, 1, 2],
            "core" => [0_u32, 0, 1, 1, 2, 2, 0],
            "state" => [0_u32, 1, 0, 1, 0, 1, 0],
            "active_ns" => [1_u64, 2, 3, 4, 5, 6, 7],
            "frequency_khz" => [
                Some(10_u32),
                Some(20),
                Some(10),
                Some(21),
                None,
                Some(30),
                Some(10),
            ],
        )
        .unwrap();
        let ticks = polars::df!(
            "publication" => [1_u64, 2],
            "machine_performance_level1_logical_cpus" => [Some(2_u32), None],
        )
        .unwrap();
        connection.register("core_states", core_states).unwrap();
        connection.register("ticks", ticks).unwrap();
        let sources = CoreLevelSources {
            core_states: "core_states",
            ticks: "ticks",
        };

        let table = data::query(
            &connection,
            &core_level_states_query(&sources),
            data::ROWS_MAX,
            &std::sync::Arc::new(crate::cancellation::Cancellation::default())
                .operation(0)
                .unwrap(),
        )
        .unwrap();

        let rows: Vec<_> = table
            .rows
            .iter()
            .map(|row| {
                ["level", "state", "active_ns", "frequency_khz"]
                    .map(|name| table.integer(row, name).unwrap())
            })
            .collect();
        assert_eq!(
            rows,
            [
                [Some(0), Some(0), Some(5), None],
                [Some(0), Some(1), Some(6), Some(30)],
                [Some(1), Some(0), Some(4), Some(10)],
                [Some(1), Some(1), Some(6), None],
            ]
        );
    }
}
