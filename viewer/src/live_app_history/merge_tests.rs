use std::sync::Arc;

use persistence::Error;
use serde_json::Value;

use super::{History, KeyKind, Merge, Observation, Target, TracePoint};
use crate::Cancellation;
use crate::apps::{AppTotals, Stamp, Subtotal};

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
fn breaks_the_merged_segment_across_a_missing_publication() {
    // Goal: a chart cannot bridge a missing observation after combining caches.
    // Method: leave publication two absent between saved one and saved three.
    let history = History::new();
    let saved = [trace(1, 7), trace(3, 9)];

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
    let absent = TracePoint::from_totals(&observation(2), Some(1_000_000_000), None);
    let saved = [trace(1, 7), absent, trace(3, 9)];

    let response = response(&history, &saved, &observation(3)).unwrap();

    assert_eq!(response["points"].as_array().unwrap().len(), 3);
    assert_eq!(response["points"][1]["members"], Value::Null);
    assert_eq!(response["points"][1]["resident_bytes"], Value::Null);
    assert_eq!(response["points"][1]["cpu_percent_one_core"], Value::Null);
    assert_eq!(response["points"][2]["resident_bytes"]["known"], 9);
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
fn rejects_more_merged_points_than_the_response_observation_bound() {
    // Goal: saved points and context points that differ cannot together grow the merged
    // projection past the bound each keeps alone.
    // Method: supply as many saved points as the bound permits and one context point more, all
    // in one wall second.
    let history = History::new();
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let observed = observation(2);
    let bound = u64::try_from(super::OBSERVATIONS_MAX).unwrap();
    let saved: Vec<TracePoint> = (1..=bound)
        .map(|publication| {
            let mut point = trace(publication, 7);
            point.observation.wall_ns = observed.wall_ns;
            point
        })
        .collect();
    let context = [crate::cpu_history_sample::Identity {
        run: collector::recording::identity_text(collector::ring::Identity(observed.stamp.run))
            .to_string(),
        boot: collector::recording::identity_text(collector::ring::Identity(observed.stamp.boot))
            .to_string(),
        observation: crate::session::Observation {
            window: 0,
            publication: bound + 1,
            run_tick: bound + 1,
            wall: observed.wall_ns.unwrap(),
            continuous: observed.stamp.continuous_ns,
        },
    }];

    let result = history.response_merged(&Merge {
        selected: &crate::live_app_response::Selected::ending_at(observed).unwrap(),
        target: &Target {
            kind: KeyKind::Executable,
            bytes: b"app",
        },
        saved: &saved,
        context: &context,
        operation: &operation,
    });

    assert!(matches!(
        result,
        Err(Error::Bound("App history merged observations"))
    ));
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
            selected: &crate::live_app_response::Selected::ending_at(observed).unwrap(),
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
    // Goal: the inclusive first second keeps fractional saved observations at the edge of the
    // charted seconds, the lead-in before the 15 minutes. Method: merge saved endpoints
    // immediately before and inside that second.
    let history = History::new();
    let mut first = trace(1, 7);
    first.observation.wall_ns = Some(941_900_000_000);
    let mut older = trace(0, 9);
    older.observation.wall_ns = Some(940_999_999_999);

    let response = response(&history, &[older, first], &observation(900)).unwrap();

    assert_eq!(response["points"].as_array().unwrap().len(), 1);
    assert_eq!(
        response["points"][0]["observation"]["stamp"]["publication"],
        "1"
    );
    assert_eq!(
        response["points"][0]["observation"]["wall_ns"],
        "941900000000"
    );
    assert_eq!(response["points"][0]["resident_bytes"]["known"], 7);
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
        selected: &crate::live_app_response::Selected::ending_at(*selected).unwrap(),
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
    let subtotal = || Subtotal {
        known: amount,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    let totals = AppTotals {
        members: 1,
        cpu_ns: subtotal(),
        energy_nj: subtotal(),
        gpu_ns: subtotal(),
        disk_read_bytes: subtotal(),
        disk_written_bytes: subtotal(),
        disk_io_bytes: subtotal(),
        resident_bytes: subtotal(),
        footprint_bytes: subtotal(),
        ..AppTotals::default()
    };

    TracePoint::from_totals(&observation(publication), Some(500_000_000), Some(&totals))
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
