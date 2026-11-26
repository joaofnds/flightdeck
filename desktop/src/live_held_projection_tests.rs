use std::sync::Arc;

use collector::battery::BatteryReading;
use collector::machine::Pressure;
use collector::recording::identity_text;
use collector::ring::{Identity, RunIdentity};
use viewer::store::{AppsSecond, AppsStretch, MachineSecond, MachineStretch, View};

use super::super::{JoinedPoints, RecentPoints, empty_panels, overview, panels};
use super::{Observations, ViewPanelsPoints, overview_point_second, view_panels};
use crate::data::Panel;
use crate::live_projection::{
    AREA_MAXIMA_ID, CPU_TEMPERATURE_ID, FANS_ID, FREQUENCY_ID, GPU_FREQUENCY_ID, GPU_MEMORY_ID,
    GPU_TEMPERATURE_ID, LOAD_AVERAGE_ID, OPEN_FILES_ID, THERMAL_STATE_ID,
};
use crate::live_store_fixtures::{
    battery_reading, cpu_readings, fan_rpm, gpu_memory_bytes, gpu_watts, levels, memory_rates,
    network_rates, open_files_percent, overview_panels, reading_at, selection, selection_of_json,
    storage_rates, summary_point, summary_selection, trend_point, view, with_battery_reading,
    with_battery_readings, with_cpu_reading, with_cpu_readings, with_energy_reading,
    with_energy_readings, with_gpu_reading, with_gpu_readings, with_memory_rates,
    with_memory_reading, with_network_rates, with_network_reading, with_storage_rates,
    with_storage_reading,
};

/// `place` as a trend point's reach names a reading.
fn reach_place(place: &crate::data::MachinePlace) -> serde_json::Value {
    serde_json::json!([
        identity_text(Identity(place.run)).to_string(),
        identity_text(Identity(place.boot)).to_string(),
        place.publication,
        place.wall_ns,
    ])
}

const RUN: RunIdentity = RunIdentity {
    recording: Identity([1, 1]),
    run: Identity([2, 2]),
    boot: Identity([3, 3]),
};

/// What the store reduces `seconds` to, all of one stretch.
fn stretch_of(stretch: u64, seconds: &[MachineSecond]) -> Arc<AppsStretch> {
    let mut held = AppsStretch::of_seconds(stretch, &[Arc::new(AppsSecond::default())]);
    let mut machine = MachineStretch::default();
    for second in seconds {
        machine.add(&RUN, second);
    }
    *held.machine = machine;

    Arc::new(held)
}

/// The ten seconds of the stretch from 9,020, each read by `read`.
fn older_seconds(read: impl Fn(&mut MachineSecond, u64)) -> Vec<MachineSecond> {
    (9_020..9_030)
        .map(|wall_second| {
            let mut second = reading_at(wall_second, 3);
            read(&mut second, wall_second);
            second
        })
        .collect()
}

/// The Overview of the store that holds `older` and a second past the charts' full-detail seconds,
/// and what it reduced them to.
fn overview_of_older(older: &[MachineSecond]) -> Vec<std::rc::Rc<Panel>> {
    let newest = reading_at(10_000, 3);
    let mut view: View = view(1, older, &[newest]);
    view.stretches = vec![stretch_of(902, older)];

    overview_panels(&view, levels().as_ref()).unwrap()
}

#[test]
fn a_stretch_older_than_every_point_of_a_chart_costs_it_none_of_its_later_stretches() {
    // Goal: a store holds stretches further back than a chart draws, which it passes over.
    // Method: an Overview of older seconds, with and without a stretch none of its seconds lies
    // in, drawn both ways.
    let older = older_seconds(|second, wall_second| {
        second.cpu_percent = Some(if wall_second == 9_024 { 95.0 } else { 10.0 });
    });
    let expected = overview_of_older(&older);
    let mut view: View = view(1, &older, &[reading_at(10_000, 3)]);
    view.stretches = vec![stretch_of(1, &[reading_at(10, 3)]), stretch_of(902, &older)];

    let drawn = overview_panels(&view, levels().as_ref()).unwrap();

    assert_eq!(drawn, expected);
}

#[test]
fn the_cpu_sensor_chart_of_a_view_draws_the_sensor_each_second_holds() {
    // Goal: the CPU page's sensor chart, drawn from the seconds the store holds, draws the
    // sensor a second's record holds and a gap where it holds none, so the chart needs no
    // complete reading. Method: two seconds of the live span, one with the sensor and one
    // without, each read by the chart's one line.
    let mut read = reading_at(9_999, 4);
    read.cpu_celsius = Some(61.5);
    let mut unread = reading_at(10_000, 4);
    unread.cpu_celsius = None;
    let seconds = [&read, &unread];
    let points = ViewPanelsPoints {
        seconds: &seconds,
        stretches: &[None, None],
    };
    let sensor = super::super::cpu_temperature();

    let drawn: Result<Vec<_>, _> = (0..seconds.len())
        .map(|index| points.value(&sensor.lines[0], index))
        .collect();

    assert_eq!(drawn, Ok(vec![Some(61.5), None]));
}

#[test]
fn the_cores_line_counts_the_logical_cpus_of_a_second_whose_record_names_no_levels() {
    // Goal: the Cores line of a View counts the logical CPUs a second's reading holds, as the
    // reader counts them, rather than the levels the record names. Method: seconds whose complete
    // readings hold eighteen logical CPUs and whose records name no levels, drawn from the View
    // and from their trend points, each Cores line read at its newest second.
    let seconds: Vec<_> = [9_999, 10_000]
        .into_iter()
        .map(|wall_second| {
            let mut second = with_cpu_reading(reading_at(wall_second, 4));
            second.performance_levels = None;
            second
        })
        .collect();
    let newest_cores = |panels: &[std::rc::Rc<Panel>]| {
        panel_named(panels, LOAD_AVERAGE_ID).lines[3]
            .values
            .last()
            .copied()
            .flatten()
    };

    let drawn = view_panels("cpu", &view(1, &[], &seconds), None).unwrap();
    let read = panels("cpu", &selection_of_json(&selection(&[], &seconds)), None).unwrap();

    assert_eq!(
        (newest_cores(&drawn), newest_cores(&read)),
        (Some(18.0), Some(18.0))
    );
}

/// The panel the Overview names `id`.
pub(crate) fn panel_named<'panels>(
    panels: &'panels [std::rc::Rc<Panel>],
    id: &str,
) -> &'panels Panel {
    panels.iter().find(|panel| panel.id == id).unwrap()
}

#[test]
fn a_view_draws_the_overview_the_trend_points_of_its_seconds_draw() {
    // Goal: the Overview draws the same charts from the seconds the store holds as from the
    // trend points of those seconds. Method: older seconds, live seconds older than the
    // full-detail ones, and full-detail seconds across a break, some readings missing, drawn
    // both ways.
    let earlier = [reading_at(6_345, 0), reading_at(7_001, 2)];
    let older = [reading_at(9_020, 3), reading_at(9_030, 3)];
    let mut detail = [
        reading_at(9_041, 3),
        reading_at(9_042, 3),
        reading_at(9_500, 4),
        reading_at(10_000, 4),
    ];
    detail[1].cpu_percent = None;
    detail[1].memory = None;
    detail[2].network = None;
    detail[2].thermal_pressure = None;
    let view = view(1, &earlier, &[older.as_slice(), &detail].concat());
    let expected = panels(
        "overview",
        &crate::live_store_fixtures::selection_of_json(&selection(
            &[earlier, older].concat(),
            &detail,
        )),
        None,
    );

    let drawn = overview_panels(&view, levels().as_ref());

    assert_eq!(drawn.unwrap(), expected.unwrap());
}

/// What a page drawn from the View draws, for the tests every such page runs alike.
struct Page {
    /// The page's route, which names it to `view_panels` and to the worker.
    route: &'static str,
    /// Gives a second the complete reading the page draws.
    with_reading: fn(MachineSecond) -> MachineSecond,
    /// How many charts the page draws.
    chart_count: usize,
    /// The lines whose readings the collector records nowhere, by chart and line name.
    unrecorded: &'static [(&'static str, &'static str)],
    /// An hour of the page's complete readings, with peaks at 7,013 and 9,040.
    hour: fn() -> Vec<MachineSecond>,
    /// The lines whose reaches the hour's peaks reach, by chart and line index.
    peak_lines: &'static [(&'static str, usize)],
    /// The lines that draw what a refused second holds itself, by chart and line index.
    held_at_a_refused_second: &'static [(&'static str, usize)],
    /// The lines that only a complete reading holds, by chart and line index.
    missing_at_a_refused_second: &'static [(&'static str, usize)],
    /// The state charts whose first line draws a state at a refused second, with that state, or
    /// none where the chart leaves the second's cell blank.
    states_at_a_refused_second: &'static [(&'static str, Option<f32>)],
}

const MEMORY: Page = Page {
    route: "memory",
    with_reading: with_memory_reading,
    chart_count: 6,
    unrecorded: &[
        ("live-swap-flow", "Paged out"),
        ("live-memory-temperature", "Memory"),
    ],
    hour: memory_hour,
    peak_lines: &[("live-swap", 0), ("live-swap-flow", 0), ("live-paging", 0)],
    held_at_a_refused_second: &[
        ("live-memory", 0),
        ("live-memory", 1),
        ("live-memory", 2),
        ("live-memory", 3),
        ("live-swap", 0),
        ("live-swap", 1),
    ],
    missing_at_a_refused_second: &[
        ("live-swap-flow", 0),
        ("live-swap-flow", 2),
        ("live-paging", 0),
        ("live-paging", 1),
        ("live-paging", 2),
    ],
    states_at_a_refused_second: &[("live-memory-pressure", Some(0.0))],
};

