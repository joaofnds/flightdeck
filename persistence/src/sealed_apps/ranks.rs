//! The App ranks of each tick of a sealed window, so the viewer shows a saved second's top Apps
//! without replaying its minute.
use std::path::Path;

use polars::prelude::{Column, DataFrame};

use super::{RankingRow, WindowApps};
use crate::app_tables::{RANKS_COLUMNS, RANKS_VERSION, kind_code, metric_code};
use crate::encoder::columns;
use crate::schema::APP_RANKS;
use crate::window::WINDOW_TICKS_MAX;
use crate::{Result, TableManifest};
use model::programs::{AppKey, Programs};
use model::ranks::{METRICS, RANKS_MAX};

/// The five rankings of each tick a window holds, full.
const ROWS_MAX: usize = WINDOW_TICKS_MAX * METRICS.len() * RANKS_MAX;

const _: () = {
    assert!(RANKS_MAX < 256);
    assert!(ROWS_MAX < 4096);
};

/// Writes the ranks of the window's ticks into its pending directory, in the table the window
/// seals with.
pub(super) fn encode(
    window: &WindowApps,
    programs: &Programs,
    pending: &Path,
) -> Result<TableManifest> {
    let mut frame = encode_frame(window, programs)?;

    crate::encode_derived(APP_RANKS, &mut frame, pending)
}

/// A row for each App each ranking ranks, and one for each ranking that ranks none.
fn encode_frame(window: &WindowApps, programs: &Programs) -> Result<DataFrame> {
    let rows = &window.rankings;
    assert!(window.ticks.len() <= WINDOW_TICKS_MAX);
    assert!(rows.len() <= ROWS_MAX);

    let tick = |row: &RankingRow| &window.ticks[row.tick];
    let key = |row: &RankingRow| row.rank.map(|rank| programs.key(rank.app));
    let columns = columns!(rows;
        "version" => |_| RANKS_VERSION,
        "publication" => |row| tick(row).observation.publication,
        "run_tick" => |row| tick(row).run_tick,
        "wall_ns" => |row| tick(row).observation.wall_ns,
        "continuous_ns" => |row| tick(row).observation.continuous_ns,
        "awake_ns" => |row| tick(row).observation.awake_ns,
        "app_count" => |row| tick(row).app_count,
        // The App rule holds no tick's membership complete, which readers of this version
        // still take.
        "membership_complete" => |_| false,
        "ungrouped_members" => |row| tick(row).ungrouped_members,
        "metric" => |row| metric_code(row.metric),
        "unavailable" => |row| row.unavailable,
        "overflowed" => |row| row.overflowed,
        "partial" => |row| row.partial,
        "position" => |row| row.position,
        "app_kind" => |row| key(row).map(kind_code),
        "app_key" => |row| key(row).map(AppKey::bytes),
        "amount" => |row| row.rank.map(|rank| rank.amount),
        "rate" => |row| row.rank.and_then(|rank| rank.rate),
        "measured_members" => |row| row.rank.map(|rank| rank.measured_members),
        "unavailable_members" => |row| row.rank.map(|rank| rank.unavailable_members),
    );
    assert!(
        columns
            .iter()
            .map(|column| column.name().as_str())
            .eq(RANKS_COLUMNS)
    );

    Ok(DataFrame::new(rows.len(), columns)?)
}
