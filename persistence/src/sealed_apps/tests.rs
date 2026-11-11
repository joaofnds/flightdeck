use std::path::PathBuf;

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
fn a_window_follows_only_a_tick_read_before_it_of_its_own_run() {
    // Goal: a derivation takes a window's first rates from what it read only where that is the
    // tick before, so another run's or boot's tick or a later one sends it to the saved minute.
    // Method: read a window of a run, and ask about that run's next window, about an earlier one,
    // about another run's and about another boot's.
    let mut state = state();
    let mut read_window = legacy_window(0);
    stage_legacy(&mut read_window, 5, &[process(3, 30)], &[]);
    read(&mut state, &read_window).unwrap();
    let staged_at = |publication| {
        let mut window = legacy_window(1);
        stage_legacy(&mut window, publication, &[process(3, 30)], &[]);
        window
    };
    let mut another_run = staged_at(6);
    another_run.identity.run.run = collector::ring::Identity([77, 78]);
    let mut another_boot = staged_at(6);
    another_boot.identity.run.boot = collector::ring::Identity([79, 80]);

    assert!(state.ticks.follows(&staged_at(6)));
    assert!(!state.ticks.follows(&staged_at(5)));
    assert!(!state.ticks.follows(&another_run));
    assert!(!state.ticks.follows(&another_boot));
    state.ticks.clear();
    assert!(!state.ticks.follows(&staged_at(6)));
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
fn a_full_index_refuses_one_window_and_not_the_next() {
    // Goal: a run that fills the index of programs loses the derived tables of one window and not
    // of every window after it.
    // Method: derive a window, fill the index, then derive two windows that name a program the
    // index has not met.
    let mut ring = Ring::new();
    let met = ring.intern(b"met", None);
    let unmet = ring.intern(b"unmet", None);
    ring.publish(
        0,
        &[process(1, 10)],
        &[identity(1, UsageAvailability::Measured, met)],
        &[],
        &[],
    );
    let mut first = ring.stage(0);
    let unmet_identity = [identity(1, UsageAvailability::Measured, unmet)];
    ring.publish(1, &[process(1, 11)], &unmet_identity, &[], &[]);
    let mut refused = ring.stage(1);
    ring.publish(2, &[process(1, 12)], &unmet_identity, &[], &[]);
    let mut next = ring.stage(2);
    let (pending, pending_next) = (Pending::new(), Pending::new());
    first.identity.run.recording = pending.1;
    refused.identity.run.recording = pending.1;
    next.identity.run.recording = pending.1;
    let apps = SealedApps::default();
    apps.derive(&derivation(&first, &pending.directory(), &pending.0))
        .unwrap();
    let mut filler = 0_u32;
    while apps
        .state
        .borrow_mut()
        .programs
        .intern(filler_program(&filler.to_le_bytes()))
        .is_some()
    {
        filler += 1;
    }

    let directory = pending_next.directory();
    let error = apps
        .derive(&derivation(&refused, &directory, &pending.0))
        .unwrap_err();
    let derived = apps.derive(&derivation(&next, &directory, &pending.0));

    assert!(matches!(error, Error::Bound("window program index")));
    assert_eq!(derived.unwrap().len(), 2);
}

fn filler_program(name: &[u8]) -> model::programs::Program<'_> {
    model::programs::Program {
        executable_name: name,
        app_path: None,
    }
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
fn holds_no_process_of_a_window_whose_processes_alone_overfill_the_parts() {
    // Goal: a window too large for the parts table costs the persister none of the memory its
    // parts would take, since a Mac that runs that many processes has the least to spare.
    // Method: read a window of two ticks of two processes with a table that keeps five rows,
    // which the first tick alone fits and the two together do not, and look at what it held.
    let mut window = legacy_window(0);
    stage_legacy(&mut window, 0, &[process(3, 30), process(4, 40)], &[]);
    stage_legacy(&mut window, 1, &[process(3, 31), process(4, 41)], &[]);
    let mut state = state();
    state.window.parts_rows_max = 5;

    read(&mut state, &window).unwrap();

    assert_eq!(state.window.processes.len(), 0);
    let rows = super::parts::encode_rows(&state.window, &state.programs);
    assert!(matches!(rows, Err(Error::Bound("sealed parts rows"))));
}

#[test]
fn holds_every_process_of_a_window_whose_rows_fill_the_parts_exactly() {
    // Goal: a window with as many rows as the parts table keeps still gets its parts.
    // Method: read a window of two ticks of two processes with a table that keeps six rows, a
    // row for each tick and each process, and look at what it held.
    let mut window = legacy_window(0);
    stage_legacy(&mut window, 0, &[process(3, 30), process(4, 40)], &[]);
    stage_legacy(&mut window, 1, &[process(3, 31), process(4, 41)], &[]);
    let mut state = state();
    state.window.parts_rows_max = 6;

    read(&mut state, &window).unwrap();

    assert_eq!(state.window.processes.len(), 4);
    let rows = super::parts::encode_rows(&state.window, &state.programs);
    assert_eq!(rows.unwrap().len(), 2);
}

#[test]
fn builds_some_rows_of_the_parts_as_the_whole_table_holds_them() {
    // Goal: the parts table is written a row group at a time, so a frame of some of its rows
    // holds what a frame of them all holds at those rows.
    // Method: read a tick of two Apps and their processes, and compare the frame of the last
    // three rows with those rows of the frame of all five.
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
    assert_eq!(rows.len(), 5);

    let some = super::parts::encode_frame(&rows[2..], &state.programs).unwrap();

    let whole = super::parts::encode_frame(&rows, &state.programs).unwrap();
    assert!(some.equals_missing(&whole.slice(2, 3)));
    assert!(!some.equals_missing(&whole.slice(1, 3)));
}

#[test]
fn derives_the_ranks_and_the_parts_of_a_window() {
    // Goal: a sealing window gets both App tables.
    // Method: derive a window into a pending directory and look at what it wrote.
    let mut window = legacy_window(0);
    stage_legacy(&mut window, 0, &[process(3, 30)], &[]);
    let pending = Pending::new();
    window.identity.run.recording = pending.1;

    let tables = SealedApps::default()
        .derive(&derivation(&window, &pending.directory(), &pending.0))
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
        &derivation(&window, &pending.directory(), &pending.0),
    )
    .unwrap();

    assert!(parts.is_none());
    assert!(!pending.0.join("app_parts.parquet").exists());
}