const NETWORK: Page = Page {
    route: "network",
    with_reading: with_network_reading,
    chart_count: 4,
    unrecorded: &[("live-network-apps", "All traffic")],
    hour: network_hour,
    peak_lines: &[
        ("live-network", 0),
        ("live-network", 1),
        ("live-packets", 0),
        ("live-packets", 1),
        ("live-network-errors", 0),
        ("live-network-errors", 1),
    ],
    held_at_a_refused_second: &[("live-network", 0), ("live-network", 1)],
    missing_at_a_refused_second: &[
        ("live-packets", 0),
        ("live-packets", 1),
        ("live-network-errors", 0),
        ("live-network-errors", 1),
    ],
    states_at_a_refused_second: &[],
};

const STORAGE: Page = Page {
    route: "storage",
    with_reading: with_storage_reading,
    chart_count: 5,
    unrecorded: &[(crate::live_projection::STORAGE_TEMPERATURE_ID, "Storage")],
    hour: storage_hour,
    peak_lines: &[
        ("live-storage-throughput", 0),
        ("live-storage-throughput", 1),
        ("live-storage-operations", 0),
        ("live-storage-operations", 1),
        ("live-storage-mean", 0),
        ("live-storage-mean", 1),
        ("live-storage-errors", 0),
        ("live-storage-errors", 1),
    ],
    held_at_a_refused_second: &[
        ("live-storage-throughput", 0),
        ("live-storage-throughput", 1),
    ],
    missing_at_a_refused_second: &[
        ("live-storage-operations", 0),
        ("live-storage-operations", 1),
        ("live-storage-mean", 0),
        ("live-storage-mean", 1),
        ("live-storage-errors", 0),
        ("live-storage-errors", 1),
    ],
    states_at_a_refused_second: &[],
};

const GPU: Page = Page {
    route: "gpu",
    with_reading: with_gpu_reading,
    chart_count: 5,
    unrecorded: &[(GPU_FREQUENCY_ID, "Frequency")],
    hour: gpu_hour,
    peak_lines: &[
        ("live-gpu-activity", 0),
        ("live-gpu-power", 0),
        (GPU_MEMORY_ID, 0),
        (GPU_MEMORY_ID, 1),
        (GPU_TEMPERATURE_ID, 0),
    ],
    held_at_a_refused_second: &[("live-gpu-activity", 0), (GPU_TEMPERATURE_ID, 0)],
    missing_at_a_refused_second: &[
        ("live-gpu-power", 0),
        (GPU_MEMORY_ID, 0),
        (GPU_MEMORY_ID, 1),
    ],
    states_at_a_refused_second: &[],
};

const CPU: Page = Page {
    route: "cpu",
    with_reading: with_cpu_reading,
    chart_count: 5,
    unrecorded: &[],
    hour: cpu_hour,
    peak_lines: &[
        ("live-cpu", 0),
        (FREQUENCY_ID, 0),
        (FREQUENCY_ID, 1),
        (LOAD_AVERAGE_ID, 0),
        (LOAD_AVERAGE_ID, 1),
        (LOAD_AVERAGE_ID, 2),
        (OPEN_FILES_ID, 0),
        (OPEN_FILES_ID, 1),
        (CPU_TEMPERATURE_ID, 0),
    ],
    held_at_a_refused_second: &[
        ("live-cpu", 0),
        ("live-cpu", 1),
        ("live-cpu", 2),
        (LOAD_AVERAGE_ID, 3),
        (CPU_TEMPERATURE_ID, 0),
    ],
    missing_at_a_refused_second: &[
        (FREQUENCY_ID, 0),
        (FREQUENCY_ID, 1),
        (LOAD_AVERAGE_ID, 0),
        (LOAD_AVERAGE_ID, 1),
        (LOAD_AVERAGE_ID, 2),
        (OPEN_FILES_ID, 0),
        (OPEN_FILES_ID, 1),
    ],
    states_at_a_refused_second: &[],
};

const ENERGY: Page = Page {
    route: "energy",
    with_reading: with_energy_reading,
    chart_count: 7,
    unrecorded: &[
        (AREA_MAXIMA_ID, "Source SoC group max"),
        (AREA_MAXIMA_ID, "Voltage regulators"),
        (AREA_MAXIMA_ID, "Display"),
        (AREA_MAXIMA_ID, "Board"),
        (AREA_MAXIMA_ID, "Wireless"),
        (AREA_MAXIMA_ID, "Ambient"),
    ],
    hour: energy_hour,
    peak_lines: &[
        ("live-energy", 0),
        ("live-gpu-power", 0),
        ("live-power-breakdown", 1),
        ("live-soc-temperature", 0),
        (FANS_ID, 0),
        (FANS_ID, 1),
        (FANS_ID, 2),
    ],
    held_at_a_refused_second: &[("live-energy", 0), ("live-soc-temperature", 0)],
    missing_at_a_refused_second: &[
        ("live-gpu-power", 0),
        ("live-power-breakdown", 0),
        ("live-power-breakdown", 1),
        (FANS_ID, 0),
        (FANS_ID, 1),
        (FANS_ID, 2),
    ],
    // The refused second reads level 4, Sleeping, which the thermal chart draws as Critical.
    states_at_a_refused_second: &[(THERMAL_STATE_ID, Some(3.0))],
};

const BATTERY: Page = Page {
    route: "battery",
    with_reading: with_battery_reading,
    chart_count: 3,
    unrecorded: &[],
    hour: battery_hour,
    peak_lines: &[("live-battery-charge", 0), ("live-battery-flow", 0)],
    held_at_a_refused_second: &[("live-battery-charge", 0), ("live-battery-flow", 0)],
    missing_at_a_refused_second: &[],
    // Only a complete reading holds the charging state, so the chart leaves the cell blank.
    states_at_a_refused_second: &[("live-battery-charging", None)],
};

macro_rules! view_drawn_as_the_readings_draw_it_cases {
    ($($name:ident, $page:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: the page draws the same charts from the seconds the store holds as from the
            // trend points the viewer writes of their complete readings. Method: seconds whose
            // complete readings hold every reading the page draws, each with its own value,
            // across older seconds and a break, drawn both ways, with every line but those
            // recorded nowhere drawing a value.
            assert_view_drawn_as_the_readings_draw_it(&$page);
        }
    )*};
}

view_drawn_as_the_readings_draw_it_cases! {
    // Memory: every line draws a value but the two recorded nowhere, Paged out and the memory
    // temperature.
    the_memory_charts_of_the_view_draw_as_the_readings_draw_them, MEMORY;
    // Network: every line draws a value but the one recorded nowhere, the Apps' All traffic.
    the_network_charts_of_the_view_draw_as_the_readings_draw_them, NETWORK;
    // Storage: every line draws a value but the one recorded nowhere, the storage temperature.
    the_storage_charts_of_the_view_draw_as_the_readings_draw_them, STORAGE;
    // GPU: every line draws a value but the one recorded nowhere, the GPU's frequency.
    the_gpu_charts_of_the_view_draw_as_the_readings_draw_them, GPU;
    // CPU: every line draws a value, the Cores line too, which the readings' levels give.
    the_cpu_charts_of_the_view_draw_as_the_readings_draw_them, CPU;
    // Energy: every line draws a value, each of three fan channels too, but the six area maxima
    // recorded nowhere.
    the_energy_charts_of_the_view_draw_as_the_readings_draw_them, ENERGY;
    // Battery: every line draws a value, the charging state too, which turns from not charging
    // to charging past 9,000.
    the_battery_charts_of_the_view_draw_as_the_readings_draw_them, BATTERY;
}

/// Asserts that `page` draws the same charts from a View as from the trend points of the same
/// complete readings.
fn assert_view_drawn_as_the_readings_draw_it(page: &Page) {
    assert_seconds_drawn_as_the_readings_draw_them(page, page.with_reading, page.unrecorded);
}

/// Asserts that `page` draws the same charts from a View as from the trend points of the seconds
/// `with_reading` gives their complete readings, with every line drawing a value but the
/// `undrawn` lines, which draw none.
fn assert_seconds_drawn_as_the_readings_draw_them(
    page: &Page,
    with_reading: fn(MachineSecond) -> MachineSecond,
    undrawn: &[(&str, &str)],
) {
    let read = |wall_second, segment| with_reading(reading_at(wall_second, segment));
    let earlier = [read(6_345, 0), read(7_001, 2)];
    let older = [read(9_020, 3), read(9_030, 3)];
    let detail = [
        read(9_041, 3),
        read(9_042, 3),
        read(9_500, 4),
        read(10_000, 4),
    ];
    let view = view(1, &earlier, &[older.as_slice(), &detail].concat());
    let expected = panels(
        page.route,
        &selection_of_json(&summary_selection(&[earlier, older].concat(), &detail)),
        None,
    );

    let drawn = view_panels(page.route, &view, levels().as_ref());

    let drawn = without_own_reaches(drawn.unwrap());
    assert_eq!(drawn, without_own_reaches(expected.unwrap()));
    assert_eq!(drawn.len(), page.chart_count);
    assert_lines_drawn(&drawn, undrawn);
}

macro_rules! battery_drawn_as_the_readings_draw_it_cases {
    ($($name:ident, $with_reading:expr, $undrawn:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: the Battery page draws the same charts from the seconds the store holds as
            // from the trend points the viewer writes of their complete readings, where the
            // battery leaves readings out. Method: seconds whose complete readings hold the
            // battery the case gives, across older seconds and a break, drawn both ways, with
            // every line drawing a value but the case's undrawn lines.
            assert_seconds_drawn_as_the_readings_draw_them(&BATTERY, $with_reading, $undrawn);
        }
    )*};
}

