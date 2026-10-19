use std::collections::BTreeMap;
use std::sync::OnceLock;

use collector::clock_states::ClockTables;
use persistence::engine::Engine;
use persistence::{Error, Result};

use super::number;
use crate::cancellation::Operation;
use crate::data::{self, Table};

/// The table that sums each performance level's time in each active clock state, the shape
/// the history and the live view both read core states in.
pub(crate) const CORE_LEVEL_STATES: &str = "core_level_states";

/// The recorded `core_states` at `source` summed by performance level, or no rows on a Mac
/// whose levels are unknown, since no core can then be placed in a level.
pub(crate) fn core_level_states(
    connection: &Engine,
    source: &str,
    operation: &Operation,
) -> Result<Table> {
    let Some(levels) = crate::cpu_history_sample::this_mac_levels()? else {
        return Ok(Table {
            columns: ["publication", "level", "state", "active_ns"]
                .map(String::from)
                .to_vec(),
            rows: Vec::new(),
        });
    };
    let [_fast, slow] = levels.logical_cpus;
    data::query(
        connection,
        &core_level_states_query(source, slow),
        data::ROWS_MAX,
        operation,
    )
}

/// The query that sums the recorded `core_states` at `source` into each performance level's
/// time in each active state, level 0 the fastest. macOS numbers the slower level's cores
/// first, so a core below `slow_cores` is in level 1. Summing every core's time before the
/// interval keeps a history of fifteen minutes within the viewer's cell bound.
pub(crate) fn core_level_states_query(source: &str, slow_cores: u32) -> String {
    format!(
        "SELECT publication, level, state, SUM(active_ns) AS active_ns FROM (\
         SELECT publication, CASE WHEN core < {slow_cores} THEN 1 ELSE 0 END AS level, \
         state, active_ns FROM {source}) \
         GROUP BY publication, level, state ORDER BY publication, level, state"
    )
}

/// This Mac's clock tables, read once, or none where they could not be read.
fn clock_tables() -> Option<&'static ClockTables> {
    static TABLES: OnceLock<Option<ClockTables>> = OnceLock::new();
    TABLES
        .get_or_init(|| match collector::clock_states::read() {
            Ok(tables) => Some(tables),
            Err(error) => {
                eprintln!("viewer: clock tables unreadable: {error}");
                None
            }
        })
        .as_ref()
}

/// The mean clock of each performance level while its cores were active over the interval
/// from `previous` to `current`, in GHz, level 0 first. Each active state's frequency is
/// weighted by the time the level's cores spent in it. A level with no active time, or
/// whose state count matches no single clock table, reads nothing.
pub(super) fn level_ghz(current: &Table, previous: &Table) -> Result<[Option<f64>; 2]> {
    let Some(clock) = clock_tables() else {
        return Ok([None, None]);
    };
    level_ghz_with(current, previous, clock)
}

fn level_ghz_with(
    current: &Table,
    previous: &Table,
    clock: &ClockTables,
) -> Result<[Option<f64>; 2]> {
    let mut levels = [None, None];
    for (level, ghz) in (0_u64..).zip(levels.iter_mut()) {
        let after = level_ghz_states(current, level)?;
        let before = level_ghz_states(previous, level)?;
        if after.len() != before.len() {
            continue;
        }
        let Some(frequencies) = clock.for_states(after.len()) else {
            continue;
        };
        *ghz = level_ghz_mean(&after, &before, frequencies);
    }
    Ok(levels)
}

/// The time `table` holds in each state of `level`, by state index.
fn level_ghz_states(table: &Table, level: u64) -> Result<BTreeMap<u64, u64>> {
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
        if states.insert(state, active_ns).is_some() {
            return Err(Error::Bound("duplicate core state"));
        }
    }
    Ok(states)
}

