use serde_json::{Value, json};
use viewer::store::MachineSecond;

use super::super::{AppAttributionHistory, Metric};
use crate::live_store_app_fixtures::{
    apps_at_observation, charted, on_the_clock, seconds, worker_app, worker_measure,
    worker_observation,
};
use crate::live_store_fixtures::selection;

#[test]
fn draws_each_second_as_the_workers_history_of_those_seconds_draws_it() {
    // Goal: Overview's CPU chart drawn from the store's View is the chart the worker's App
    // history of the same seconds draws, but for the span it is drawn across and what its help
    // says it chose its leaders over. Method: a View of four seconds, whose third holds no Apps
    // and whose fourth does not follow the third, beside the history the worker writes of them,
    // and compare the two charts on the clock.
    let seconds = seconds();
    let worker = AppAttributionHistory::prepare(
        Metric::Cpu,
        &worker_history(&seconds),
        &selection(&[], &seconds),
    );
    let (view, cohorts) = charted(&seconds);

    let drawn = AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts).panel();

    assert!(drawn.is_some());
    assert_eq!(on_the_clock(drawn), on_the_clock(worker.panel()));
}

#[test]
fn describes_each_second_as_the_workers_history_of_those_seconds_describes_it() {
    // Goal: hovering Overview's CPU chart drawn from the store's View reads as hovering the
    // chart the worker's history draws. Method: the seconds of the chart test, and what the hover
    // shows at each of them, the missing third included.
    let seconds = seconds();
    let worker = AppAttributionHistory::prepare(
        Metric::Cpu,
        &worker_history(&seconds),
        &selection(&[], &seconds),
    );
    let (view, cohorts) = charted(&seconds);

    let drawn = AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts);

    assert_eq!(hovered(&drawn), hovered(&worker));
}

#[test]
fn says_its_leaders_lead_over_the_range_shown() {
    // Goal: the help of a chart drawn from the store's View says it draws the Apps that led over
    // the range shown, since the page chooses them over the range the reader picked, where the
    // worker's chart chooses them over the context it holds. Method: the help of the chart drawn
    // from the View of the chart test beside the worker's.
    let seconds = seconds();
    let worker = AppAttributionHistory::prepare(
        Metric::Cpu,
        &worker_history(&seconds),
        &selection(&[], &seconds),
    );
    let (view, cohorts) = charted(&seconds);

    let drawn = AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts).panel();

    let help = |panel: Option<std::rc::Rc<crate::data::Panel>>| panel.unwrap().help.clone();
    let help = (help(drawn).unwrap(), help(worker.panel()).unwrap());
    assert!(
        help.0
            .contains("Eight leaders use their mean rate over the range shown.")
    );
    assert!(
        help.1
            .contains("Eight leaders use the complete context's mean rate.")
    );
}

mod after_the_chart_of_an_older_view {
    use super::{AppAttributionHistory, Metric, hovered, seconds};
    use crate::live_store_app_fixtures::sliding;

    #[test]
    fn draws_the_chart_its_own_seconds_draw() {
        // Goal: a chart drawn from a View after the chart of an older View of the same Apps is
        // the chart the newer View draws alone, its line broken where the newer View's seconds
        // break it. Method: an older View of three seconds whose second does not follow its
        // first, a newer View of its last two and a later second, and the chart drawn after the
        // older one's beside the chart the newer View draws alone.
        let seconds = seconds();
        let (view, cohorts) = sliding(&seconds[..3], &["A", "B"]);
        let older = AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts);
        let (newer, cohorts) = sliding(&seconds[1..], &["A", "B"]);

        let drawn = AppAttributionHistory::from_view_after(Metric::Cpu, &newer, &cohorts, older);