battery_drawn_as_the_readings_draw_it_cases! {
    // No battery: a Mac without one draws no charge, power flow or charging state.
    a_mac_without_a_battery_draws_its_battery_charts_without_a_reading_from_the_view,
        |second| with_battery_readings(second, None),
        &[
            ("live-battery-charge", "Charge"),
            ("live-battery-flow", "Into battery"),
            ("live-battery-charging", "Charging"),
        ];
    // Charging unread: the controller did not say whether it charged, so the charging chart
    // draws no cell, while the charge and power flow draw as they read.
    a_battery_whose_charging_is_unread_draws_no_charging_state_from_the_view,
        |second| {
            let mut battery = battery_reading(&second);
            battery.charging = None;
            with_battery_readings(second, Some(battery))
        },
        &[("live-battery-charging", "Charging")];
}

/// `panels` with each reach that names its point's own reading, the same high at the same place,
/// left out, so that two sources compare by what they draw. The reader's older tail leaves out
/// the reach of a number that held one value over its stretch, while the View's stretch series
/// keep it, and that reach draws nothing different: `chart_data_highest` takes it over the
/// point's own reading only through `chart_data_higher`, which keeps a reach that ties, with the
/// same value and place, and `timeline_cells` names a reach only where its state differs from
/// the point's. A stacked line's reach is its stack's and not its own, so it stays.
fn without_own_reaches(panels: Vec<std::rc::Rc<Panel>>) -> Vec<std::rc::Rc<Panel>> {
    panels
        .into_iter()
        .map(|panel| {
            let mut panel = std::rc::Rc::unwrap_or_clone(panel);
            let stacked: Vec<bool> = panel
                .lines
                .iter()
                .map(|line| crate::charts::stacked(&panel, line))
                .collect();
            let places = panel.places.clone();
            for (line, stacked) in panel.lines.iter_mut().zip(stacked) {
                if stacked {
                    continue;
                }
                let Some(extremes) = line.extremes.as_mut() else {
                    continue;
                };
                for (index, reach) in extremes.iter_mut().enumerate() {
                    let place = places.as_ref().and_then(|places| places.get(index));
                    let own = reach.is_some_and(|reach| {
                        Some(reach.high) == line.values[index] && Some(&reach.peak) == place
                    });
                    if own {
                        *reach = None;
                    }
                }
            }
            std::rc::Rc::new(panel)
        })
        .collect()
}

/// Asserts that every line of `panels` draws a value, except the `unrecorded` lines, which draw
/// none.
fn assert_lines_drawn(panels: &[std::rc::Rc<Panel>], unrecorded: &[(&str, &str)]) {
    for panel in panels {
        for line in &panel.lines {
            let recorded_nowhere = unrecorded.contains(&(panel.id.as_str(), line.name.as_str()));
            let drawn = line.values.iter().filter(|value| value.is_some()).count();
            if recorded_nowhere {
                assert_eq!(drawn, 0, "{} {}", panel.id, line.name);
            } else {
                assert!(drawn > 0, "{} {}", panel.id, line.name);
            }
        }
    }
}

#[test]
fn the_fan_chart_of_the_view_draws_every_channel_the_readings_draw() {
    // Goal: the Energy page's fan chart drawn from the View draws as many channels as the trend
    // points of the same complete readings draw, past the two a second's record holds. Method:
    // seconds whose complete readings hold three fan channels, drawn from a View and from their
    // trend points, and the fan chart of each.
    let seconds =
        [9_998, 9_999, 10_000].map(|wall_second| with_energy_reading(reading_at(wall_second, 3)));
    let view = view(1, &[], &seconds);
    let expected = panels(
        "energy",
        &selection_of_json(&summary_selection(&[], &seconds)),
        None,
    )
    .unwrap();

    let drawn = view_panels("energy", &view, levels().as_ref()).unwrap();

    let fans = panel_named(&drawn, FANS_ID);
    assert_eq!(fans, panel_named(&expected, FANS_ID));
    assert_eq!(fans.lines.len(), 3);
}

#[test]
fn the_fan_chart_of_the_view_draws_uneven_and_unread_channels_as_the_readings_draw_them() {
    // Goal: the View's fan chart draws a stopped fan as zero, an unread channel and a second
    // with no fans as gaps, and as many channels as the longest reading holds, as the trend
    // points of the same complete readings draw them. Method: a second whose fans read a stopped
    // zero and an unread channel, a second whose complete reading holds no fans, and a second
    // with one fan, drawn from a View and from their trend points.
    let seconds = [
        with_energy_readings(reading_at(9_998, 3), None, vec![Some(0.0), None]),
        with_gpu_reading(reading_at(9_999, 3)),
        with_energy_readings(reading_at(10_000, 3), None, vec![Some(1_200.0)]),
    ];
    let view = view(1, &[], &seconds);
    let expected = panels(
        "energy",
        &selection_of_json(&summary_selection(&[], &seconds)),
        None,
    )
    .unwrap();

    let drawn = view_panels("energy", &view, levels().as_ref()).unwrap();

    let fans = panel_named(&drawn, FANS_ID);
    assert_eq!(fans, panel_named(&expected, FANS_ID));
    assert_eq!(fans.lines.len(), 2);
}

#[test]
fn the_power_breakdown_of_the_view_draws_the_remainders_the_readings_draw() {
    // Goal: the View's power breakdown draws a measured zero and a negative remainder as they
    // are, and no remainder where either power is unread, as the trend points of the same
    // readings draw it. Method: seconds whose system and GPU power are 8 and 2, 0 and 0, 2 and
    // 5, 4 and unread, and unread and 2, drawn from a View and from their trend points, and the
    // power breakdown of each.
    let powers = [
        (Some(8.0), Some(2.0)),
        (Some(0.0), Some(0.0)),
        (Some(2.0), Some(5.0)),
        (Some(4.0), None),
        (None, Some(2.0)),
    ];
    let seconds = (9_996..=10_000)
        .zip(powers)
        .map(|(wall_second, (system, gpu))| {
            let mut second = reading_at(wall_second, 3);
            second.power_watts = system;
            with_energy_readings(second, gpu, Vec::new())
        })
        .collect::<Vec<_>>();
    let view = view(1, &[], &seconds);
    let expected = panels(
        "energy",
        &selection_of_json(&summary_selection(&[], &seconds)),
        None,
    )
    .unwrap();

    let drawn = view_panels("energy", &view, levels().as_ref()).unwrap();

    let breakdown = panel_named(&drawn, "live-power-breakdown");
    assert_eq!(breakdown, panel_named(&expected, "live-power-breakdown"));
    assert_eq!(
        breakdown.lines[0].values,
        [Some(6.0), Some(0.0), Some(-3.0), None, None]
    );
}

#[test]
fn the_view_refuses_a_power_remainder_that_is_not_finite() {
    // Goal: the View draws no power breakdown whose remainder overflows, as the readings draw
    // none, since a finite system and GPU power can still differ by more than a float holds.
    // Method: a second whose system power is 3e38 and GPU power -3e38, drawn from a View and from
    // its trend points.
    let mut second = reading_at(10_000, 3);
    second.power_watts = Some(3.0e38);
    let seconds = [with_energy_readings(second, Some(-3.0e38), Vec::new())];
    let view = view(1, &[], &seconds);
    let read = panels(
        "energy",
        &selection_of_json(&summary_selection(&[], &seconds)),
        None,
    );

    let drawn = view_panels("energy", &view, levels().as_ref());

    assert!(read.unwrap_err().contains("power remainder"));
    assert!(drawn.unwrap_err().contains("power remainder"));
}

/// The first second that a page drawn from a View whose newest second is 10,000 draws in full
/// detail.
const HOUR_DETAIL_FIRST: u64 = 9_041;

/// What a second of `hour` is, which a page's readings of it follow.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HourSecond {
    /// A second like those beside it.
    Ordinary,
    /// One of the hour's two peaks, at 7,013 and 9,040.
    Peak,
    /// The first second after the gap or the restart, 7,600 and 8,000, which has no rates.
    AfterBreak,
}

/// An hour of seconds that ends at 10,000, each with the complete reading `each` writes for a
/// page given its wall second and what the second is: a gap from 7,500 to 7,599, a collector
/// restart at 8,000, a peak at 7,013 in an older stretch, and one at 9,040, which lies at the
/// join of the older tail and the full-detail seconds.
fn hour(each: impl Fn(MachineSecond, u64, HourSecond) -> MachineSecond) -> Vec<MachineSecond> {
    (6_400..=10_000)
        .filter(|wall_second| !(7_500..7_600).contains(wall_second))
        .map(|wall_second| {
            let segment = match wall_second {
                ..7_500 => 0,
                7_500..8_000 => 1,
                8_000.. => 2,
            };
            let mut second = reading_at(wall_second, segment);
            if wall_second >= 8_000 {
                second.run = [5, 6];
            }
            let moment = match wall_second {
                7_013 | 9_040 => HourSecond::Peak,
                7_600 | 8_000 => HourSecond::AfterBreak,
                _ => HourSecond::Ordinary,
            };
            if moment == HourSecond::AfterBreak {
                second.elapsed_ns = None;
            }

            each(second, wall_second, moment)
        })
        .collect()
}

