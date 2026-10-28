use std::fmt::Write;
use std::sync::Arc;

use serde_json::{Value, json};
use viewer::live_app_response::{KeyKind, RankedApp};
use viewer::store::{Cohorts, MachineSecond, View};

use super::super::{ResidentHistory, decode_cohort};
use super::from_view_labels;
use crate::live_store_app_fixtures::{
    apps_at_observation, charted, on_the_clock, ranked, seconds, worker_app, worker_measure,
    worker_observation,
};
use crate::live_store_fixtures::selection;

#[test]
fn draws_each_second_as_the_workers_history_of_those_seconds_draws_it() {
    // Goal: Overview's chart of resident memory by App drawn from the store's View is the chart
    // the worker's history of resident memory of the same seconds draws, but for the span it is
    // drawn across and what its help says it chose its leaders over. Method: a View of four
    // seconds, whose third holds no Apps and whose fourth does not follow the third, beside the
    // history the worker writes of them, and compare the two charts on the clock.
    let seconds = seconds();
    let worker = ResidentHistory::prepare(&worker_history(&seconds), &selection(&[], &seconds));
    let (view, cohorts) = charted_resident(&seconds);

    let drawn = ResidentHistory::from_view(&view, &cohorts).panel();

    assert!(drawn.is_some());
    assert_eq!(on_the_clock(drawn), on_the_clock(worker.panel()));
}

#[test]
fn describes_each_second_as_the_workers_history_of_those_seconds_describes_it() {
    // Goal: hovering Overview's chart of resident memory drawn from the store's View reads as
    // hovering the chart the worker's history draws. Method: the seconds of the chart test, and
    // what the hover shows at each of them, the missing third included.
    let seconds = seconds();
    let worker = ResidentHistory::prepare(&worker_history(&seconds), &selection(&[], &seconds));
    let (view, cohorts) = charted_resident(&seconds);

    let drawn = ResidentHistory::from_view(&view, &cohorts);

    assert_eq!(hovered(&drawn), hovered(&worker));
}

#[test]
fn names_each_app_as_the_workers_history_names_it() {
    // Goal: the chart drawn from the View labels its Apps as the worker's history labels them,
    // a bundle by the last part of its path. Method: a cohort of a bundle and an executable,
    // labelled from the View and decoded from the cohort the worker writes of them.
    let path = "/Applications/Brave Browser.app";
    let bundle = Arc::new(RankedApp::named(
        KeyKind::Bundle,
        path.as_bytes().to_vec(),
        "Brave Browser".into(),
    ));
    let hex = path.bytes().fold(String::new(), |mut hex, byte| {
        write!(hex, "{byte:02x}").unwrap();
        hex
    });
    let mut worker_bundle = worker_resident_app(path, &hex, 1);
    worker_bundle["key"]["kind"] = json!("bundle");
    let worker = json!([
        worker_bundle,
        worker_resident_app("claude", "636c61756465", 1)
    ]);

    let labels = from_view_labels(&[bundle, ranked("claude")]);

    assert_eq!(labels, decode_cohort(&worker));
}

mod after_the_chart_of_an_older_view {
    use super::{ResidentHistory, hovered, seconds};
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
        let older = ResidentHistory::from_view(&view, &cohorts);
        let (newer, cohorts) = sliding(&seconds[1..], &["A", "B"]);

        let drawn = ResidentHistory::from_view_after(&newer, &cohorts, older);

        let alone = ResidentHistory::from_view(&newer, &cohorts);
        assert_eq!(drawn.panel(), alone.panel());
        assert_eq!(hovered(&drawn), hovered(&alone));
    }

    mod when_the_older_chart_drew_other_apps {
        use super::{ResidentHistory, hovered, seconds, sliding};

        #[test]
        fn draws_the_chart_its_own_seconds_draw() {
            // Goal: a chart drawn after the chart of an older View that drew other Apps on lines
            // of their own is the chart the newer View draws alone. Method: the Views of the
            // test above, the older drawing B and A where the newer draws A and B.
            let seconds = seconds();
            let (view, cohorts) = sliding(&seconds[..3], &["B", "A"]);
            let older = ResidentHistory::from_view(&view, &cohorts);
            let (newer, cohorts) = sliding(&seconds[1..], &["A", "B"]);

            let drawn = ResidentHistory::from_view_after(&newer, &cohorts, older);

            let alone = ResidentHistory::from_view(&newer, &cohorts);
            assert_eq!(drawn.panel(), alone.panel());
            assert_eq!(hovered(&drawn), hovered(&alone));
        }
    }
}

