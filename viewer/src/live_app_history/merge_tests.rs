use std::sync::Arc;

use persistence::Error;
use serde_json::Value;

use super::{App, CompactMeasure, History, KeyKind, Merge, Observation, Target, TracePoint};
use crate::Cancellation;
use crate::apps::Stamp;
use crate::live_app_response::Measure;

#[test]
fn prefers_the_entire_live_point_without_summing_the_saved_overlap() {
    // Goal: scheduled saved totals cannot erase verified live contributions or mix denominators.
    // Method: merge one saved endpoint with a live endpoint carrying independent totals and time.
    let mut history = History::new();
    history.push(live(2), 0);
    let saved = [trace(2, 7)];

    let response = response(&history, &saved, &observation(2)).unwrap();

    assert_eq!(response["points"].as_array().unwrap().len(), 1);
    assert_eq!(response["points"][0]["cpu_ns"]["known"], "42");
    assert_eq!(response["points"][0]["elapsed_ns"], "1000000000");
    assert_eq!(response["points"][0]["cpu_percent_one_core"], 0.000_004_2);
    assert_eq!(response["points"][0]["disk_read_bytes"]["known"], "42");
    assert_eq!(response["points"][0]["disk_written_bytes"]["known"], "42");
    assert_eq!(response["points"][0]["disk_io_bytes"]["known"], "42");
    assert_eq!(response["points"][0]["disk_read_bps"], 42.0);
    assert_eq!(response["points"][0]["disk_written_bps"], 42.0);
    assert_eq!(response["points"][0]["disk_io_bps"], 42.0);
}

#[test]
fn rejects_conflicting_clocks_for_the_same_saved_and_live_publication() {
    // Goal: an overlap cannot silently replace a structurally different observation.
    // Method: give one exact run/publication two different continuous timestamps.
    let mut history = History::new();
    history.push(live(2), 0);
    let mut saved = trace(2, 7);
    saved.observation.stamp.continuous_ns = Some(9_000_000_000);

    let result = response(&history, &[saved], &observation(2));

    assert!(matches!(result, Err(Error::Bound(_))));
}

#[test]
fn rejects_duplicate_saved_observations_instead_of_double_counting_them() {
    // Goal: repeated source rows are refused even when their values agree.
    // Method: supply two identical committed facts and no live observations.
    let history = History::new();
    let saved = [trace(2, 7), trace(2, 7)];

    let result = response(&history, &saved, &observation(2));

    assert!(matches!(result, Err(Error::Bound(_))));
}

#[test]
fn joins_adjacent_saved_and_live_points_with_one_recomputed_segment() {
    // Goal: independent cache segment identifiers do not introduce a false chart gap.
    // Method: merge saved publication one and consecutive live publication two.
    let mut history = History::new();
    history.push(live(2), 0);
    let saved = [trace(1, 7)];

    let response = response(&history, &saved, &observation(2)).unwrap();

    assert_eq!(response["points"].as_array().unwrap().len(), 2);
    assert_eq!(response["points"][0]["segment"], "0");
    assert_eq!(response["points"][1]["segment"], "0");
}

#[test]
fn breaks_the_merged_segment_across_a_missing_publication() {
    // Goal: a chart cannot bridge a missing observation after combining caches.
    // Method: leave publication two absent between saved one and live three.
    let mut history = History::new();
    history.push(live(3), 0);
    let saved = [trace(1, 7)];

    let response = response(&history, &saved, &observation(3)).unwrap();

    assert_eq!(response["points"][0]["segment"], "0");
    assert_eq!(response["points"][1]["segment"], "1");
}

#[test]
fn keeps_repeated_wall_times_as_distinct_observations_with_a_segment_break() {
    // Goal: repeated clock seconds neither erase an observation nor imply continuity.
    // Method: give consecutive publications the same wall timestamp.
    let history = History::new();
    let first = trace(1, 7);
    let mut second = trace(2, 9);
    second.observation.wall_ns = first.observation.wall_ns;

    let response = response(&history, &[first, second], &observation(2)).unwrap();

    assert_eq!(response["points"].as_array().unwrap().len(), 2);
    assert_eq!(
        response["points"][0]["observation"]["stamp"]["publication"],
        "1"
    );
    assert_eq!(
        response["points"][1]["observation"]["stamp"]["publication"],
        "2"
    );
    assert_eq!(response["points"][1]["segment"], "1");
}

#[test]
fn retains_a_null_point_when_the_target_has_no_attributed_members() {
    // Goal: absence of attributed members cannot become measured zero in the restored trace.
    // Method: place an observation with no target totals between two captured target facts.
    let history = History::new();
    let absent = TracePoint::from_live(&observation(2), Some(1_000_000_000), None);
    let saved = [trace(1, 7), absent, trace(3, 9)];

    let response = response(&history, &saved, &observation(3)).unwrap();

    assert_eq!(response["points"].as_array().unwrap().len(), 3);
    assert_eq!(response["points"][1]["members"], Value::Null);
    assert_eq!(response["points"][1]["resident_bytes"], Value::Null);
    assert_eq!(response["points"][1]["cpu_percent_one_core"], Value::Null);
    assert_eq!(response["points"][2]["resident_bytes"]["known"], "9");
}

macro_rules! broken_interval {
    ($name:ident, $field:ident, $value:expr) => {
        #[test]
        fn $name() {
            // Goal: incompatible endpoint clocks or identities cannot share a chart segment.
            // Method: change one fact of a consecutive saved pair and inspect the segment.
            let history = History::new();
            let first = trace(1, 7);
            let mut second = trace(2, 9);
            second.observation.stamp.$field = $value;

            let response = response(&history, &[first, second], &observation(2)).unwrap();

            assert_eq!(response["points"][1]["segment"], "1");
        }
    };
}

