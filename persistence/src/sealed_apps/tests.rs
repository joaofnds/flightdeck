use std::path::{Path, PathBuf};

use collector::ring::UsageAvailability;
use model::ranks::{METRICS, Metric};

use super::{SealedApps, State, derive_parts};
use crate::app_tables::{PARTS_ROWS_MAX, ROW_APP, ROW_HEADER, ROW_PROCESS};
use crate::window_ticks::tests::{
    Ring, identity, legacy_window, observation, process, stage_legacy,
};
use crate::worker::{Derivation, Derive};
use crate::{Error, Window};

#[test]
fn keeps_each_tick_with_the_observation_its_rates_come_from() {
    // Goal: a tick's facts name the observation its rates come from, and a tick without one
    // has no rates.
    // Method: read a window of two ticks of one process and look at both ticks.
    let mut window = legacy_window(0);
    stage_legacy(&mut window, 0, &[process(3, 30)], &[]);
    stage_legacy(&mut window, 1, &[process(3, 31)], &[]);
    let mut state = state();

    read(&mut state, &window).unwrap();

    let ticks = &state.window.ticks;
    assert_eq!(ticks.len(), 2);
    assert_eq!((ticks[0].previous, ticks[0].elapsed_ns), (None, None));
    assert_eq!(ticks[1].previous, Some(observation(0)));
    assert_eq!(ticks[1].elapsed_ns, Some(1_000_000_000));
    assert_eq!(ticks[1].run_tick, 1);
    assert_eq!((ticks[1].process_count, ticks[1].app_count), (1, 0));
    assert_eq!(ticks[1].ungrouped_members, 1);
}

#[test]
fn keeps_a_row_for_each_ranking_that_ranks_no_app() {
    // Goal: a ranking of no App still says what it is of and why it ranks none.
    // Method: read a tick of a process outside every App and look at its rankings.
    let mut window = legacy_window(0);
    stage_legacy(&mut window, 0, &[process(3, 30)], &[]);
    let mut state = state();

    read(&mut state, &window).unwrap();

    let rankings = &state.window.rankings;
    let metrics: Vec<Metric> = rankings.iter().map(|row| row.metric).collect();
    assert_eq!(metrics, METRICS);
    assert!(rankings.iter().all(|row| row.position.is_none()));
    assert!(rankings.iter().all(|row| row.rank.is_none()));
}

#[test]
fn takes_no_rates_for_the_first_tick_after_a_refused_window() {
    // Goal: a refused window leaves no tick for the next window's first to take rates from.
    // Method: read a window whose second tick holds two processes of one pid, then a window
    // after it.
    let mut refused = legacy_window(0);
    stage_legacy(&mut refused, 0, &[process(3, 30)], &[]);
    stage_legacy(&mut refused, 1, &[process(3, 31), process(3, 32)], &[]);
    let mut next = legacy_window(1);
    stage_legacy(&mut next, 2, &[process(3, 33)], &[]);
    let mut state = state();

    let error = read(&mut state, &refused).unwrap_err();
    read(&mut state, &next).unwrap();

    assert!(matches!(error, Error::Bound("window tick pid order")));
    assert_eq!(state.window.ticks.len(), 1);
    assert_eq!(state.window.ticks[0].previous, None);
}

#[test]
fn ranks_and_parts_each_app_and_its_processes() {
    // Goal: the parts keep a tick's Apps in key order, and its processes in their Apps' order.
    // Method: publish a process of a bundle and one of a bare executable through a real ring,
    // then read and order the window's parts.
    let mut ring = Ring::new();
    let helper = ring.intern(b"helper", Some(b"/Apps/Outer.app"));
    let tool = ring.intern(b"tool", None);
    let identities = [
        identity(1, UsageAvailability::Measured, tool),
        identity(7, UsageAvailability::Measured, helper),
    ];
    ring.publish(0, &[process(1, 10), process(7, 70)], &identities, &[], &[]);
    let window = ring.stage(0);
    let mut state = state();

    read(&mut state, &window).unwrap();
    let rows = super::parts::encode_rows(&state.window, &state.programs).unwrap();

    let kinds: Vec<u8> = rows.iter().map(super::parts::PartRow::kind).collect();
    assert_eq!(
        kinds,
        [ROW_HEADER, ROW_APP, ROW_APP, ROW_PROCESS, ROW_PROCESS]
    );
    let pids: Vec<Option<i32>> = rows
        .iter()
        .map(|row| row.process().map(|(_index, process, _entry)| process.pid))
        .collect();
    assert_eq!(pids, [None, None, None, Some(7), Some(1)]);
    let tick = &state.window.ticks[0];
    assert_eq!((tick.app_count, tick.process_count), (2, 2));
    let resident = state
        .window
        .rankings
        .iter()
        .filter(|row| row.metric == Metric::Resident);
    assert_eq!(resident.filter(|row| row.rank.is_some()).count(), 2);
}

#[test]
fn refuses_the_parts_of_a_window_of_more_rows_than_the_table_keeps() {
    // Goal: a window too large for the parts table writes no parts.
    // Method: read a window, mark it a row over the table's bound, and order its parts.
    let mut window = legacy_window(0);
    stage_legacy(&mut window, 0, &[process(3, 30)], &[]);
    let mut state = state();
    read(&mut state, &window).unwrap();
    state.window.parts_rows = u64::from(PARTS_ROWS_MAX) + 1;

    let rows = super::parts::encode_rows(&state.window, &state.programs);

    assert!(matches!(rows, Err(Error::Bound("sealed parts rows"))));
}

#[test]
fn derives_the_ranks_and_the_parts_of_a_window() {
    // Goal: a sealing window gets both App tables.
    // Method: derive a window into a pending directory and look at what it wrote.
    let mut window = legacy_window(0);
    stage_legacy(&mut window, 0, &[process(3, 30)], &[]);
    let pending = Pending::new();

    let tables = SealedApps::default()
        .derive(&derivation(&window, &pending.0))
        .unwrap();

    let names: Vec<&str> = tables.iter().map(|table| table.name).collect();
    assert_eq!(names, ["app_ranks", "app_parts"]);
    assert!(pending.0.join("app_ranks.parquet").exists());
    assert!(pending.0.join("app_parts.parquet").exists());
}

#[test]
fn removes_the_parts_a_window_failed_to_write() {
    // Goal: a window whose parts failed seals with its ranks and no partial parts table.
    // Method: leave a parts file in a pending directory and hand the derivation a failure.
    let window = legacy_window(0);
    let pending = Pending::new();
    std::fs::write(pending.0.join("app_parts.parquet"), b"partial").unwrap();

    let parts = derive_parts(
        Err(Error::Bound("sealed parts rows")),
        &derivation(&window, &pending.0),
    )
    .unwrap();

    assert!(parts.is_none());
    assert!(!pending.0.join("app_parts.parquet").exists());
}

fn state() -> State {
    SealedApps::default().state.into_inner()
}

fn read(state: &mut State, window: &Window) -> crate::Result<()> {
    state.read(&derivation(window, Path::new("/nonexistent")))
}

fn derivation<'seal>(window: &'seal Window, pending: &'seal Path) -> Derivation<'seal> {
    Derivation { window, pending }
}

/// A pending directory of its own, removed with it.
struct Pending(PathBuf);

impl Pending {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("sealed-apps-{}-{number}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
