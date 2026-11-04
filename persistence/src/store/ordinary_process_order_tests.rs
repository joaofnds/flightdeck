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
    last.identities.as_mut().unwrap().processes[1].usage = crate::schema::availability::REFUSED;
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