broken_interval!(breaks_at_a_different_run, run, [9, 10]);
broken_interval!(breaks_at_a_different_boot, boot, [9, 10]);
broken_interval!(breaks_at_a_missing_continuous_clock, continuous_ns, None);
broken_interval!(
    breaks_at_a_nonincreasing_continuous_clock,
    continuous_ns,
    Some(1_000_000_000)
);
broken_interval!(breaks_at_a_missing_awake_clock, awake_ns, None);
broken_interval!(
    breaks_at_a_backwards_awake_clock,
    awake_ns,
    Some(999_999_999)
);

#[test]
fn rejects_more_saved_points_than_the_response_observation_bound() {
    // Goal: repeated wall times cannot grow the merged projection without a bound.
    // Method: supply one more point than the shared context permits.
    let history = History::new();
    let saved = vec![trace(2, 7); super::OBSERVATIONS_MAX + 1];

    let result = response(&history, &saved, &observation(2));

    assert!(matches!(result, Err(Error::Bound(_))));
}

#[test]
fn emits_a_null_gap_for_a_known_context_observation_without_app_facts() {
    // Goal: a known App observation cannot disappear when neither source retains its facts.
    // Method: provide scalar context for one publication that neither App source retains.
    let history = History::new();
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let observed = observation(2);
    let context = [crate::cpu_history_sample::Identity {
        run: collector::recording::identity_text(collector::ring::Identity(observed.stamp.run))
            .to_string(),
        boot: collector::recording::identity_text(collector::ring::Identity(observed.stamp.boot))
            .to_string(),
        observation: crate::session::Observation {
            window: 0,
            publication: 2,
            run_tick: 2,
            wall: observed.wall_ns.unwrap(),
            continuous: observed.stamp.continuous_ns,
        },
    }];

    let response = history
        .response_merged(&Merge {
            selected: &observed,
            target: &Target {
                kind: KeyKind::Executable,
                bytes: b"app",
            },
            saved: &[],
            context: &context,
            operation: &operation,
        })
        .unwrap();

    assert_eq!(response["points"].as_array().unwrap().len(), 1);
    assert_eq!(response["points"][0]["source_available"], false);
    assert_eq!(response["points"][0]["disk_read_bytes"], Value::Null);
    assert_eq!(response["points"][0]["disk_written_bps"], Value::Null);
    assert_eq!(response["points"][0]["disk_io_bps"], Value::Null);
    assert_eq!(response["points"][0]["members"], Value::Null);
    assert_eq!(response["coverage"]["retention_limited"], true);
}

#[test]
fn retains_the_entire_first_second_of_merged_history() {
    // Goal: the inclusive first second keeps fractional saved observations at the 15-minute edge.
    // Method: merge saved endpoints immediately before and inside that second with a live endpoint.
    let mut history = History::new();
    history.push(live(900), 0);
    let mut first = trace(1, 7);
    first.observation.wall_ns = Some(1_001_900_000_000);
    let mut older = trace(0, 9);
    older.observation.wall_ns = Some(1_000_999_999_999);

    let response = response(&history, &[older, first], &observation(900)).unwrap();

    assert_eq!(response["points"].as_array().unwrap().len(), 2);
    assert_eq!(
        response["points"][0]["observation"]["stamp"]["publication"],
        "1"
    );
    assert_eq!(
        response["points"][0]["observation"]["wall_ns"],
        "1001900000000"
    );
    assert_eq!(response["points"][0]["resident_bytes"]["known"], "7");
    assert_eq!(response["points"][0]["source_available"], true);
}

fn response(
    history: &History,
    saved: &[TracePoint],
    selected: &Observation,
) -> persistence::Result<Value> {
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();

    history.response_merged(&Merge {
        selected,
        target: &Target {
            kind: KeyKind::Executable,
            bytes: b"app",
        },
        saved,
        context: &[],
        operation: &operation,
    })
}

fn trace(publication: u64, amount: u64) -> TracePoint {
    let measure = CompactMeasure::capture(&Measure {
        known: amount,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    });
    let app = App {
        key: super::compact::capture_key(
            &crate::live_app_response::Key {
                kind: KeyKind::Executable,
                bytes: b"app".to_vec(),
                display: String::new(),
            },
            &[],
            &mut 0,
        ),
        members: 1,
        membership_complete: false,
        cpu_ns: measure,
        energy_nj: measure,
        disk_read_bytes: measure,
        disk_written_bytes: measure,
        disk_io_bytes: measure,
        resident_bytes: measure,
        footprint_bytes: measure,
    };

    TracePoint::from_live(&observation(publication), Some(500_000_000), Some(&app))
}

fn live(publication: u64) -> Box<super::Point> {
    let mut point = super::tests::point(publication, 1);
    point.observation = observation(publication);
    point.apps[0].key = super::compact::capture_key(
        &crate::live_app_response::Key {
            kind: KeyKind::Executable,
            bytes: b"app".to_vec(),
            display: String::new(),
        },
        &[],
        &mut 0,
    );

    point
}

fn observation(publication: u64) -> Observation {
    Observation {
        recording: [1, 2],
        stamp: Stamp {
            boot: [3, 4],
            run: [5, 6],
            publication,
            continuous_ns: Some(publication * 1_000_000_000),
            awake_ns: Some(publication * 1_000_000_000),
        },
        run_tick: publication,
        wall_ns: Some((1_000 + publication) * 1_000_000_000),
    }
}