/// The `hour` of the complete readings the Memory page draws.
fn memory_hour() -> Vec<MachineSecond> {
    hour(|mut second, wall_second, moment| {
        let memory = second.memory.as_mut().unwrap();
        // Every reading moves over each stretch, so the reach of each names it.
        memory.total_bytes += wall_second * 4_096;
        memory.swap_total_bytes += wall_second * 8_192;
        if moment == HourSecond::Peak {
            memory.app_bytes = memory.app_bytes.map(|bytes| bytes * 4);
            memory.wired_bytes *= 4;
            memory.compressed_bytes *= 4;
            memory.swap_used_bytes *= 4;
            memory.total_bytes *= 2;
            memory.swap_total_bytes *= 2;
        }
        let mut rates = memory_rates(&second);
        if moment == HourSecond::Peak {
            rates = rates.map(|rate| rate.map(|rate| rate * 10.0));
        }
        if moment == HourSecond::AfterBreak {
            rates = [None; 5];
        }

        with_memory_rates(second, rates)
    })
}

/// The `hour` of the complete readings the Network page draws.
fn network_hour() -> Vec<MachineSecond> {
    hour(|mut second, _, moment| {
        let mut rates = network_rates(&second);
        if moment == HourSecond::Peak {
            let traffic = second.network.as_mut().unwrap();
            traffic.received_bps *= 10.0;
            traffic.sent_bps *= 10.0;
            rates = rates.map(|rate| rate.map(|rate| rate * 10.0));
        }
        if moment == HourSecond::AfterBreak {
            second.network = None;
            rates = [None; 4];
        }

        with_network_rates(second, rates)
    })
}

/// The `hour` of the complete readings the Storage page draws.
fn storage_hour() -> Vec<MachineSecond> {
    hour(|mut second, _, moment| {
        let mut rates = storage_rates(&second);
        if moment == HourSecond::Peak {
            second.disk_read_bps = Some(1e12);
            second.disk_written_bps = Some(1e12);
            rates = rates.map(|rate| rate.map(|rate| rate * 1_000.0));
        }
        if moment == HourSecond::AfterBreak {
            second.disk_read_bps = None;
            second.disk_written_bps = None;
            rates = [None; 6];
        }

        with_storage_rates(second, rates)
    })
}

/// The `hour` of the complete readings the GPU page draws.
fn gpu_hour() -> Vec<MachineSecond> {
    hour(|mut second, wall_second, moment| {
        // Every reading moves over each stretch, so the reach of each names it.
        let quarter_degrees = u16::try_from(wall_second % 40).unwrap();
        second.gpu_celsius = Some(f32::from(quarter_degrees) / 4.0 + 40.0);
        let mut watts = Some(gpu_watts(&second));
        let mut memory_bytes = gpu_memory_bytes(&second);
        if moment == HourSecond::Peak {
            second.gpu_active_percent = Some(100.0);
            second.gpu_celsius = Some(110.0);
            watts = watts.map(|watts| watts * 10.0);
            memory_bytes = memory_bytes.map(|bytes| bytes.map(|bytes| bytes * 4));
        }
        if moment == HourSecond::AfterBreak {
            second.gpu_active_percent = None;
            watts = None;
        }

        with_gpu_readings(second, watts, memory_bytes)
    })
}

/// The `hour` of the complete readings the CPU page draws.
fn cpu_hour() -> Vec<MachineSecond> {
    hour(|mut second, wall_second, moment| {
        // Every reading moves over each stretch, so the reach of each names it.
        let quarter_degrees = u16::try_from(wall_second % 40).unwrap();
        second.cpu_celsius = Some(f32::from(quarter_degrees) / 4.0 + 60.0);
        let mut readings = cpu_readings(&second);
        if moment == HourSecond::Peak {
            second.cpu_percent = Some(100.0);
            second.cpu_celsius = Some(110.0);
            readings.level_ghz = readings.level_ghz.map(|ghz| ghz.map(|ghz| ghz * 2.0));
            readings.load_average = readings
                .load_average
                .map(|load| load.map(|average| average * 10.0));
            let open_files = readings.open_files.map(|files| files * 3);
            readings.open_files = open_files;
            readings.open_files_percent = open_files.map(open_files_percent);
        }
        if moment == HourSecond::AfterBreak {
            second.cpu_percent = None;
            readings.level_ghz = [None, None];
            readings.load_average = None;
        }

        with_cpu_readings(second, readings)
    })
}

/// The `hour` of the complete readings the Energy page draws.
fn energy_hour() -> Vec<MachineSecond> {
    hour(|mut second, wall_second, moment| {
        // Every reading moves over each stretch, so the reach of each names it.
        let quarter_degrees = u16::try_from(wall_second % 40).unwrap();
        second.soc_celsius = Some(f32::from(quarter_degrees) / 4.0 + 45.0);
        let mut watts = Some(gpu_watts(&second));
        let mut fans = fan_rpm(&second);
        if moment == HourSecond::Peak {
            second.power_watts = Some(500.0);
            second.soc_celsius = Some(110.0);
            watts = watts.map(|watts| watts * 10.0);
            fans = fans
                .into_iter()
                .map(|rpm| rpm.map(|rpm| rpm * 4.0))
                .collect();
        }
        if moment == HourSecond::AfterBreak {
            watts = None;
        }
        // The third fan reads none over the older stretch from 7,200, so that stretch reaches
        // no reading of it, rather than a stopped fan.
        if (7_200..7_210).contains(&wall_second) {
            fans[2] = None;
        }

        with_energy_readings(second, watts, fans)
    })
}

/// The `hour` of the complete readings the Battery page draws.
fn battery_hour() -> Vec<MachineSecond> {
    battery_hour_with(|_battery, _moment| {})
}

/// The Battery page's `hour` whose controller never says whether the battery charges.
fn battery_hour_with_charging_unread() -> Vec<MachineSecond> {
    battery_hour_with(|battery, _moment| battery.charging = None)
}

/// The Battery page's `hour` of a Mac without a battery.
fn battery_hour_without_a_battery() -> Vec<MachineSecond> {
    hour(|second, _wall_second, _moment| with_battery_readings(second, None))
}

/// The `hour` of the complete readings the Battery page draws, with `change` changing the battery
/// of each second, given what the second is.
pub(crate) fn battery_hour_with(
    change: impl Fn(&mut BatteryReading, HourSecond),
) -> Vec<MachineSecond> {
    hour(|second, wall_second, moment| {
        let mut battery = battery_reading(&second);
        // The collector reads the battery once a minute and every tick holds that reading, so
        // each stretch holds one charge, power flow and charging state, which its first second
        // reads too.
        let held = wall_second / 10 * 10;
        battery.charge_current = Some(held);
        battery.current_ma = Some(i64::try_from(held).unwrap() - 9_000);
        battery.charging = Some(held > 9_000);
        // A peak draws power in and charges, though the battery around it drains before 9,000,
        // so its stretch reaches the charging state at the peak's own reading.
        if moment == HourSecond::Peak {
            battery.charge_current = Some(19_000);
            battery.current_ma = Some(5_000);
            battery.charging = Some(true);
        }
        change(&mut battery, moment);

        with_battery_readings(second, Some(battery))
    })
}

/// `seconds` by the stretch each lies in, oldest first.
fn seconds_by_stretch(
    seconds: &[MachineSecond],
) -> std::collections::BTreeMap<u64, Vec<&MachineSecond>> {
    let mut stretches = std::collections::BTreeMap::<u64, Vec<&MachineSecond>>::new();
    for second in seconds {
        let stretch = second.wall_ns / 1_000_000_000 / viewer::EARLIER_STRIDE_SECONDS;
        stretches.entry(stretch).or_default().push(second);
    }
    stretches
}

/// The View of `seconds`, which end at 10,000: its seconds from 9,000 on, the seconds before
/// them as its older tail, and the stretches the store reduced them to, up to the one
/// `HOUR_DETAIL_FIRST` lies in.
pub(crate) fn view_of_hour(seconds: &[MachineSecond]) -> View {
    assert_eq!(seconds.last().unwrap().wall_ns / 1_000_000_000, 10_000);
    let stretches = seconds_by_stretch(seconds);
    let join = HOUR_DETAIL_FIRST / viewer::EARLIER_STRIDE_SECONDS;
    let (earlier, live) = seconds.split_at(
        seconds
            .iter()
            .position(|second| second.wall_ns / 1_000_000_000 >= 9_000)
            .unwrap(),
    );
    let mut view = view(1, earlier, live);
    view.stretches = stretches
        .range(..=join)
        .map(|(stretch, held)| reduced_stretch(*stretch, held))
        .collect();

    view
}

/// What the store reduces `seconds`, all of stretch `stretch`, to, each reading placed in its own
/// run.
fn reduced_stretch(stretch: u64, seconds: &[&MachineSecond]) -> Arc<AppsStretch> {
    let mut held = AppsStretch::of_seconds(stretch, &[Arc::new(AppsSecond::default())]);
    for second in seconds {
        let run = RunIdentity {
            recording: Identity([1, 1]),
            run: Identity(second.run),
            boot: Identity(second.boot),
        };
        held.machine.add(&run, second);
    }

    Arc::new(held)
}

/// The reach the older tail writes for `seconds`, the readings of one stretch.
fn older_tail_reach(seconds: &[&MachineSecond]) -> serde_json::Value {
    let readings: Vec<_> = seconds
        .iter()
        .map(|second| {
            let place = viewer::store::MachinePlace {
                boot: second.boot,
                run: second.run,
                publication: second.publication,
                wall_ns: second.wall_ns,
            };
            (summary_point(second), place)
        })
        .collect();

    viewer::reach_of_readings(&readings)
}