/// The time-weighted mean of `frequencies`, in kilohertz, over the time each state gained
/// from `before` to `after`, in GHz, or none where a state lost time or none was gained.
fn level_ghz_mean(
    after: &BTreeMap<u64, u64>,
    before: &BTreeMap<u64, u64>,
    frequencies: &[u32],
) -> Option<f64> {
    assert_eq!(after.len(), frequencies.len());

    let mut weighted = 0.0;
    let mut total = 0.0;
    for ((state, now), frequency) in after.iter().zip(frequencies) {
        let gained = now.checked_sub(*before.get(state)?)?;
        weighted += number(gained) * f64::from(*frequency);
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

    fn level_table(rows: &[(u64, u64, u64)]) -> Table {
        Table {
            columns: ["publication", "level", "state", "active_ns"]
                .map(String::from)
                .to_vec(),
            rows: rows
                .iter()
                .map(|&(level, state, active_ns)| {
                    [1, level, state, active_ns]
                        .map(|value| Some(value.to_string()))
                        .to_vec()
                })
                .collect(),
        }
    }

    fn tables(tables: Vec<Vec<u32>>) -> ClockTables {
        ClockTables::from_tables(tables)
    }

    #[test]
    fn level_ghz_weights_each_state_by_its_active_time() {
        // Goal: a level reads the mean of its state frequencies weighted by the time gained.
        // Method: the fast level gains 1 s at 1 GHz and 3 s at 3 GHz, the slow level 2 s at
        // 2 GHz alone, from tables of two and three states.
        let previous = level_table(&[(0, 0, 5), (0, 1, 5), (1, 0, 0), (1, 1, 0), (1, 2, 0)]);
        let current = level_table(&[
            (0, 0, 1_000_000_005),
            (0, 1, 3_000_000_005),
            (1, 0, 0),
            (1, 1, 2_000_000_000),
            (1, 2, 0),
        ]);
        let clock = tables(vec![
            vec![1_000_000, 3_000_000],
            vec![1_000_000, 2_000_000, 4_000_000],
        ]);

        let ghz = level_ghz_with(&current, &previous, &clock).unwrap();

        assert_eq!(ghz, [Some(2.5), Some(2.0)]);
    }

    #[test]
    fn level_ghz_reads_nothing_without_active_time_or_a_single_table() {
        // Goal: an idle level, and a level whose count of states no single table matches, read
        // nothing rather than a frequency taken from the wrong table.
        // Method: the fast level gains no time, and the slow level has three states against
        // two tables of three.
        let previous = level_table(&[(0, 0, 7), (0, 1, 7), (1, 0, 0), (1, 1, 0), (1, 2, 0)]);
        let current = level_table(&[(0, 0, 7), (0, 1, 7), (1, 0, 9), (1, 1, 9), (1, 2, 9)]);
        let clock = tables(vec![vec![1, 2], vec![1, 2, 3], vec![4, 5, 6]]);

        let ghz = level_ghz_with(&current, &previous, &clock).unwrap();

        assert_eq!(ghz, [None, None]);
    }

    #[test]
    fn level_ghz_reads_nothing_where_a_state_lost_time() {
        // Goal: a counter that went backward reads no frequency rather than a wrapped one.
        // Method: the fast level's second state falls from 9 to 8.
        let previous = level_table(&[(0, 0, 0), (0, 1, 9)]);
        let current = level_table(&[(0, 0, 5), (0, 1, 8)]);
        let clock = tables(vec![vec![1_000_000, 2_000_000]]);

        let ghz = level_ghz_with(&current, &previous, &clock).unwrap();

        assert_eq!(ghz, [None, None]);
    }

    #[test]
    fn core_level_states_query_sums_each_level_state() {
        // Goal: the query sums every core's time in a state into its level, the cores below
        // the slow count in level 1.
        // Method: two slow and one fast core over two states, summed by the query engine.
        let connection = data::connection();
        let frame = polars::df!(
            "publication" => [1_u64, 1, 1, 1, 1, 1],
            "core" => [0_u32, 0, 1, 1, 2, 2],
            "state" => [0_u32, 1, 0, 1, 0, 1],
            "active_ns" => [1_u64, 2, 3, 4, 5, 6],
        )
        .unwrap();
        connection.register("core_states", frame).unwrap();

        let table = data::query(
            &connection,
            &core_level_states_query("core_states", 2),
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
                ["level", "state", "active_ns"].map(|name| table.integer(row, name).unwrap())
            })
            .collect();
        assert_eq!(
            rows,
            [
                [Some(0), Some(0), Some(5)],
                [Some(0), Some(1), Some(6)],
                [Some(1), Some(0), Some(4)],
                [Some(1), Some(1), Some(6)],
            ]
        );
    }
}