        let alone = AppAttributionHistory::from_view(Metric::Cpu, &newer, &cohorts);
        assert_eq!(drawn.panel(), alone.panel());
        assert_eq!(hovered(&drawn), hovered(&alone));
    }

    mod when_the_older_chart_drew_other_apps {
        use super::{AppAttributionHistory, Metric, hovered, seconds, sliding};

        #[test]
        fn draws_the_chart_its_own_seconds_draw() {
            // Goal: a chart drawn after the chart of an older View that drew other Apps on lines
            // of their own is the chart the newer View draws alone. Method: the Views of the
            // test above, the older drawing B and A where the newer draws A and B.
            let seconds = seconds();
            let (view, cohorts) = sliding(&seconds[..3], &["B", "A"]);
            let older = AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts);
            let (newer, cohorts) = sliding(&seconds[1..], &["A", "B"]);

            let drawn =
                AppAttributionHistory::from_view_after(Metric::Cpu, &newer, &cohorts, older);

            let alone = AppAttributionHistory::from_view(Metric::Cpu, &newer, &cohorts);
            assert_eq!(drawn.panel(), alone.panel());
            assert_eq!(hovered(&drawn), hovered(&alone));
        }
    }
}

mod when_the_seconds_hold_more_apps_than_the_store_adds_up {
    use super::{AppAttributionHistory, Metric, charted, seconds};

    #[test]
    fn draws_no_chart() {
        // Goal: a chart whose Apps the store could not choose draws nothing rather than lines
        // of the wrong Apps. Method: a View whose cohorts were refused.
        let (view, mut cohorts) = charted(&seconds());
        cohorts.refused = true;

        assert!(
            AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts)
                .panel()
                .is_none()
        );
    }
}

/// What hovering `history` shows at each of the four seconds of `seconds`, line by line.
fn hovered(history: &AppAttributionHistory) -> Vec<Vec<String>> {
    (0..4)
        .map(|index| {
            let index = Some(index);
            let mut shown = vec![
                history.coverage(index),
                history.headline(index),
                history.total_rate(index),
                format!("{:?}", history.reference_reading(index)),
            ];
            shown.extend((0..4).map(|line| history.reading(line, index)));
            shown
        })
        .collect()
}

/// The history of CPU time the worker writes of `charted`'s seconds, the newest selected.
fn worker_history(seconds: &[MachineSecond; 4]) -> Value {
    let observation = |index: usize| worker_observation(&apps_at_observation(&seconds[index]));

    json!({"metric": "cpu", "selected": observation(3), "context_seconds": 900,
        "cohort": [worker_app("A", "41"), worker_app("B", "42")],
        "points": [worker_point(&observation(0), 0), worker_point(&observation(1), 0),
            worker_missing_point(&observation(2)), worker_point(&observation(3), 1)],
        "coverage": {"retention_limited": true, "context_app_count": 3,
            "context_key_bytes": 3, "cache_bytes": 0}})
}

/// A point of `charted`'s readings at `observation`, in segment `segment`.
fn worker_point(observation: &Value, segment: u64) -> Value {
    json!({"observation": observation, "segment": segment.to_string(),
        "elapsed_ns": "1000000000", "source_available": true, "membership_complete": true,
        "ungrouped_members": 0, "usage_refused": 0, "usage_gone": 0,
        "apps": [worker_reading(500_000_000, 50.0), worker_reading(250_000_000, 25.0)],
        "other": {"app_count": 1, "membership_complete": true,
            "cpu_ns": worker_measure(125_000_000), "cpu_percent_one_core": 12.5}})
}

/// A point at `observation`, in the first segment, whose Apps the recording does not keep.
fn worker_missing_point(observation: &Value) -> Value {
    json!({"observation": observation, "segment": "0", "elapsed_ns": null,
        "source_available": false, "membership_complete": null, "ungrouped_members": null,
        "usage_refused": null, "usage_gone": null, "apps": [null, null], "other": null})
}

fn worker_reading(cpu_ns: u64, cpu_percent_one_core: f64) -> Value {
    json!({"members": 1, "membership_complete": true, "cpu_ns": worker_measure(cpu_ns),
        "cpu_percent_one_core": cpu_percent_one_core})
}

mod over_the_loaded_hour {
    use std::sync::Arc;

