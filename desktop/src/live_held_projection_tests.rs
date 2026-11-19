use std::sync::Arc;

use collector::machine::Pressure;
use collector::recording::identity_text;
use collector::ring::{Identity, RunIdentity};
use viewer::store::{AppsSecond, AppsStretch, MachineSecond, MachineStretch, View};

use super::super::{JoinedPoints, RecentPoints, empty_panels, overview, panels};
use super::{Observations, ViewPanelsPoints, overview_point_second, view_panels};
use crate::data::Panel;
use crate::live_store_fixtures::{
    levels, memory_rates, overview_panels, reading_at, selection, selection_of_json, summary_point,
    summary_selection, trend_point, view, with_memory_rates, with_memory_reading,
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

/// The panel the Overview names `id`.
fn panel_named<'panels>(panels: &'panels [std::rc::Rc<Panel>], id: &str) -> &'panels Panel {
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

/// The Memory lines whose readings the collector records nowhere, by chart and line.
const MEMORY_UNRECORDED: [(&str, &str); 2] = [
    ("live-swap-flow", "Paged out"),
    ("live-memory-temperature", "Memory"),
];

/// Asserts that every line of `panels` draws a value, except the lines no reading is recorded
/// for, which draw none.
fn assert_memory_lines_drawn(panels: &[std::rc::Rc<Panel>]) {
    for panel in panels {
        for line in &panel.lines {
            let unrecorded = MEMORY_UNRECORDED.contains(&(panel.id.as_str(), line.name.as_str()));
            let drawn = line.values.iter().filter(|value| value.is_some()).count();
            if unrecorded {
                assert_eq!(drawn, 0, "{} {}", panel.id, line.name);
            } else {
                assert!(drawn > 0, "{} {}", panel.id, line.name);
            }
        }
    }
}

#[test]
fn the_memory_charts_of_the_view_draw_as_the_readings_draw_them() {
    // Goal: the Memory page draws the same charts from the seconds the store holds as from the
    // trend points the viewer writes of their complete readings. Method: seconds whose complete
    // readings hold every reading Memory draws, each with its own value, across older seconds and
    // a break, drawn both ways, with every line but the two recorded nowhere drawing a value.
    let read = |wall_second, segment| with_memory_reading(reading_at(wall_second, segment));
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
        "memory",
        &selection_of_json(&summary_selection(&[earlier, older].concat(), &detail)),
        None,
    );

    let drawn = view_panels("memory", &view, levels().as_ref());

    let drawn = drawn.unwrap();
    assert_eq!(drawn, expected.unwrap());
    assert_eq!(drawn.len(), 6);
    assert_memory_lines_drawn(&drawn);
}

/// The first second the Memory page of a View whose newest second is 10,000 draws in full
/// detail.
const MEMORY_HOUR_DETAIL_FIRST: u64 = 9_041;

/// An hour of seconds that ends at 10,000, each with the complete reading the Memory page draws:
/// a gap from 7,500 to 7,599, a collector restart at 8,000, a peak at 7,013 in an older stretch,
/// and one at 9,040, which lies at the join of the older tail and the full-detail seconds.
fn memory_hour() -> Vec<MachineSecond> {
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
            let peak = matches!(wall_second, 7_013 | 9_040);
            let memory = second.memory.as_mut().unwrap();
            // Every reading moves over each stretch, so the reach of each names it.
            memory.total_bytes += wall_second * 4_096;
            memory.swap_total_bytes += wall_second * 8_192;
            if peak {
                memory.app_bytes = memory.app_bytes.map(|bytes| bytes * 4);
                memory.wired_bytes *= 4;
                memory.compressed_bytes *= 4;
                memory.swap_used_bytes *= 4;
                memory.total_bytes *= 2;
                memory.swap_total_bytes *= 2;
            }
            let mut rates = memory_rates(&second);
            if peak {
                rates = rates.map(|rate| rate.map(|rate| rate * 10.0));
            }
            // A gap and a restart each leave the first second after them with no rates.
            if matches!(wall_second, 7_600 | 8_000) {
                second.elapsed_ns = None;
                rates = [None; 5];
            }
            with_memory_rates(second, rates)
        })
        .collect()
}

/// `seconds` by the stretch each lies in, oldest first.
fn memory_hour_stretches(
    seconds: &[MachineSecond],
) -> std::collections::BTreeMap<u64, Vec<&MachineSecond>> {
    let mut stretches = std::collections::BTreeMap::<u64, Vec<&MachineSecond>>::new();
    for second in seconds {
        let stretch = second.wall_ns / 1_000_000_000 / viewer::EARLIER_STRIDE_SECONDS;
        stretches.entry(stretch).or_default().push(second);
    }
    stretches
}