macro_rules! hour_drawn_as_the_readings_draw_it_cases {
    ($($name:ident, $page:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: over the loaded hour the page draws the same charts, peaks included, from
            // the seconds and stretches the store holds as from the trend points and reach the
            // viewer writes of the same readings. Method: an hour with a peak in an older
            // stretch, a gap, a collector restart and a peak at the join, drawn both ways, the
            // tail written as the session writes it: the first reading of each stretch before
            // the join with the reach of the whole stretch, and the reach of the join's stretch
            // on its last point.
            assert_hour_drawn_as_the_readings_draw_it(
                &$page,
                &($page.hour)(),
                $page.unrecorded,
                $page.peak_lines,
            );
        }
    )*};
}

hour_drawn_as_the_readings_draw_it_cases! {
    // Memory: the peaks reach the first line of the swap, swap flow and paging charts.
    the_memory_charts_of_the_view_draw_an_hour_as_the_readings_draw_it, MEMORY;
    // Network: the peaks reach both lines of the traffic, packets and errors charts.
    the_network_charts_of_the_view_draw_an_hour_as_the_readings_draw_it, NETWORK;
    // Storage: the peaks reach both lines of the throughput, operations, service time and errors
    // charts.
    the_storage_charts_of_the_view_draw_an_hour_as_the_readings_draw_it, STORAGE;
    // GPU: the peaks reach the activity, power and sensor lines and both lines of the memory
    // chart.
    the_gpu_charts_of_the_view_draw_an_hour_as_the_readings_draw_it, GPU;
    // CPU: the peaks reach the activity line, both frequency lines, the three load averages,
    // both open files lines and the sensor line.
    the_cpu_charts_of_the_view_draw_an_hour_as_the_readings_draw_it, CPU;
    // Energy: the peaks reach the system power, GPU power and SoC sensor lines, the stacked power
    // chart's GPU line, which reaches the system power, and each of three fan channels.
    the_energy_charts_of_the_view_draw_an_hour_as_the_readings_draw_it, ENERGY;
    // Battery: the peaks reach the charge and power flow lines, and the charging state of the
    // older stretch from 7,010 names the reading at 7,013, the one that charged.
    the_battery_charts_of_the_view_draw_an_hour_as_the_readings_draw_it, BATTERY;
}

macro_rules! battery_hour_drawn_as_the_readings_draw_it_cases {
    ($($name:ident, $hour:expr, $undrawn:expr, $peak_lines:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: over the loaded hour the Battery page draws the same charts from the seconds
            // and stretches the store holds as from the trend points and reach the viewer writes
            // of the same readings, where the battery leaves readings out of every second, so the
            // older stretches hold none of them either. Method: the hour the case gives, drawn
            // both ways as the hour of every page is, with every line drawing a value but the
            // case's undrawn lines, and the hour's peaks reaching the case's lines.
            assert_hour_drawn_as_the_readings_draw_it(&BATTERY, &$hour(), $undrawn, $peak_lines);
        }
    )*};
}

battery_hour_drawn_as_the_readings_draw_it_cases! {
    // No battery: a Mac without one draws no charge, power flow or charging state, in an older
    // stretch as at a recent second, and no peak.
    an_hour_of_a_mac_without_a_battery_draws_no_battery_reading_from_the_view,
        battery_hour_without_a_battery,
        &[
            ("live-battery-charge", "Charge"),
            ("live-battery-flow", "Into battery"),
            ("live-battery-charging", "Charging"),
        ],
        &[];
    // Charging unread: the controller never said whether it charged, so no cell of the charging
    // chart is drawn, while the charge and power flow draw as they read, peaks included.
    an_hour_whose_charging_is_unread_draws_no_charging_state_from_the_view,
        battery_hour_with_charging_unread,
        &[("live-battery-charging", "Charging")],
        &[("live-battery-charge", 0), ("live-battery-flow", 0)];
}

#[test]
fn a_stretch_whose_first_second_reads_no_charging_state_draws_the_state_its_other_seconds_read() {
    // Goal: an older stretch of the charging chart drawn from the View shows the highest state of
    // its ten seconds, as the glossary's Point says an older cell does, though its first second
    // read no state. The reader draws those ten cells blank, since its point is the first
    // second's and its reach leaves out a state that held over the stretch, so the View's cell is
    // the glossary's and the reader's is not. Method: the Battery hour with no charging state at
    // 7,600 and 8,000, drawn from its View, and the ten cells from 7,600, whose other nine
    // seconds read not charging.
    let seconds = battery_hour_with(|battery, moment| {
        if moment == HourSecond::AfterBreak {
            battery.charging = None;
        }
    });
    let view = view_of_hour(&seconds);

    let drawn = view_panels("battery", &view, levels().as_ref()).unwrap();

    let chart = panel_named(&drawn, "live-battery-charging");
    let cell = usize::try_from(7_600 - chart.window.unwrap().first).unwrap();
    assert_eq!(chart.lines[0].values[cell..cell + 10], [Some(0.0); 10]);
}

/// Asserts that `page` draws the hour of `seconds` from a View as from the trend points and
/// reach of the same readings, with every line drawing a value but the `undrawn` lines, which
/// draw none, and the hour's peaks reaching the `peak_lines`.
fn assert_hour_drawn_as_the_readings_draw_it(
    page: &Page,
    seconds: &[MachineSecond],
    undrawn: &[(&str, &str)],
    peak_lines: &[(&str, usize)],
) {
    let view = view_of_hour(seconds);
    let stretches = seconds_by_stretch(seconds);
    let join = HOUR_DETAIL_FIRST / viewer::EARLIER_STRIDE_SECONDS;
    let mut tail: Vec<serde_json::Value> = stretches
        .range(..join)
        .map(|(_, held)| {
            let mut point = summary_point(held[0]);
            point["reach"] = older_tail_reach(held);
            point
        })
        .collect();
    tail.last_mut().unwrap()["reach_after"] = older_tail_reach(&stretches[&join]);
    let detail: Vec<MachineSecond> = seconds
        .iter()
        .filter(|second| second.wall_ns / 1_000_000_000 >= HOUR_DETAIL_FIRST)
        .cloned()
        .collect();
    let mut selected = summary_selection(&[], &detail);
    selected["earlier_trends"] = serde_json::json!(tail);
    let expected = panels(page.route, &selection_of_json(&selected), None);

    let drawn = view_panels(page.route, &view, levels().as_ref());

    let drawn = without_own_reaches(drawn.unwrap());
    assert_eq!(drawn, without_own_reaches(expected.unwrap()));
    assert_lines_drawn(&drawn, undrawn);
    for &(id, line_index) in peak_lines {
        let extremes = panel_named(&drawn, id).lines[line_index]
            .extremes
            .clone()
            .unwrap();
        let peaks: Vec<u64> = extremes
            .iter()
            .flatten()
            .map(|reach| reach.peak.second())
            .filter(|second| matches!(second, 7_013 | 9_040))
            .collect();
        assert_eq!(peaks, [7_013, 9_040], "{id} {line_index}");
        assert_eq!(
            extremes[tail.len()].unwrap().peak.second(),
            9_040,
            "{id} {line_index}"
        );
    }
}

macro_rules! refused_second_drawn_as_it_holds_cases {
    ($($name:ident, $page:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: where the store refused a second's complete reading, the page draws the
            // readings the second holds itself, as the Overview does, and leaves the readings
            // only a complete reading holds missing. Method: a refused second among seconds with
            // complete readings, the value each listed line draws there against the value the
            // worker's charts draw from its trend point of that second, and the state each listed
            // state chart draws there.
            assert_refused_second_drawn_as_it_holds(&$page);
        }
    )*};
}

refused_second_drawn_as_it_holds_cases! {
    // Memory: the readings the second holds itself, the memory and swap in use, are held, the
    // readings only a complete reading holds, the swap flow and paging, are missing, and the
    // pressure chart draws the second's state.
    at_a_refused_second_the_memory_charts_draw_what_the_second_holds, MEMORY;
    // Network: the traffic the second holds itself is held, and the packets and errors only a
    // complete reading holds are missing.
    at_a_refused_second_the_network_charts_draw_what_the_second_holds, NETWORK;
    // Storage: the bytes read and written the second holds itself are held, and the operations,
    // errors and service times only a complete reading holds are missing.
    at_a_refused_second_the_storage_charts_draw_what_the_second_holds, STORAGE;
    // GPU: the activity and sensor the second holds itself are held, and the power and memory
    // only a complete reading holds are missing.
    at_a_refused_second_the_gpu_charts_draw_what_the_second_holds, GPU;
    // CPU: the activity, the sensor and the Cores line the second's logical CPUs give are held,
    // and the clocks, load averages and open files only a complete reading holds are missing.
    at_a_refused_second_the_cpu_charts_draw_what_the_second_holds, CPU;
    // Energy: the system power and SoC sensor the second holds itself are held, the GPU power,
    // the remainder and every fan channel only a complete reading holds are missing, and the
    // thermal chart draws the second's state.
    at_a_refused_second_the_energy_charts_draw_what_the_second_holds, ENERGY;
    // Battery: the charge and power flow the second holds itself are held, and the charging
    // state only a complete reading holds leaves the second's cell blank.
    at_a_refused_second_the_battery_charts_draw_what_the_second_holds, BATTERY;
}

