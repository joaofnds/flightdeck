//! Characterization of the common production and projected process file order.
use super::*;
use crate::owned_identity::{OwnedIdentity, Programs};
use polars::prelude::*;

#[test]
fn ordinary_process_projection_orders_reused_pids_and_publication_ties() {
    // Goal: projection preserves the production key across reused PIDs and equal starts.
    // Method: compare literal key columns and every two-row projection of three ticks.
    let window = process_order_window(false);
    let frame = crate::identity_encoder::processes_in_file_order(&window).unwrap();
    assert_eq!(
        frame
            .column("pid")
            .unwrap()
            .i32()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [2, 2, 2, 7, 7, 7]
    );
    assert_eq!(
        frame
            .column("publication")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [1, 2, 3, 2, 3, 1]
    );
    assert_eq!(
        process_order_starts(&frame),
        [Some(3), Some(3), Some(3), Some(10), Some(10), Some(20)]
    );
    process_order_projection(&window, &frame);
}

#[test]
fn ordinary_process_projection_places_unavailable_start_after_measured_starts() {
    // Goal: an unavailable process retains its null start and sorts after measured starts.
    // Method: compare literal keys and all projected columns with a refused-usage fixture.
    let window = process_order_window(true);
    let frame = crate::identity_encoder::processes_in_file_order(&window).unwrap();
    assert_eq!(
        frame
            .column("pid")
            .unwrap()
            .i32()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [2, 2, 2, 7, 7, 7]
    );
    assert_eq!(
        frame
            .column("publication")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [1, 2, 3, 2, 1, 3]
    );
    assert_eq!(
        process_order_starts(&frame),
        [Some(3), Some(3), Some(3), Some(10), Some(20), None]
    );
    assert_eq!(
        frame
            .column("usage_availability")
            .unwrap()
            .u8()
            .unwrap()
            .get(5),
        Some(crate::schema::availability::REFUSED)
    );
    process_order_projection(&window, &frame);
}

fn process_order_window(captured: bool) -> Window {
    let mut shape = encoder_fixture::profile(false);
    shape.process_capacity = 2;
    let mut tick = encoder_fixture::tick(&shape);
    tick.events = 1..1;
    tick.processes_refused = 0;
    tick.processes_gone = 0;
    let identity = WindowIdentity {
        run: RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        },
        window: 0,
    };
    let mut window = Window::new(identity, Limits::from_shape(&shape).unwrap());
    for publication in 1..=3 {
        tick.run_tick = publication - 1;
        tick.processes[0].pid = 2;
        tick.processes[0].usage.awake_ns_at_start = 3;
        tick.processes[1].pid = 7;
        tick.processes[1].usage.awake_ns_at_start = if publication == 1 { 20 } else { 10 };
        window
            .push_tick(
                &Observation {
                    publication,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                },
                &tick,
                &[],
            )
            .unwrap();
    }
    if captured {
        process_order_window_identities(&mut window);
    }
    window
}

fn process_order_window_identities(window: &mut Window) {
    Programs::initialize(&mut window.programs);
    for tick in &mut window.ticks {
        let identities = tick
            .processes
            .iter()
            .map(|process| OwnedIdentity {
                pid: process.pid,
                creation: None,
                availability: crate::schema::identity_availability::UNSUPPORTED,
                usage: crate::schema::availability::MEASURED,
                program: None,
            })
            .collect();
        tick.identities = Some(crate::window::IdentityRows {
            processes: identities,
            events: Vec::new(),
        });
    }
    let last = window.ticks.last_mut().unwrap();
    last.processes.pop().unwrap();
    last.processes_refused = 1;
    let identities = &mut last.identities.as_mut().unwrap().processes;
    identities.last_mut().unwrap().usage = crate::schema::availability::REFUSED;
}

fn process_order_projection(window: &Window, expected: &DataFrame) {
    assert_eq!(expected.height(), 6);
    for offset in (0..expected.height()).step_by(2) {
        let projected = crate::identity_encoder::processes_projection(window, offset, 2).unwrap();
        assert!(projected.equals_missing(&expected.slice(i64::try_from(offset).unwrap(), 2)));
    }
    let empty =
        crate::identity_encoder::processes_projection(window, expected.height(), 0).unwrap();
    assert!(empty.equals_missing(&expected.slice(6, 0)));
}

fn process_order_starts(frame: &DataFrame) -> Vec<Option<u64>> {
    assert_eq!(frame.height(), 6);
    let starts = frame.column("awake_ns_at_start").unwrap().u64().unwrap();
    (0..6).map(|position| starts.get(position)).collect()
}

#[test]
fn a_processes_table_of_several_row_groups_keeps_the_bytes_of_its_whole_frame() {
    // Goal: writing the processes table a row group at a time changes no byte of its file.
    // Method: write a window of three row groups as one frame and by row groups, and compare.
    let window = row_groups_window(crate::encoder::ROW_GROUP_ROWS + 1, false);

    assert_written_as_its_whole_frame(&window);
}

#[test]
fn a_processes_table_with_identities_keeps_the_bytes_of_its_whole_frame() {
    // Goal: the identity columns and a process whose usage was refused survive the same change.
    // Method: the same comparison on a window that names its programs and refuses one process.
    let window = row_groups_window(crate::encoder::ROW_GROUP_ROWS + 1, true);

    assert_written_as_its_whole_frame(&window);
}

#[test]
fn a_processes_table_with_no_process_keeps_the_bytes_of_its_whole_frame() {
    // Goal: a window that read no process still saves a processes table, with no row.
    // Method: the same comparison on a window whose ticks hold no process.
    let window = row_groups_window(0, false);

    assert_written_as_its_whole_frame(&window);
}

#[test]
fn a_processes_table_builds_one_frame_for_each_of_its_row_groups() {
    // Goal: no frame holds the rows of the whole window, which at 12,000 processes took more
    // memory than the persister may use. Method: count the frames built for three row groups.
    let window = row_groups_window(crate::encoder::ROW_GROUP_ROWS + 1, false);
    let observation = crate::work::observe();

    crate::encoder::encode_processes(&window, Vec::new()).unwrap();

    assert_eq!(observation.counts().frames, 3);
}

/// A window of two ticks, each with `processes` processes in pid order, so its table holds twice
/// that many rows.
fn row_groups_window(processes: usize, captured: bool) -> Window {
    let mut shape = encoder_fixture::profile(false);
    shape.process_capacity = u64::try_from(processes).unwrap();
    let mut tick = encoder_fixture::tick(&shape);
    tick.events = 1..1;
    tick.processes_refused = 0;
    tick.processes_gone = 0;
    let identity = WindowIdentity {
        run: RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        },
        window: 0,
    };
    let mut window = Window::new(identity, Limits::from_shape(&shape).unwrap());
    for publication in 1..=2 {
        tick.run_tick = publication - 1;
        window
            .push_tick(
                &Observation {
                    publication,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                },
                &tick,
                &[],
            )
            .unwrap();
    }
    if captured {
        process_order_window_identities(&mut window);
    }
    window
}

fn assert_written_as_its_whole_frame(window: &Window) {
    let mut frame = crate::identity_encoder::processes_in_file_order(window).unwrap();
    let mut whole = Vec::new();
    crate::encoder::encode_write(&mut frame, &mut whole).unwrap();
    let mut by_row_groups = Vec::new();

    let rows = crate::encoder::encode_processes(window, &mut by_row_groups).unwrap();

    assert_eq!(rows, collector::u64_from_usize(frame.height()));
    assert_eq!(by_row_groups, whole);
}