/// What the store reduces `seconds`, all of stretch `stretch`, to, each reading placed in its own
/// run.
fn memory_hour_stretch(stretch: u64, seconds: &[&MachineSecond]) -> Arc<AppsStretch> {
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
fn memory_hour_reach(seconds: &[&MachineSecond]) -> serde_json::Value {
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

#[test]
fn the_memory_charts_of_the_view_draw_an_hour_as_the_readings_draw_it() {
    // Goal: over the loaded hour the Memory page draws the same charts, peaks included, from the
    // seconds and stretches the store holds as from the trend points and reach the viewer writes
    // of the same readings. Method: an hour with a peak in an older stretch, a gap, a collector
    // restart and a peak at the join, drawn both ways, the tail written as the session writes it:
    // the first reading of each stretch before the join with the reach of the whole stretch, and
    // the reach of the join's stretch on its last point.
    let seconds = memory_hour();
    let stretches = memory_hour_stretches(&seconds);
    let join = MEMORY_HOUR_DETAIL_FIRST / viewer::EARLIER_STRIDE_SECONDS;
    let (earlier, live) = seconds.split_at(
        seconds
            .iter()
            .position(|second| second.wall_ns / 1_000_000_000 >= 9_000)
            .unwrap(),
    );
    let mut view = view(1, earlier, live);
    view.stretches = stretches
        .range(..=join)
        .map(|(stretch, held)| memory_hour_stretch(*stretch, held))
        .collect();
    let mut tail: Vec<serde_json::Value> = stretches
        .range(..join)
        .map(|(_, held)| {
            let mut point = summary_point(held[0]);
            point["reach"] = memory_hour_reach(held);
            point
        })
        .collect();
    tail.last_mut().unwrap()["reach_after"] = memory_hour_reach(&stretches[&join]);
    let detail: Vec<MachineSecond> = seconds
        .iter()
        .filter(|second| second.wall_ns / 1_000_000_000 >= MEMORY_HOUR_DETAIL_FIRST)
        .cloned()
        .collect();
    let mut selected = summary_selection(&[], &detail);
    selected["earlier_trends"] = serde_json::json!(tail);
    let expected = panels("memory", &selection_of_json(&selected), None);

    let drawn = view_panels("memory", &view, levels().as_ref());

    let drawn = drawn.unwrap();
    assert_eq!(drawn, expected.unwrap());
    assert_memory_lines_drawn(&drawn);
    for (id, line) in [("live-swap", 0), ("live-swap-flow", 0), ("live-paging", 0)] {
        let extremes = panel_named(&drawn, id).lines[line]
            .extremes
            .clone()
            .unwrap();
        let peaks: Vec<u64> = extremes
            .iter()
            .flatten()
            .map(|reach| reach.peak.second())
            .filter(|second| matches!(second, 7_013 | 9_040))
            .collect();
        assert_eq!(peaks, [7_013, 9_040], "{id}");
        assert_eq!(extremes[tail.len()].unwrap().peak.second(), 9_040, "{id}");
    }
}

#[test]
fn at_a_refused_second_the_memory_charts_draw_what_the_second_holds() {
    // Goal: where the store refused a second's complete reading, the Memory page draws the
    // readings the second holds itself, as the Overview does, and leaves the readings only a
    // complete reading holds missing. Method: a refused second among seconds with complete
    // readings, and the value each Memory line draws there against the value the worker's
    // charts draw from its trend point of that second.
    let read = |wall_second| with_memory_reading(reading_at(wall_second, 3));
    let mut refused = reading_at(9_999, 3);
    refused.complete = viewer::store::CompleteMachineState::Refused("fixture");
    let seconds = [read(9_998), refused, read(10_000)];
    let view = view(1, &[], &seconds);

    let drawn = view_panels("memory", &view, levels().as_ref()).unwrap();

    let worker = panels(
        "memory",
        &selection_of_json(&selection(&[], &seconds)),
        None,
    )
    .unwrap();
    let at = |id: &str, line: usize| {
        let line = &panel_named(&drawn, id).lines[line];
        (line.name.clone(), line.values[1])
    };
    let read_by_the_worker = |id: &str, line: usize| panel_named(&worker, id).lines[line].values[1];
    let held = [
        ("live-memory", 0),
        ("live-memory", 1),
        ("live-memory", 2),
        ("live-memory", 3),
        ("live-swap", 0),
        ("live-swap", 1),
    ];
    let missing = [
        at("live-swap-flow", 0),
        at("live-swap-flow", 2),
        at("live-paging", 0),
        at("live-paging", 1),
        at("live-paging", 2),
    ];
    for (id, line) in held {
        let (name, value) = at(id, line);
        assert!(value.is_some(), "{name}");
        assert_eq!(value, read_by_the_worker(id, line), "{name}");
    }
    for (name, value) in missing {
        assert_eq!(value, None, "{name}");
    }
    let pressure = &panel_named(&drawn, "live-memory-pressure").lines[0];
    let window = panel_named(&drawn, "live-memory-pressure").window.unwrap();
    let cell = usize::try_from(9_999 - window.first).unwrap();
    assert_eq!(pressure.values[cell], Some(0.0));
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