/// Asserts that at a refused second `page` draws what the second holds itself, as the worker
/// draws it, and nothing that only a complete reading holds.
fn assert_refused_second_drawn_as_it_holds(page: &Page) {
    let read = |wall_second| (page.with_reading)(reading_at(wall_second, 3));
    // The second holds in its record what the page's readings give it, as a second the page
    // read does, while its complete reading is refused.
    let mut refused = read(9_999);
    refused.complete = viewer::store::CompleteMachineState::Refused("fixture");
    // The store keeps the per-CPU readings of a second whose complete reading it refused.
    let budget = Arc::new(viewer::store::ByteBudget::new(1_048_576));
    refused.cpu_refused = Some(viewer::store::CpuReading::of_levels(&refused, &budget));
    let seconds = [read(9_998), refused, read(10_000)];
    let view = view(1, &[], &seconds);

    let drawn = view_panels(page.route, &view, levels().as_ref()).unwrap();

    let worker = panels(
        page.route,
        &selection_of_json(&selection(&[], &seconds)),
        None,
    )
    .unwrap();
    for &(id, line_index) in page.held_at_a_refused_second {
        let drawn_line = &panel_named(&drawn, id).lines[line_index];
        let read_by_the_worker = panel_named(&worker, id).lines[line_index].values[1];
        assert!(drawn_line.values[1].is_some(), "{id} {}", drawn_line.name);
        assert_eq!(
            drawn_line.values[1], read_by_the_worker,
            "{id} {}",
            drawn_line.name
        );
    }
    for &(id, line_index) in page.missing_at_a_refused_second {
        let drawn_line = &panel_named(&drawn, id).lines[line_index];
        assert_eq!(drawn_line.values[1], None, "{id} {}", drawn_line.name);
    }
    for &(id, state) in page.states_at_a_refused_second {
        let chart = panel_named(&drawn, id);
        let cell = usize::try_from(9_999 - chart.window.unwrap().first).unwrap();
        assert_eq!(chart.lines[0].values[cell], state, "{id}");
    }
}

#[test]
fn the_older_tail_keeps_a_lone_reading_of_its_ten_seconds() {
    // Goal: a stretch of the older tail whose only reading of a line came at one second, as an
    // idle disk reads one service time, keeps that reading as the stretch's peak at that second.
    // Method: an hour of storage seconds whose one older stretch of 8,500 has no read service
    // time at its first second and exactly one, at 8,503, drawn from the View, then the reach
    // the read service time line holds for that stretch.
    let lone_second = 8_503;
    let read_mean = viewer::store::StorageRate::ALL
        .iter()
        .position(|rate| *rate == viewer::store::StorageRate::ReadMeanMilliseconds)
        .unwrap();
    let seconds: Vec<MachineSecond> = (8_000..=10_000)
        .map(|wall_second| {
            let second = reading_at(wall_second, 0);
            let mut rates = storage_rates(&second);
            if wall_second / 10 == lone_second / 10 {
                rates[read_mean] = (wall_second == lone_second).then_some(42.0);
            }
            with_storage_rates(second, rates)
        })
        .collect();
    let view = view_of_hour(&seconds);

    let drawn = view_panels("storage", &view, levels().as_ref()).unwrap();

    let read_service_time = &panel_named(&drawn, "live-storage-mean").lines[0];
    let reaches: Vec<_> = read_service_time
        .extremes
        .as_ref()
        .unwrap()
        .iter()
        .flatten()
        .filter(|reach| reach.peak.second() / 10 == lone_second / 10)
        .collect();
    assert_eq!(reaches.len(), 1, "{reaches:?}");
    assert_eq!(reaches[0].peak.second(), lone_second);
    assert_eq!(reaches[0].high.to_bits(), 42.0_f32.to_bits());
}

mod when_seconds_are_older_than_the_full_detail_ones {
    use super::*;

    #[test]
    fn a_second_of_a_stretch_already_drawn_is_left_out() {
        // Goal: an older stretch draws one reading, as the trend points keep one, even where the
        // store's older seconds and its live span both hold seconds of it. Method: a stretch
        // whose first second is an older second and whose next ones are live, and a live
        // stretch of two seconds.
        let earlier = [reading_at(9_020, 3)];
        let live = [
            reading_at(9_025, 3),
            reading_at(9_030, 3),
            reading_at(9_031, 3),
            reading_at(9_041, 3),
            reading_at(10_000, 3),
        ];
        let view = view(1, &earlier, &live);
        let kept = [reading_at(9_020, 3), reading_at(9_030, 3)];
        let expected = panels(
            "overview",
            &crate::live_store_fixtures::selection_of_json(&selection(&kept, &live[3..])),
            None,
        );

        let drawn = overview_panels(&view, levels().as_ref());

        assert_eq!(drawn.unwrap(), expected.unwrap());
    }

    #[test]
    fn a_second_of_the_stretch_of_the_first_full_detail_second_is_left_out() {
        // Goal: the older tail ends where the stretch that holds the first full-detail second
        // starts, so that stretch draws its full-detail seconds and nothing before them.
        // Method: a live second in that stretch one second before the first full-detail
        // second, which is 9,041.
        let live = [
            reading_at(9_030, 3),
            reading_at(9_040, 3),
            reading_at(9_041, 3),
            reading_at(10_000, 3),
        ];
        let view = view(1, &[], &live);
        let expected = panels(
            "overview",
            &crate::live_store_fixtures::selection_of_json(&selection(&live[..1], &live[2..])),
            None,
        );

        let drawn = overview_panels(&view, levels().as_ref());

        assert_eq!(drawn.unwrap(), expected.unwrap());
    }

    #[test]
    fn a_second_before_the_loaded_hour_is_left_out() {
        // Goal: the charts span the loaded hour and its lead-in, so an older second the store
        // still holds from before them draws nothing. Method: an older second one second
        // before the first second the charts hold, which is 3,659 seconds before the newest.
        let earlier = [reading_at(6_340, 0), reading_at(6_345, 0)];
        let live = [reading_at(9_041, 1), reading_at(10_000, 1)];
        let view = view(1, &earlier, &live);
        let expected = panels(
            "overview",
            &crate::live_store_fixtures::selection_of_json(&selection(&earlier[1..], &live)),
            None,
        );

        let drawn = overview_panels(&view, levels().as_ref());

        assert_eq!(drawn.unwrap(), expected.unwrap());
    }
}

mod when_the_store_holds_no_second {
    use super::*;

    #[test]
    fn the_overview_draws_its_charts_without_readings() {
        // Goal: before the store holds a second, the page lays out the charts its first second
        // fills, so that second moves nothing. Method: a View with no second.
        let view = view(1, &[], &[]);

        let drawn = overview_panels(&view, None);

        assert_eq!(drawn.unwrap(), empty_panels("overview").unwrap());
    }
}

/// The point a hover of a second stands on, in a View whose newest second is 10,005, so its
/// charts draw the seconds from 9,046 on in full detail.
mod point_second {
    use super::*;

    #[test]
    fn a_second_of_full_detail_stands_on_its_own_point() {
        // Goal: a second the charts draw in full detail stands on its own point. Method: the
        // View holds 9,500 among its full-detail seconds.
        let view = view(1, &[], &held(&[9_030, 9_046, 9_500, 10_005]));

        let shown = overview_point_second(&view, 9_500);

        assert_eq!(shown, Ok(Some(9_500)));
    }

    #[test]
    fn the_first_second_of_full_detail_stands_on_its_own_point() {
        // Goal: the first second the charts draw in full detail is not read as part of the older
        // stretch before it. Method: the View holds 9,046, where full detail starts.
        let view = view(1, &[], &held(&[9_041, 9_046, 10_005]));

        let shown = overview_point_second(&view, 9_046);

        assert_eq!(shown, Ok(Some(9_046)));
    }

    #[test]
    fn a_second_missing_from_full_detail_stands_on_no_point() {
        // Goal: a missed tick in full detail stays a gap rather than borrowing a neighbour's
        // reading. Method: the View holds 9,500 and 9,502 but not 9,501.
        let view = view(1, &[], &held(&[9_500, 9_502, 10_005]));

        let shown = overview_point_second(&view, 9_501);

        assert_eq!(shown, Ok(None));
    }

    #[test]
    fn a_live_second_older_than_full_detail_stands_on_the_point_of_its_stretch() {
        // Goal: a second the View holds, older than full detail, stands on the one point the
        // charts draw for its stretch. Method: the View holds 9,031 and 9,033 of the stretch
        // from 9,030, and the hover is on 9,038.
        let view = view(1, &[], &held(&[9_031, 9_033, 10_005]));

        let shown = overview_point_second(&view, 9_038);

        assert_eq!(shown, Ok(Some(9_031)));
    }

    #[test]
    fn a_second_of_the_older_tail_stands_on_the_point_of_its_stretch() {
        // Goal: a second of the older tail, which keeps one second in every ten, stands on the
        // second kept of its stretch. Method: the older tail holds 8,000, and the hover is on
        // 8,007.
        let view = view(1, &held(&[8_000]), &held(&[10_005]));

        let shown = overview_point_second(&view, 8_007);

        assert_eq!(shown, Ok(Some(8_000)));
    }

    #[test]
    fn a_second_before_full_detail_in_its_first_stretch_stands_on_no_point() {
        // Goal: the seconds before full detail in the stretch where it starts, which the charts
        // draw no point for, stand on no later point. Method: the View holds 9,043 and 9,046 of
        // the stretch from 9,040, and the hover is on 9,043.
        let view = view(1, &[], &held(&[9_043, 9_046, 10_005]));

        let shown = overview_point_second(&view, 9_043);

        assert_eq!(shown, Ok(None));
    }

    fn held(seconds: &[u64]) -> Vec<viewer::store::MachineSecond> {
        seconds
            .iter()
            .map(|second| reading_at(*second, 0))
            .collect()
    }
}

mod when_a_chart_draws_one_reading_of_a_stretch {
    use super::*;

