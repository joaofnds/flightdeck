use std::sync::Arc;

use super::{App, BYTES_MAX, History, KeyKind, Measure, Observation, Point, Target};
use crate::Cancellation;
use crate::apps::Stamp;
use crate::session::OBSERVATIONS_MAX;

#[test]
fn shortens_dense_history_at_its_byte_limit_and_reports_the_loss() {
    // Large valid bundle keys force byte eviction while all observations remain within 15 minutes.
    let mut history = History::new();
    for publication in 1..=9 {
        history.push(point(publication, 1024), 0);
    }
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let key = bundle_key(0);

    let response = history
        .response(
            &observation(9),
            &Target {
                kind: KeyKind::Bundle,
                bytes: &key,
            },
            &operation,
        )
        .unwrap();

    assert!(history.bytes <= BYTES_MAX);
    assert!(history.points.len() < 9);
    assert!(history.points.len() > 1);
    assert_eq!(response["coverage"]["retention_limited"], true);
    let points = response["points"].as_array().unwrap();
    assert_eq!(points.len(), history.points.len());
    assert_eq!(
        points.last().unwrap()["observation"]["stamp"]["publication"],
        "9"
    );
    assert_eq!(points.last().unwrap()["resident_bytes"]["known"], "42");
}

#[test]
fn bounds_repeated_clock_observations_and_their_serialized_response() {
    // Repeated wall times cannot grow history or its response beyond their independent limits.
    let mut history = History::new();
    for publication in 1..=u64::try_from(OBSERVATIONS_MAX + 1).unwrap() {
        history.push(point(publication, 1), 0);
    }
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let key = bundle_key(0);
    let selected = observation(u64::try_from(OBSERVATIONS_MAX + 1).unwrap());

    let response = history
        .response(
            &selected,
            &Target {
                kind: KeyKind::Bundle,
                bytes: &key,
            },
            &operation,
        )
        .unwrap();
    let encoded = serde_json::to_vec(&response).unwrap();

    assert_eq!(history.points.len(), OBSERVATIONS_MAX);
    assert!(history.bytes <= BYTES_MAX);
    assert_eq!(response["coverage"]["retention_limited"], true);
    assert_eq!(
        response["points"][0]["observation"]["stamp"]["publication"],
        "2"
    );
    assert!(encoded.len() <= crate::live_app_response::RESPONSE_BYTES_MAX);
}

#[test]
fn emits_a_gap_when_an_app_is_absent_in_an_observed_publication() {
    // App absence is a missing metric, so a chart cannot connect through a fabricated zero.
    let mut history = History::new();
    history.push(point(1, 1), 0);
    history.push(point(2, 0), 0);
    history.push(point(3, 1), 0);
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let key = bundle_key(0);

    let response = history
        .response(
            &observation(3),
            &Target {
                kind: KeyKind::Bundle,
                bytes: &key,
            },
            &operation,
        )
        .unwrap();

    assert_eq!(response["points"][0]["resident_bytes"]["known"], "42");
    assert_eq!(response["points"][1]["members"], serde_json::Value::Null);
    assert_eq!(
        response["points"][1]["resident_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(
        response["points"][1]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(response["points"][2]["resident_bytes"]["known"], "42");
}

#[test]
fn refuses_app_history_after_its_operation_is_canceled() {
    // Cancellation applies to the App projection even when it needs no database query.
    let mut history = History::new();
    history.push(point(1, 1), 0);
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let key = bundle_key(0);
    cancellation.cancel().unwrap();

    let result = history.response(
        &observation(1),
        &Target {
            kind: KeyKind::Bundle,
            bytes: &key,
        },
        &operation,
    );

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
}

fn point(publication: u64, app_count: usize) -> Box<Point> {
    let apps = (0..app_count)
        .map(|index| App {
            kind: KeyKind::Bundle,
            key: bundle_key(index),
            members: 1,
            membership_complete: false,
            cpu_ns: measure(),
            energy_nj: measure(),
            resident_bytes: measure(),
            footprint_bytes: measure(),
        })
        .collect();

    Box::new(Point {
        observation: observation(publication),
        segment: 0,
        elapsed_ns: Some(1_000_000_000),
        apps,
    })
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
        wall_ns: Some(1_000_000_000_000),
    }
}

fn bundle_key(index: usize) -> Vec<u8> {
    let mut bytes = format!("/Applications/{index:04}").into_bytes();
    bytes.resize(3946, b'x');
    bytes.extend_from_slice(b".app");

    bytes
}

fn measure() -> Measure {
    Measure {
        known: 42,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    }
}
