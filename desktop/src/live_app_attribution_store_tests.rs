use std::sync::Arc;

use serde_json::{Value, json};
use viewer::apps::Stamp;
use viewer::live_app_response::{App, Key, KeyKind, Measure, Observation, RankedApp};
use viewer::store::{AppRow, AppsSecond, MachineSecond, Totals, View};

use super::super::{AppAttributionHistory, Metric};
use crate::live_store_fixtures::{NANOSECONDS_PER_SECOND, reading_at, selection, view};

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

/// Seconds 100, 101, 102 and 104, the last in a segment of its own.
fn seconds() -> [MachineSecond; 4] {
    [
        reading_at(100, 0),
        reading_at(101, 0),
        reading_at(102, 0),
        reading_at(104, 1),
    ]
}

/// The View of `seconds`, at each of which but the third the executables A, B and C used 0.5,
/// 0.25 and 0.125 s of CPU, the third holding no Apps, and whose CPU chart draws A and B on
/// lines of their own.
fn charted(seconds: &[MachineSecond; 4]) -> View {
    let mut view = view(1, &[], seconds);
    let used = [("A", 500_000_000), ("B", 250_000_000), ("C", 125_000_000)];
    for (index, consecutive) in [(0, false), (1, true), (3, false)] {
        view.apps[index] = Arc::new(AppsSecond {
            consecutive,
            ..apps_at(&seconds[index], &used)
        });
    }
    view.apps[2] = Arc::new(AppsSecond {
        observation: Some(apps_at_observation(&seconds[2])),
        consecutive: true,
        ..AppsSecond::default()
    });
    view.cohorts.seconds = seconds.len();
    view.cohorts.cpu = vec![ranked("A"), ranked("B")];

    view
}

/// The executable `name`, as a chart's cohort names it.
fn ranked(name: &str) -> Arc<RankedApp> {
    Arc::new(RankedApp {
        kind: KeyKind::Executable,
        bytes: name.as_bytes().to_vec(),
        display_name: name.into(),
    })
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

fn worker_measure(known: u64) -> Value {
    json!({"known": known.to_string(), "measured_members": 1, "unavailable_members": 0,
        "overflowed": false})
}

fn worker_app(name: &str, bytes_hex: &str) -> Value {
    json!({"key": {"kind": "executable", "bytes_hex": bytes_hex, "display": name}})
}

fn worker_observation(observation: &Observation) -> Value {
    let pair = |words: [u64; 2]| [words[0].to_string(), words[1].to_string()];
    let text = |value: Option<u64>| value.map(|value| value.to_string());

    json!({"recording": pair(observation.recording),
        "stamp": {"boot": pair(observation.stamp.boot), "run": pair(observation.stamp.run),
            "publication": observation.stamp.publication.to_string(),
            "continuous_ns": text(observation.stamp.continuous_ns),
            "awake_ns": text(observation.stamp.awake_ns)},
        "run_tick": observation.run_tick.to_string(),
        "wall_ns": text(observation.wall_ns)})
}

/// The Apps of `second`, each of `apps` an executable of that name that spent that many
/// nanoseconds of CPU time in the second that ends there and nothing else, every process of
/// the second found. `apps` is in key order.
pub fn apps_at(second: &MachineSecond, apps: &[(&str, u64)]) -> AppsSecond {
    let rows: Vec<AppRow> = apps
        .iter()
        .map(|(name, cpu_ns)| apps_at_row(name, *cpu_ns))
        .collect();

    AppsSecond {
        missing: None,
        observation: Some(apps_at_observation(second)),
        consecutive: true,
        elapsed_ns: Some(NANOSECONDS_PER_SECOND),
        membership_complete: true,
        ungrouped_members: 0,
        usage_refused: 0,
        usage_gone: 0,
        totals: Totals::of(&rows),
        apps: rows,
    }
}

/// The tick of `second`, of the run and boot the trend points of `selection` name.
pub fn apps_at_observation(second: &MachineSecond) -> Observation {
    Observation {
        recording: [5, 6],
        stamp: Stamp {
            boot: [3, 4],
            run: [1, 2],
            publication: second.publication,
            continuous_ns: second.continuous_ns,
            awake_ns: second.awake_ns,
        },
        run_tick: second.run_tick,
        wall_ns: Some(second.wall_ns),
    }
}

/// The App of one process, the executable `name`, that spent `cpu_ns` of CPU time.
fn apps_at_row(name: &str, cpu_ns: u64) -> AppRow {
    let measure = |known| Measure {
        known,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    let app = App {
        key: Key {
            kind: KeyKind::Executable,
            bytes: name.as_bytes().to_vec(),
            display: name.into(),
        },
        display_name: name.into(),
        members: 1,
        live_members: 1,
        membership_complete: true,
        cpu_ns: measure(cpu_ns),
        energy_nj: measure(0),
        gpu_ns: measure(0),
        disk_read_bytes: measure(0),
        disk_written_bytes: measure(0),
        disk_io_bytes: measure(0),
        resident_bytes: measure(0),
        footprint_bytes: measure(0),
    };
    let ranked = Arc::new(RankedApp {
        kind: KeyKind::Executable,
        bytes: app.key.bytes.clone(),
        display_name: app.display_name.clone(),
    });

    AppRow::new(&app, ranked)
}