    #[test]
    fn a_peak_of_one_second_reaches_the_chart_at_every_offset() {
        // Goal: a reading that lasted one second stays on the chart once the chart draws one
        // reading of its ten seconds, wherever in those seconds it fell. The first regression of
        // this work, since the chart used to keep only the stretch's first second.
        // Method: for each second of a stretch, a CPU that peaks at that second alone, and read
        // what the chart's line says of the stretch's one point.
        for offset in 0..10 {
            let peak = 9_020 + offset;
            let older = older_seconds(|second, wall_second| {
                second.cpu_percent = Some(if wall_second == peak { 95.0 } else { 10.0 });
            });

            let panels = overview_of_older(&older);

            let line = &panel_named(&panels, "live-cpu").lines[0];
            let reach = line.extremes.as_ref().unwrap()[0].unwrap();
            assert_eq!(Some(reach.high), Some(95.0), "offset {offset}");
            assert_eq!(reach.peak.second(), peak, "offset {offset}");
        }
    }

    #[test]
    fn a_chart_keeps_the_reading_a_point_shows_beside_its_reach() {
        // Goal: drawing a stretch's reach leaves what its point reads alone, since the raw readings
        // a chart shows stay what they were. Method: a CPU that peaks past the stretch's first
        // second, and read both.
        let older = older_seconds(|second, wall_second| {
            second.cpu_percent = Some(if wall_second == 9_025 { 95.0 } else { 10.0 });
        });

        let panels = overview_of_older(&older);

        let line = &panel_named(&panels, "live-cpu").lines[0];
        assert_eq!(line.values[0], Some(10.0));
    }

    #[test]
    fn the_stack_of_memory_reaches_its_total_at_one_instant_and_no_more() {
        // Goal: a stacked chart's peak is what the stack held at one reading, not its layers'
        // separate peaks added up, which it never held. Method: wired peaks in one second and
        // compressed in another, and read the stack's reach beside each layer.
        let older = older_seconds(|second, wall_second| {
            let memory = second.memory.as_mut().unwrap();
            memory.app_bytes = Some(1 << 30);
            memory.wired_bytes = if wall_second == 9_023 {
                3 << 30
            } else {
                1 << 30
            };
            memory.compressed_bytes = if wall_second == 9_027 {
                3 << 30
            } else {
                1 << 30
            };
        });

        let panels = overview_of_older(&older);

        let lines = &panel_named(&panels, "live-memory").lines;
        assert_eq!(lines[0].extremes, None);
        assert_eq!(lines[1].extremes, None);
        let reach = lines[2].extremes.as_ref().unwrap()[0].unwrap();
        assert_eq!(Some(reach.high), Some(5.0));
    }

    #[test]
    fn a_stretch_of_one_second_reaches_what_that_second_reads_on_every_chart() {
        // Goal: a chart's reach reads the series the chart's line reads, on every line the Overview
        // draws, so no table gives one reading as another's maximum. Method: reduce one second,
        // then compare what each line reads of it with what it reaches.
        let mut second = reading_at(9_020, 3);
        second.cpu_percent = Some(11.0);
        second.cpu_level_percent = [Some(12.0), Some(13.0)];
        second.gpu_active_percent = Some(14.0);
        second.power_watts = Some(15.0);
        second.soc_celsius = Some(16.0);
        second.fan_mean_rpm = Some(17.0);
        second.disk_read_bps = Some(18.0);
        second.disk_written_bps = Some(19.0);
        let stretch = stretch_of(902, std::slice::from_ref(&second));
        let seconds = [&second];
        let stretches = [Some(&*stretch.machine)];
        let points = ViewPanelsPoints {
            seconds: &seconds,
            stretches: &stretches,
        };

        let readings = overview(levels().as_ref())
            .into_iter()
            .flat_map(|specification| specification.lines)
            .filter(|reading| reading.reach.is_none());
        for reading in readings {
            let expected = points.value(&reading, 0).unwrap();

            let reach = points.reach(&reading, 0).unwrap();

            assert_eq!(reach.map(|reach| reach.high), expected, "{}", reading.key);
        }
    }
    #[test]
    fn a_chart_says_what_its_points_stand_for_and_where_its_highest_reading_is() {
        // Goal: a reader who sees a line can learn which statistic it is and where the reading it
        // smooths is found, and that a state's cell is no mean. Method: read the help of a chart of
        // numbers and of states.
        let older = older_seconds(|_, _| {});

        let panels = overview_of_older(&older);

        let help = |id| panel_named(&panels, id).help.clone().unwrap();
        assert!(help("live-cpu").contains("Each point is one reading"));
        assert!(help("live-cpu").contains("The Data table gives the highest reading it keeps"));
        assert!(help("live-memory-pressure").contains("shows the highest state among them"));
    }

    /// The reach of the line named `line` of the chart `panel`, for a stretch that peaks at each of
    /// its seconds in turn, as `read` writes a reading into a second, names that second.
    fn every_offset_reaches(panel: &str, line: &str, read: impl Fn(&mut MachineSecond, f32)) {
        for offset in 0..10 {
            let peak = 9_020 + offset;
            let older = older_seconds(|second, wall_second| {
                read(second, if wall_second == peak { 95.0 } else { 10.0 });
            });

            let panels = overview_of_older(&older);

            let chart = panel_named(&panels, panel);
            let drawn = chart.lines.iter().find(|drawn| drawn.name == line).unwrap();
            let reach = drawn.extremes.as_ref().unwrap()[0].unwrap();
            assert_eq!(Some(reach.high), Some(95.0), "offset {offset}");
            assert_eq!(reach.peak.second(), peak, "offset {offset}");
        }
    }

    #[test]
    fn a_gpu_peak_of_one_second_reaches_its_chart_at_every_offset() {
        // Goal: a one-second GPU peak stays on the chart wherever in its ten seconds it fell.
        // Method: `every_offset_reaches`.
        every_offset_reaches("live-gpu-activity", "Busy", |second, value| {
            second.gpu_active_percent = Some(f64::from(value));
        });
    }

    #[test]
    fn a_power_peak_of_one_second_reaches_its_chart_at_every_offset() {
        // Goal: a one-second power peak stays on the chart wherever in its ten seconds it fell.
        // Method: `every_offset_reaches`.
        every_offset_reaches("live-power", "Whole machine", |second, value| {
            second.power_watts = Some(value);
        });
    }

    #[test]
    fn a_temperature_peak_of_one_second_reaches_its_chart_at_every_offset() {
        // Goal: a one-second temperature peak stays on the chart wherever in its ten seconds it
        // fell. Method: `every_offset_reaches`.
        every_offset_reaches("live-correlation", "SoC temperature", |second, value| {
            second.soc_celsius = Some(value);
        });
    }

    #[test]
    fn a_fan_peak_of_one_second_reaches_its_chart_at_every_offset() {
        // Goal: a one-second fan peak stays on the chart wherever in its ten seconds it fell.
        // Method: `every_offset_reaches`.
        every_offset_reaches("live-correlation", "Fans", |second, value| {
            second.fan_mean_rpm = Some(value);
        });
    }

    #[test]
    fn a_chart_finds_each_lines_reach_where_a_trend_point_holds_its_number() {
        // Goal: each line asks for its reach by the path a trend point holds its number at, which
        // is the path the reach's writer gives it, whatever form that path takes.
        // Method: each line of the Overview, whose lines take a resource or a channel, against a
        // reach holding only that line's path, and against what a trend point holds.
        let point = trend_point(&reading_at(9_020, 3));
        let derived = [
            "memory_app_bytes",
            "memory_used_bytes",
            "power_remainder_watts",
            "fan_mean_rpm",
        ];
        let peak = crate::data::MachinePlace::default();
        let readings = overview(levels().as_ref())
            .into_iter()
            .flat_map(|specification| specification.lines);
        for reading in readings {
            let key = reading.reach.unwrap_or(reading.key);
            let path = viewer::number_path(reading.resource, key, reading.channel);
            let reached = serde_json::json!({"reach": {
                "leaves": {path.clone(): [1.0, 2.0, 0]},
                "places": [reach_place(&peak)],
            }});

            let points = JoinedPoints {
                older: &[],
                recent: RecentPoints::Readings(std::slice::from_ref(&reached)),
            };

            let reach = points.reach(&reading, 0).unwrap();

            assert_eq!(reach.map(|reach| reach.high), Some(2.0), "{path}");
            let held = point.pointer(&format!("/{path}")).is_some();
            assert!(held || derived.contains(&path.as_str()), "{path}");
        }
    }

    #[test]
    fn the_stacked_power_chart_reaches_its_total_at_one_instant() {
        // Goal: the top of the Energy page's stack names the whole machine's reach, which adding up
        // the stack's parts would overstate.
        // Method: a point whose reach names the machine's power, and read the stack's top line.
        let mut point = trend_point(&reading_at(9_020, 3));
        point["reach"] = serde_json::json!({"leaves": {"power_watts": [10.0, 95.0, 0]},
            "places": [reach_place(&crate::data::MachinePlace::default())]});
        let newest = reading_at(10_000, 3);
        let mut selected = selection(&[], std::slice::from_ref(&newest));
        selected["earlier_trends"] = serde_json::json!([point]);

        let panels = panels(
            "energy",
            &crate::live_store_fixtures::selection_of_json(&selected),
            None,
        )
        .unwrap();

        let chart = panel_named(&panels, "live-power-breakdown");
        let top = chart.lines.iter().find(|line| line.name == "GPU").unwrap();
        let reach = top.extremes.as_ref().unwrap()[0];
        assert_eq!(reach.map(|reach| reach.high), Some(95.0));
    }

