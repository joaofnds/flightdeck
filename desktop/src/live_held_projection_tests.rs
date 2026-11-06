use std::sync::Arc;

use collector::machine::Pressure;
use collector::ring::{Identity, RunIdentity};
use viewer::store::{AppsSecond, AppsStretch, MachineSecond, MachineStretch, View};

use super::super::{empty_panels, overview, panels};
use super::{Observations, OverviewPoints, overview_panels, overview_point_second};
use crate::data::Panel;
use crate::live_store_fixtures::{levels, reading_at, selection, trend_point, view};

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
        &selection(&[earlier, older].concat(), &detail),
        None,
    );

    let drawn = overview_panels(&view, levels().as_ref());

    assert_eq!(drawn.unwrap(), expected.unwrap());
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
        let expected = panels("overview", &selection(&kept, &live[3..]), None);

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
        let expected = panels("overview", &selection(&live[..1], &live[2..]), None);

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
        let expected = panels("overview", &selection(&earlier[1..], &live), None);

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
            assert_eq!((reach.low, reach.high), (10.0, 95.0), "offset {offset}");
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
        assert_eq!((reach.low, reach.high), (3.0, 5.0));
    }

    #[test]
    fn a_stretch_of_one_second_reaches_what_that_second_reads_on_every_chart() {
        // Goal: a chart's reach reads the series the chart's line reads, on every line the Overview
        // draws, so no chart bands one reading under another's curve. Method: reduce one second,
        // then compare what each line reads of it with what it reaches.
        let second = reading_at(9_020, 3);
        let stretch = stretch_of(902, std::slice::from_ref(&second));
        let seconds = [&second];
        let stretches = [Some(&*stretch.machine)];
        let points = OverviewPoints {
            seconds: &seconds,
            stretches: &stretches,
        };

        let readings = overview(levels().as_ref())
            .into_iter()
            .flat_map(|specification| specification.lines)
            .filter(|reading| reading.reach.is_none());
        for reading in readings {
            let expected = points.value(&reading, 0).unwrap().map(|read| (read, read));

            let reach = points.reach(&reading, 0).unwrap();

            let reach = reach.map(|reach| (reach.low, reach.high));
            assert_eq!(reach, expected, "{}", reading.key);
        }
    }
    #[test]
    fn a_chart_says_what_its_points_and_marks_stand_for() {
        // Goal: a reader who sees a line and a stem can learn which statistic each one is, and that
        // a state's cell is no mean. Method: read the help of a chart of numbers and of states.
        let older = older_seconds(|_, _| {});

        let panels = overview_of_older(&older);

        let help = |id| panel_named(&panels, id).help.clone().unwrap();
        assert!(help("live-cpu").contains(crate::live_projection::LINE_STATISTICS_HELP));
        assert!(
            help("live-memory-pressure").contains(crate::live_projection::STATE_STATISTICS_HELP)
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
            "places": [[peak.run, peak.boot, peak.publication, peak.wall_ns]]});
        let mut selected = selection(&[], std::slice::from_ref(&newest));
        selected["earlier_trends"] = serde_json::json!([point]);

        let expected = panels("overview", &selected, None).unwrap();

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
        older["reach_after"] = serde_json::json!({"leaves": {"cpu_percent": [10.0, 95.0, 0]},
            "places": [[[1, 2], [3, 4], 7, 9_031_000_000_000_u64]]});
        let newest = reading_at(10_000, 3);
        let mut selected = selection(&[], std::slice::from_ref(&newest));
        selected["earlier_trends"] = serde_json::json!([older]);

        let drawn = panels("overview", &selected, None).unwrap();

        let line = &panel_named(&drawn, "live-cpu").lines[0];
        let reach = line.extremes.as_ref().unwrap()[1].unwrap();
        assert_eq!((reach.low, reach.high), (10.0, 95.0));
        let peak = reach.peak;
        assert_eq!((peak.run, peak.boot, peak.publication), ([1, 2], [3, 4], 7));
        assert_eq!(peak.second(), 9_031);
    }
}
