use collector::ring::{Identity, RunIdentity, Shape, SystemSnapshot};

use crate::schema::Table;
use crate::{Limits, Observation, Window, WindowIdentity};

const SHAPE: Shape = Shape {
    slot_count: 2,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 2,
    fan_capacity: 1,
    process_capacity: 1,
    coalition_capacity: 1,
    event_capacity: 1,
};

#[test]
fn saves_a_core_states_clock_only_where_its_tick_knew_it() {
    // Goal: a state's clock reads back where the tick published it, and NULL where the clock
    // was unknown or the tick published no system readings.
    // Method: stage a tick with one known and one unknown clock and a tick with no system
    // readings that still holds clocks, and encode the core states.
    let mut window = window();
    stage(
        &mut window,
        1,
        Some(&SystemSnapshot::default()),
        vec![0, 4_608_000],
    );
    stage(&mut window, 2, None, vec![1_000, 2_000]);

    let frame = super::cores(&window).unwrap();

    let column = frame.column("frequency_khz").unwrap().u32().unwrap();
    let clocks: Vec<Option<u32>> = column.iter().collect();
    assert_eq!(clocks, [None, Some(4_608_000), None, None]);
}

#[test]
fn declares_the_core_state_clock_it_encodes() {
    // Goal: a staged table and a sealed file of one window hold the same core state columns.
    // Method: compare the declared columns with the encoded frame's for a window with clocks.
    let mut window = window();
    stage(
        &mut window,
        1,
        Some(&SystemSnapshot::default()),
        vec![0, 4_608_000],
    );

    let declared = crate::table_columns(Table::CoreStates, &window);
    let frame = super::cores(&window).unwrap();

    let names: Vec<&str> = declared
        .split(", ")
        .map(|column| column.split(' ').next().unwrap())
        .collect();
    let encoded: Vec<&str> = frame
        .get_column_names()
        .into_iter()
        .map(polars::prelude::PlSmallStr::as_str)
        .collect();
    assert_eq!(names, encoded);
}

/// Stages a tick whose one core spent time in two states at the clocks `khz`.
fn stage(window: &mut Window, publication: u64, system: Option<&SystemSnapshot>, khz: Vec<u32>) {
    let tick = crate::tick_buffer(&SHAPE).unwrap();
    let observation = Observation {
        publication,
        wall_ns: None,
        continuous_ns: None,
        awake_ns: None,
    };
    window.push_tick(&observation, &tick, &[]).unwrap();

    let staged = window.ticks.last_mut().unwrap();
    staged.ioreport_read = true;
    staged.cores = vec![vec![10, 20]];
    staged.core_state_khz = vec![khz];
    staged.system = system.copied();
}

fn window() -> Window {
    Window::new(
        WindowIdentity {
            run: RunIdentity {
                recording: Identity([1, 2]),
                run: Identity([3, 4]),
                boot: Identity([5, 6]),
            },
            window: 0,
        },
        Limits::from_shape(&SHAPE).unwrap(),
    )
}