    #[test]
    fn only_the_first_full_detail_second_stands_for_the_stretch_at_the_join() {
        // Goal: a full-detail second stands for itself, and the first of them also for the seconds
        // before it that lie in no point, whichever stretch it reads in itself.
        // Method: a gap across the join, so the first full-detail second reads in a later stretch
        // than the one at the join, and read the reach of the first two.
        let joined = reading_at(9_040, 3);
        let later = (9_052..=10_000).map(|wall_second| reading_at(wall_second, 3));
        let seconds: Vec<_> = std::iter::once(joined.clone()).chain(later).collect();
        let mut view = view(1, &[], &seconds);
        let mut peak = joined;
        peak.cpu_percent = Some(95.0);
        view.stretches = vec![stretch_of(904, &[peak])];

        let panels = overview_panels(&view, levels().as_ref()).unwrap();

        let line = &panel_named(&panels, "live-cpu").lines[0];
        let extremes = line.extremes.as_ref().unwrap();
        assert_eq!(extremes[0].map(|reach| reach.high), Some(95.0));
        assert_eq!(extremes[1], None);
    }

    #[test]
    fn a_thermal_chart_of_a_view_shows_the_worst_state_a_stretch_reached() {
        // Goal: Overview's thermal chart keeps a short spell in a worse state that its first second
        // does not read, and names the reading that read it.
        // Method: a stretch Nominal but for one second past its first, and read its cell.
        let older = older_seconds(|second, wall_second| {
            second.thermal_pressure = Some(if wall_second == 9_024 { 2 } else { 0 });
        });

        let panels = overview_of_older(&older);

        let panel = panel_named(&panels, crate::live_projection::THERMAL_STATE_ID);
        let cell = usize::try_from(9_020 - panel.window.unwrap().first).unwrap();
        let line = &panel.lines[0];
        assert_eq!(line.values[cell], Some(2.0));
        assert_eq!(
            line.extremes.as_ref().unwrap()[cell].unwrap().peak.second(),
            9_024
        );
    }

    #[test]
    fn a_state_chart_shows_the_worst_state_a_stretch_reached() {
        // Goal: a chart of states keeps a short spell in a worse state once it draws one reading of
        // its ten seconds, and no state between them, since a state has no mean.
        // Method: a stretch Normal but for one Critical second past its first, and read the cells.
        let older = older_seconds(|second, wall_second| {
            let memory = second.memory.as_mut().unwrap();
            memory.pressure = if wall_second == 9_026 {
                Pressure::Critical
            } else {
                Pressure::Normal
            };
        });

        let panels = overview_of_older(&older);

        let panel = panel_named(&panels, "live-memory-pressure");
        let first = usize::try_from(9_020 - panel.window.unwrap().first).unwrap();
        assert_eq!(panel.lines[0].values[first..first + 10], [Some(2.0); 10]);
    }

    #[test]
    fn a_state_cell_names_the_reading_that_reached_its_state() {
        // Goal: a pick of ten cells that read Critical for one second holds that second, and no
        // reading stays named by the cell of another.
        // Method: a stretch Normal but for one Critical second past its first, and read a cell.
        let older = older_seconds(|second, wall_second| {
            let memory = second.memory.as_mut().unwrap();
            memory.pressure = if wall_second == 9_026 {
                Pressure::Critical
            } else {
                Pressure::Normal
            };
        });

        let panels = overview_of_older(&older);

        let panel = panel_named(&panels, "live-memory-pressure");
        let cell = usize::try_from(9_023 - panel.window.unwrap().first).unwrap();
        let reach = panel.lines[0].extremes.as_ref().unwrap()[cell].unwrap();
        assert_eq!(reach.peak.second(), 9_026);
        assert_eq!(panel.places.as_ref().unwrap()[cell].second(), 9_020);
    }

    #[test]
    fn a_state_cell_whose_own_reading_read_its_state_names_no_other() {
        // Goal: a pick of a cell whose point already reads what it shows stays on the second under
        // the pointer. Method: a stretch whose first second is its worst, and read a cell.
        let older = older_seconds(|second, wall_second| {
            let memory = second.memory.as_mut().unwrap();
            memory.pressure = if wall_second == 9_020 {
                Pressure::Critical
            } else {
                Pressure::Normal
            };
        });

        let panels = overview_of_older(&older);

        let panel = panel_named(&panels, "live-memory-pressure");
        let cell = usize::try_from(9_023 - panel.window.unwrap().first).unwrap();
        assert_eq!(panel.lines[0].extremes.as_ref().unwrap()[cell], None);
    }

    #[test]
    fn a_state_no_chart_knows_leaves_a_cell_what_its_own_reading_read() {
        // Goal: a recording can hold a level the chart has no state for, and that level blanks no
        // stretch. Method: a point reading fair that names a reach of an unknown level.
        let older = older_seconds(|second, wall_second| {
            second.cpu_percent = Some(if wall_second == 9_024 { 95.0 } else { 10.0 });
        });
        let drawn = overview_of_older(&older);
        let line = &panel_named(&drawn, "live-cpu").lines[0];
        let peak = line.extremes.as_ref().unwrap()[0].unwrap().peak;
        let newest = reading_at(10_000, 3);
        let mut point = trend_point(&older[0]);
        point["thermal_pressure"] = serde_json::json!(1);
        point["reach"] = serde_json::json!({"leaves": {"thermal_pressure": [1.0, 9.0, 0]},
            "places": [reach_place(&peak)]});
        let mut selected = selection(&[], std::slice::from_ref(&newest));
        selected["earlier_trends"] = serde_json::json!([point]);

        let panels = panels(
            "energy",
            &crate::live_store_fixtures::selection_of_json(&selected),
            None,
        )
        .unwrap();

        let panel = panel_named(&panels, crate::live_projection::THERMAL_STATE_ID);
        let cell = usize::try_from(9_020 - panel.window.unwrap().first).unwrap();
        assert_eq!(panel.lines[0].values[cell], Some(1.0));
    }

    #[test]
    fn a_stretch_reaches_the_same_from_the_store_as_from_its_trend_points() {
        // Goal: the two routes to a chart agree on a stretch's reach as they agree on its readings.
        // Method: the Overview of a store's stretch, and of the trend points that carry the reach
        // the viewer writes for the same seconds.
        let older = older_seconds(|second, wall_second| {
            second.cpu_percent = Some(if wall_second == 9_024 { 95.0 } else { 10.0 });
        });
        let newest = reading_at(10_000, 3);
        let mut point = trend_point(&older[0]);
        let drawn = overview_of_older(&older);
        let line = &panel_named(&drawn, "live-cpu").lines[0];
        let peak = line.extremes.as_ref().unwrap()[0].unwrap().peak;
        point["reach"] = serde_json::json!({"leaves": {"cpu_percent": [10.0, 95.0, 0]},
            "places": [reach_place(&peak)]});
        let mut selected = selection(&[], std::slice::from_ref(&newest));
        selected["earlier_trends"] = serde_json::json!([point]);

        let expected = panels(
            "overview",
            &crate::live_store_fixtures::selection_of_json(&selected),
            None,
        )
        .unwrap();

        let expected_line = &panel_named(&expected, "live-cpu").lines[0];
        assert_eq!(peak.second(), 9_024);
        assert_eq!(expected_line.extremes, line.extremes);
    }
    #[test]
    fn a_peak_in_the_seconds_at_the_join_stays_on_the_chart() {
        // Goal: the seconds of the stretch that holds the first full-detail second, before that
        // second, lie in no point, so a peak there stays on the chart until it ages into a stretch
        // of the tail. Method: a peak in the second before the first full-detail second, with the
        // view at each offset of a stretch, and read what the first full-detail point reaches.
        for offset in 0..9 {
            let last = 10_000 + offset;
            let recent_first = last - 959;
            let seconds: Vec<_> = ((recent_first - 1)..=last)
                .map(|wall_second| {
                    let mut second = reading_at(wall_second, 3);
                    let peak = wall_second == recent_first - 1;
                    second.cpu_percent = Some(if peak { 95.0 } else { 10.0 });
                    second
                })
                .collect();
            let joined: Vec<_> = seconds
                .iter()
                .filter(|second| second.wall_ns / 1_000_000_000 / 10 == (recent_first / 10))
                .cloned()
                .collect();
            let mut view = view(1, &[], &seconds);
            view.stretches = vec![stretch_of(recent_first / 10, &joined)];

            let panels = overview_panels(&view, levels().as_ref()).unwrap();

            let line = &panel_named(&panels, "live-cpu").lines[0];
            assert_eq!(
                line.extremes.as_ref().unwrap()[0].map(|reach| reach.high),
                Some(95.0),
                "offset {offset}"
            );
        }
    }

    #[test]
    fn the_first_point_after_the_tail_takes_what_the_seconds_at_the_join_reached() {
        // Goal: the trend route shows the join's peak as the store route does.
        // Method: a tail whose last point names what the join reached, and read the first
        // full-detail point of the chart.
        let mut older = trend_point(&reading_at(9_020, 3));
        let joined = crate::data::MachinePlace {
            run: [1, 2],
            boot: [3, 4],
            publication: 7,
            wall_ns: 9_031_000_000_000,
        };
        older["reach_after"] = serde_json::json!({"leaves": {"cpu_percent": [10.0, 95.0, 0]},
            "places": [reach_place(&joined)]});
        let newest = reading_at(10_000, 3);
        let mut selected = selection(&[], std::slice::from_ref(&newest));
        selected["earlier_trends"] = serde_json::json!([older]);

        let drawn = panels(
            "overview",
            &crate::live_store_fixtures::selection_of_json(&selected),
            None,
        )
        .unwrap();

        let line = &panel_named(&drawn, "live-cpu").lines[0];
        let reach = line.extremes.as_ref().unwrap()[1].unwrap();
        assert_eq!(Some(reach.high), Some(95.0));
        let peak = reach.peak;
        assert_eq!((peak.run, peak.boot, peak.publication), ([1, 2], [3, 4], 7));
        assert_eq!(peak.second(), 9_031);
    }
}
