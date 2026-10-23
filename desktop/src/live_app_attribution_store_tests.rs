use serde_json::{Value, json};
use viewer::store::MachineSecond;

use super::super::{AppAttributionHistory, Metric};
use crate::live_store_app_fixtures::{
    apps_at_observation, charted, seconds, worker_app, worker_measure, worker_observation,
};
use crate::live_store_fixtures::selection;

#[test]
fn draws_each_second_as_the_workers_history_of_those_seconds_draws_it() {
    // Goal: Overview's CPU chart drawn from the store's View is the chart the worker's App
    // history of the same seconds draws. Method: a View of four seconds, whose third holds no
    // Apps and whose fourth does not follow the third, beside the history the worker writes of
    // them, and compare the two charts.
    let seconds = seconds();
    let worker = AppAttributionHistory::prepare(
        Metric::Cpu,
        &worker_history(&seconds),
        &selection(&[], &seconds),
    );

    let drawn = AppAttributionHistory::from_view(Metric::Cpu, &charted(&seconds)).panel();

    assert!(drawn.is_some());
    assert_eq!(drawn, worker.panel());
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

    let drawn = AppAttributionHistory::from_view(Metric::Cpu, &charted(&seconds));

    assert_eq!(hovered(&drawn), hovered(&worker));
}

mod when_the_seconds_hold_more_apps_than_the_store_adds_up {
    use super::{AppAttributionHistory, Metric, charted, seconds};

    #[test]
    fn draws_no_chart() {
        // Goal: a chart whose Apps the store could not choose draws nothing rather than lines
        // of the wrong Apps. Method: a View whose cohorts the store refused.
        let mut view = charted(&seconds());
        view.cohorts.refused = true;

        assert!(
            AppAttributionHistory::from_view(Metric::Cpu, &view)
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