    use viewer::store::MachineSecond;

    use super::{AppAttributionHistory, Metric, hovered};
    use crate::live_store_app_fixtures::{HOUR_NEWEST, hour, hour_until};

    #[test]
    fn draws_each_older_stretch_where_the_machine_charts_draw_it_with_its_mean() {
        // Goal: the CPU chart by App draws the older part of the loaded hour as the machine
        // charts do, one point for each stretch of ten seconds at the machine charts' point,
        // and shows each App's mean over the stretch rather than one second of it. Method: a View
        // of the hour whose oldest stretch has A use 0.07 s at its first second and 0.27 s at
        // each of the other nine, the chart drawn from it beside the machine CPU chart, and A at
        // the chart's first point.
        let (view, cohorts) = hour();

        let drawn = AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts);

        let machine = crate::live_projection::overview_panels(&view, None)
            .unwrap()
            .into_iter()
            .find(|panel| panel.id == "live-cpu")
            .unwrap();
        let panel = drawn.panel().unwrap();
        assert_eq!(panel.window, machine.window);
        assert_eq!(panel.positions, machine.positions);
        assert_eq!(panel.segments, machine.segments);
        assert_eq!(drawn.amount(1, Some(0)), Some(25.0));
    }

    #[test]
    fn draws_after_the_chart_of_an_older_view_the_chart_it_draws_alone() {
        // Goal: a chart of the hour drawn after the chart of an older View is the chart the
        // newer View draws alone, though seconds the older chart drew one by one now stand in a
        // stretch of the older part. Method: the hour's View, and a View twenty seconds newer,
        // whose older part draws as a stretch at second 9,050 what the older chart drew as that
        // second, and the chart drawn after the older one's beside the chart drawn alone.
        let (view, cohorts) = hour();
        let older = AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts);
        let (newer, cohorts) = hour_until(HOUR_NEWEST + 20);

        let drawn = AppAttributionHistory::from_view_after(Metric::Cpu, &newer, &cohorts, older);

        let alone = AppAttributionHistory::from_view(Metric::Cpu, &newer, &cohorts);
        assert_eq!(drawn.panel(), alone.panel());
        assert_eq!(hovered(&drawn), hovered(&alone));
    }

    #[test]
    fn breaks_its_line_in_the_older_part_where_the_machine_charts_break_theirs() {
        // Goal: the chart breaks its line between two stretches of the older part where the
        // machine charts break theirs, though a stretch always follows the one before it.
        // Method: a View of the hour whose second older second stands in a segment of its own,
        // and the chart's segments beside the machine CPU chart's.
        let (mut view, cohorts) = hour();
        view.earlier[1] = Arc::new(MachineSecond {
            segment: 1,
            ..MachineSecond::clone(&view.earlier[1])
        });

        let drawn = AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts);

        let machine = crate::live_projection::overview_panels(&view, None)
            .unwrap()
            .into_iter()
            .find(|panel| panel.id == "live-cpu")
            .unwrap();
        let panel = drawn.panel().unwrap();
        assert_eq!(machine.segments.as_ref().unwrap()[..3], [0, 1, 2]);
        assert_eq!(panel.segments, machine.segments);
    }

    #[test]
    fn breaks_its_line_at_an_older_point_the_view_holds_no_stretch_for() {
        // Goal: the chart breaks its line across an older point whose stretch the View holds no
        // Apps of, though the machine charts draw the point in the same segment as its
        // neighbours. Method: a View of the hour without the Apps of its third stretch, one of the
        // five the chart draws before its full-detail seconds, and the segments of the chart's
        // points on either side of it.
        let (mut view, cohorts) = hour();
        view.stretches.remove(2);

        let drawn = AppAttributionHistory::from_view(Metric::Cpu, &view, &cohorts);

        let panel = drawn.panel().unwrap();
        let segments = panel.segments.as_ref().unwrap();
        assert_eq!(segments[..2], [0; 2]);
        assert_eq!(segments[2], 1);
    }
}