mod when_the_seconds_hold_more_apps_than_the_store_adds_up {
    use super::{ResidentHistory, charted_resident, seconds};

    #[test]
    fn draws_no_chart() {
        // Goal: a chart whose Apps the store could not choose draws nothing rather than lines
        // of the wrong Apps, and says why. Method: a View whose cohorts were refused.
        let (view, mut cohorts) = charted_resident(&seconds());
        cohorts.refused = true;

        let drawn = ResidentHistory::from_view(&view, &cohorts);

        assert!(drawn.panel().is_none());
        assert_eq!(
            drawn.coverage(None),
            "Resident App history unavailable: Resident App history holds more Apps than it adds up"
        );
    }
}

/// The View `charted` draws of `seconds`, and its cohorts, which also draw A and B on lines of
/// their own on the chart of resident memory.
fn charted_resident(seconds: &[MachineSecond; 4]) -> (View, Cohorts) {
    let (view, mut cohorts) = charted(seconds);
    cohorts.resident = cohorts.cpu.clone();

    (view, cohorts)
}

/// What hovering `history` shows at each of the four seconds of `seconds`, line by line.
fn hovered(history: &ResidentHistory) -> Vec<Vec<String>> {
    (0..4)
        .map(|index| {
            let index = Some(index);
            let mut shown = vec![history.coverage(index), history.headline(index)];
            shown.extend((0..3).map(|line| history.reading(line, index)));
            shown
        })
        .collect()
}

/// The history of resident memory the worker writes of `charted_resident`'s seconds, the newest
/// selected.
fn worker_history(seconds: &[MachineSecond; 4]) -> Value {
    let observation = |index: usize| worker_observation(&apps_at_observation(&seconds[index]));

    json!({"selected": observation(3), "context_seconds": 900,
        "cohort": [worker_resident_app("A", "41", 1_500_000_000),
            worker_resident_app("B", "42", 750_000_000)],
        "points": [worker_point(&observation(0), 0), worker_point(&observation(1), 0),
            worker_missing_point(&observation(2)), worker_point(&observation(3), 1)],
        "coverage": {"retention_limited": true, "context_app_count": 3,
            "context_key_bytes": 3, "cache_bytes": 0}})
}

/// The App `name` of the cohort, whose three seconds held `sum_bytes` resident in all.
fn worker_resident_app(name: &str, bytes_hex: &str, sum_bytes: u64) -> Value {
    let mut app = worker_app(name, bytes_hex);
    app["sum_bytes"] = json!(sum_bytes.to_string());
    app["samples"] = json!(3);

    app
}

/// A point of `charted_resident`'s readings at `observation`, in segment `segment`.
fn worker_point(observation: &Value, segment: u64) -> Value {
    json!({"observation": observation, "segment": segment.to_string(),
        "source_available": true, "membership_complete": true, "ungrouped_members": 0,
        "usage_refused": 0, "usage_gone": 0,
        "apps": [worker_measure(500_000_000), worker_measure(250_000_000)],
        "other": {"app_count": 1, "resident_bytes": worker_measure(125_000_000)}})
}

/// A point at `observation`, in the first segment, whose Apps the recording does not keep.
fn worker_missing_point(observation: &Value) -> Value {
    json!({"observation": observation, "segment": "0", "source_available": false,
        "membership_complete": null, "ungrouped_members": null, "usage_refused": null,
        "usage_gone": null, "apps": [null, null], "other": null})
}

mod over_the_loaded_hour {
    use super::ResidentHistory;
    use crate::live_store_app_fixtures::hour;

    #[test]
    fn draws_each_older_stretch_where_the_machine_charts_draw_it_with_its_mean() {
        // Goal: the chart of resident memory by App draws the older part of the loaded hour as
        // the machine charts do, one point for each stretch of ten seconds at the machine charts'
        // point, and shows each App's mean memory over the stretch rather than one second's.
        // Method: a View of the hour whose oldest stretch has A hold 70 MB at its first second
        // and 270 MB at each of the other nine, the chart drawn from it beside the machine CPU
        // chart, and A at the chart's first point.
        let (view, cohorts) = hour();

        let drawn = ResidentHistory::from_view(&view, &cohorts);

        let machine = crate::live_projection::overview_panels(&view, None)
            .unwrap()
            .into_iter()
            .find(|panel| panel.id == "live-cpu")
            .unwrap();
        let panel = drawn.panel().unwrap();
        assert_eq!(panel.window, machine.window);
        assert_eq!(panel.positions, machine.positions);
        assert_eq!(panel.segments, machine.segments);
        assert_eq!(drawn.amount(1, Some(0)), Some(250_000_000));
    }
}
