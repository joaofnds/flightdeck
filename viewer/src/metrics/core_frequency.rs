use std::collections::BTreeMap;

use persistence::{Error, Result};

use super::number;

/// The table that sums each performance level's time in each active clock state, the shape
/// the history and the live view both read core states in.
pub(crate) const CORE_LEVEL_STATES: &str = "core_level_states";

/// The mean clock of each performance level while its cores were active over the interval
/// from `previous` to `current`, in GHz, level 0 first. Each active state's clock is weighted
/// by the time the level's cores spent in it. A level with no active time, or with a state
/// whose clock is unknown, reads nothing.
pub(super) fn level_ghz_view(
    current: &crate::metric_view::View<'_>,
    previous: &crate::metric_view::View<'_>,
) -> Result<[Option<f64>; 2]> {
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
fn level_ghz_states(
    table: &crate::metric_view::View<'_>,
    level: u64,
) -> Result<BTreeMap<u64, (u64, Option<u64>)>> {
    let mut states = BTreeMap::new();
    for row in table.rows() {
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
    use collector::system::PerformanceLevel;

    use super::*;
    use crate::metric_view::View;
    use crate::recording::{CoreStateRow, MachineMinute, TickRow};
    use crate::store::reading_at;

    const PUBLICATION: u64 = 1;

    /// The readings of a tick with `slow_cpus` in the slower level, 1, whose cores come first.
    fn level_minute(slow_cpus: u32, rows: &[(u32, u32, u64, Option<u32>)]) -> MachineMinute {
        let level = PerformanceLevel {
            logical_cpus: slow_cpus,
            ..PerformanceLevel::default()
        };
        MachineMinute {
            ticks: vec![TickRow {
                performance_levels: Some([level; 2]),
                ..TickRow::fixture(PUBLICATION)
            }],
            core_states: rows
                .iter()
                .map(|&(core, state, active_ns, frequency_khz)| CoreStateRow {
                    publication: PUBLICATION,
                    core,
                    state,
                    active_ns,
                    frequency_khz,
                })
                .collect(),
            ..MachineMinute::default()
        }
    }

    /// `level_minute`'s one fast and one slow core, of rows of level, state, active time and
    /// clock in kHz.
    fn level_rows(rows: &[(u32, u32, u64, Option<u32>)]) -> MachineMinute {
        let rows: Vec<_> = rows
            .iter()
            .map(|&(level, state, active_ns, frequency_khz)| {
                (1 - level, state, active_ns, frequency_khz)
            })
            .collect();

        level_minute(1, &rows)
    }

    fn level_ghz(current: &MachineMinute, previous: &MachineMinute) -> Result<[Option<f64>; 2]> {
        let current = reading_at(current, PUBLICATION).unwrap();
        let previous = reading_at(previous, PUBLICATION).unwrap();

        level_ghz_view(
            &View(crate::live_raw::View::machine(&current, CORE_LEVEL_STATES)?),
            &View(crate::live_raw::View::machine(
                &previous,
                CORE_LEVEL_STATES,
            )?),
        )
    }

    #[test]
    fn level_ghz_weights_each_state_by_its_active_time() {
        // Goal: a level reads the mean of its state clocks weighted by the time gained.
        // Method: the fast level gains 1 s at 1 GHz and 3 s at 3 GHz, the slow level 2 s at
        // 2 GHz alone.
        let ghz = |ghz: u32| Some(ghz * 1_000_000);
        let previous = level_rows(&[
            (0, 0, 5, ghz(1)),
            (0, 1, 5, ghz(3)),
            (1, 0, 0, ghz(1)),
            (1, 1, 0, ghz(2)),
            (1, 2, 0, ghz(4)),
        ]);
        let current = level_rows(&[
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
        let previous = level_rows(&[(0, 0, 7, Some(1)), (0, 1, 7, Some(2)), (1, 0, 0, None)]);
        let current = level_rows(&[(0, 0, 7, Some(1)), (0, 1, 7, Some(2)), (1, 0, 9, None)]);

        let levels = level_ghz(&current, &previous).unwrap();

        assert_eq!(levels, [None, None]);
    }

    #[test]
    fn level_ghz_reads_nothing_where_a_state_lost_time() {
        // Goal: a counter that went backward reads no clock rather than a wrapped one.
        // Method: the fast level's second state falls from 9 to 8.
        let previous = level_rows(&[(0, 0, 0, Some(1_000_000)), (0, 1, 9, Some(2_000_000))]);
        let current = level_rows(&[(0, 0, 5, Some(1_000_000)), (0, 1, 8, Some(2_000_000))]);

        let levels = level_ghz(&current, &previous).unwrap();

        assert_eq!(levels, [None, None]);
    }

    #[test]
    fn core_level_states_sum_each_level_state_by_its_ticks_levels() {
        // Goal: the view sums every core's time in a state into its level, the cores below the
        // slower level's count in level 1, and keeps the clock the level's cores share.
        // Method: two slow and one fast core over two states, where the slow cores record
        // different clocks for state 1.
        let minute = level_minute(
            2,
            &[
                (0, 0, 1, Some(10)),
                (0, 1, 2, Some(20)),
                (1, 0, 3, Some(10)),
                (1, 1, 4, Some(21)),
                (2, 0, 5, None),
                (2, 1, 6, Some(30)),
            ],
        );
        let reading = reading_at(&minute, PUBLICATION).unwrap();

        let view = View(crate::live_raw::View::machine(&reading, CORE_LEVEL_STATES).unwrap());

        let rows: Vec<_> = view
            .rows()
            .map(|row| {
                ["level", "state", "active_ns", "frequency_khz"]
                    .map(|name| view.integer(row, name).unwrap())
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

    #[test]
    fn core_level_states_leave_out_a_tick_that_recorded_no_levels() {
        // Goal: a tick with no levels places no core in one.
        // Method: read the view of a tick holding a core state and no levels.
        let mut minute = level_minute(2, &[(0, 0, 7, Some(10))]);
        minute.ticks[0].performance_levels = None;
        let reading = reading_at(&minute, PUBLICATION).unwrap();

        let view = crate::live_raw::View::machine(&reading, CORE_LEVEL_STATES).unwrap();

        assert_eq!(view.len(), 0);
    }
}