fn state() -> State {
    SealedApps::default().state.into_inner()
}

fn read(state: &mut State, window: &Window) -> crate::Result<()> {
    state.read(window)
}

fn derivation<'seal>(
    window: &'seal Window,
    pending: &'seal collector::recording::private::PrivateDirectory,
    root: &'seal std::path::Path,
) -> Derivation<'seal> {
    static BUDGET: std::sync::LazyLock<std::sync::Arc<crate::resources::ByteBudget>> =
        std::sync::LazyLock::new(|| {
            std::sync::Arc::new(crate::resources::ByteBudget::new(256 * 1024 * 1024))
        });

    Derivation {
        window,
        pending,
        saved: crate::worker::SavedRecording {
            root,
            budget: &BUDGET,
        },
    }
}

/// A pending directory of its own, removed with it, which is also a recording of this identity.
struct Pending(PathBuf, collector::ring::Identity);

impl Pending {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("sealed-apps-{}-{number}", std::process::id()));
        collector::recording::private::create_private_dir(&directory).unwrap();
        let root = collector::recording::private::PrivateDirectory::open(&directory).unwrap();
        let lock = collector::recording::private::FileLock::try_lock_in(&root, "registration.lock")
            .unwrap();
        let recording =
            collector::recording::bootstrap::initialize(&root, &lock, &mut [0; 4097]).unwrap();
        Self(directory, recording)
    }

    fn directory(&self) -> collector::recording::private::PrivateDirectory {
        collector::recording::private::PrivateDirectory::open(&self.0).unwrap()
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
