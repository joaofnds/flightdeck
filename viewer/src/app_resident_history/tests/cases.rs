use persistence::Error;
use serde_json::{Value, json};

use super::fixture::{Resident, ResidentFixture};
use crate::LiveSession;

const KEYS: [&[u8]; 10] = [
    b"/Apps/A.app",
    b"/Apps/B.app",
    b"/Apps/C.app",
    b"/Apps/D.app",
    b"/Apps/E.app",
    b"/Apps/F.app",
    b"/Apps/G.app",
    b"/Apps/H.app",
    b"/Apps/I.app",
    b"/Apps/J.app",
];

#[test]
fn resident_history_orders_fractional_means_before_integer_ties() {
    // Goal: exact interval means distinguish B's 3/2 bytes from A's one byte.
    // Method: two real publications would tie after integer division and incorrectly prefer A.
    let mut fixture = ResidentFixture::new();
    fixture.publish(&rows(&[Some(1), Some(1)]));
    fixture.publish(&rows(&[Some(1), Some(2)]));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let response = session.app_resident_history().unwrap();

    assert_eq!(response["cohort"][0]["key"]["display"], "/Apps/B.app");
    assert_eq!(response["cohort"][0]["sum_bytes"], "3");
    assert_eq!(response["cohort"][0]["samples"], 2);
    assert_eq!(response["cohort"][1]["key"]["display"], "/Apps/A.app");
}

#[test]
fn resident_history_keeps_interval_leaders_when_the_current_leader_changes() {
    // Nine real captured keys make the current leader differ from the displayed interval cohort.
    let mut fixture = ResidentFixture::new();
    fixture.publish(&rows(&[
        Some(199),
        Some(199),
        Some(199),
        Some(199),
        Some(199),
        Some(199),
        Some(199),
        Some(199),
        Some(0),
    ]));
    fixture.publish(&rows(&[
        Some(1),
        Some(1),
        Some(1),
        Some(1),
        Some(1),
        Some(1),
        Some(1),
        Some(1),
        Some(100),
    ]));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let response = session.app_resident_history().unwrap();

    assert_eq!(
        cohort_keys(&response),
        json!([
            "/Apps/A.app",
            "/Apps/B.app",
            "/Apps/C.app",
            "/Apps/D.app",
            "/Apps/E.app",
            "/Apps/F.app",
            "/Apps/G.app",
            "/Apps/H.app"
        ])
    );
    assert_eq!(response["cohort"][0]["sum_bytes"], "200");
    assert_eq!(response["cohort"][0]["samples"], 2);
    assert_eq!(
        response["points"][0]["other"]["resident_bytes"]["known"],
        "0"
    );
    assert_eq!(
        response["points"][1]["other"]["resident_bytes"]["known"],
        "100"
    );
}

#[test]
fn resident_history_compares_exact_means_and_skips_unavailable_denominators() {
    // One unsigned byte above the f64 integer boundary must win despite its unavailable first tick.
    let mut fixture = ResidentFixture::new();
    let mut values = vec![Some(9_007_199_254_740_992); 9];
    values[8] = None;
    fixture.publish(&rows(&values));
    values[8] = Some(9_007_199_254_740_993);
    fixture.publish(&rows(&values));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let response = session.app_resident_history().unwrap();

    assert_eq!(response["cohort"][0]["key"]["display"], "/Apps/I.app");
    assert_eq!(response["cohort"][0]["sum_bytes"], "9007199254740993");
    assert_eq!(response["cohort"][0]["samples"], 1);
    assert_eq!(response["points"][0]["apps"][0]["measured_members"], 0);
    assert_eq!(response["points"][0]["apps"][0]["unavailable_members"], 1);
    assert_eq!(
        response["points"][1]["apps"][0]["known"],
        "9007199254740993"
    );
    assert_eq!(
        response["points"][1]["other"]["resident_bytes"]["known"],
        "9007199254740992"
    );
}

#[test]
fn resident_history_preserves_distinct_observations_in_the_same_wall_second() {
    // The selected sibling cannot overwrite an earlier gauge or connect through a repeated clock.
    let mut fixture = ResidentFixture::new();
    fixture.publish_at(&rows(&[Some(10)]), 1_000_000_000_000);
    fixture.publish_at(&rows(&[Some(90)]), 1_000_000_000_000);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1000, 0).unwrap();

    let response = session.app_resident_history().unwrap();

    assert_eq!(response["selected"]["stamp"]["publication"], "1");
    assert_eq!(response["points"].as_array().unwrap().len(), 2);
    assert_eq!(
        response["points"][0]["observation"]["stamp"]["publication"],
        "1"
    );
    assert_eq!(
        response["points"][1]["observation"]["stamp"]["publication"],
        "2"
    );
    assert_eq!(response["points"][0]["apps"][0]["known"], "10");
    assert_eq!(response["points"][1]["apps"][0]["known"], "90");
    assert_eq!(response["points"][1]["segment"], "1");
    assert_eq!(
        response["points"][0]["other"]["resident_bytes"],
        Value::Null
    );
}

#[test]
fn resident_history_keeps_partial_residents_eligible_and_refused_readings_unavailable() {
    // Two members share A: one unknown member cannot erase the known subtotal or become zero.
    let mut fixture = ResidentFixture::new();
    fixture.publish(&[
        Resident {
            key: KEYS[0],
            bytes: Some(10),
        },
        Resident {
            key: KEYS[0],
            bytes: None,
        },
        Resident {
            key: KEYS[1],
            bytes: Some(2),
        },
    ]);
    fixture.publish(&[
        Resident {
            key: KEYS[0],
            bytes: None,
        },
        Resident {
            key: KEYS[0],
            bytes: None,
        },
        Resident {
            key: KEYS[1],
            bytes: Some(2),
        },
    ]);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let response = session.app_resident_history().unwrap();

    assert_eq!(response["cohort"][0]["key"]["display"], "/Apps/A.app");
    assert_eq!(response["cohort"][0]["sum_bytes"], "10");
    assert_eq!(response["cohort"][0]["samples"], 1);
    assert_eq!(
        response["points"][0]["apps"][0],
        json!({"known":"10",
        "measured_members":1,"unavailable_members":1,"overflowed":false})
    );
    assert_eq!(
        response["points"][1]["apps"][0],
        json!({"known":"0",
        "measured_members":0,"unavailable_members":2,"overflowed":false})
    );
}

#[test]
fn resident_history_reports_overflow_when_other_adds_large_captured_apps() {
    // Ten maximal gauges choose eight deterministic ties; Other must not wrap the final pair.
    let mut fixture = ResidentFixture::new();
    fixture.publish(&rows(&[Some(u64::MAX); 10]));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let response = session.app_resident_history().unwrap();

    assert_eq!(response["cohort"].as_array().unwrap().len(), 8);
    assert_eq!(response["cohort"][7]["key"]["display"], "/Apps/H.app");
    assert_eq!(response["points"][0]["other"]["app_count"], 2);
    assert_eq!(
        response["points"][0]["other"]["resident_bytes"]["overflowed"],
        true
    );
    assert_eq!(
        response["points"][0]["other"]["resident_bytes"]["measured_members"],
        2
    );
}

fn rows(values: &[Option<u64>]) -> Vec<Resident<'static>> {
    assert!(values.len() <= KEYS.len());
    values
        .iter()
        .enumerate()
        .map(|(index, bytes)| Resident {
            key: KEYS[index],
            bytes: *bytes,
        })
        .collect()
}

fn cohort_keys(response: &Value) -> Value {
    response["cohort"]
        .as_array()
        .unwrap()
        .iter()
        .map(|app| app["key"]["display"].clone())
        .collect()
}

#[test]
fn resident_history_accepts_the_bounded_context_union_across_app_churn() {
    // Each observation fits the population bound; together they reach exactly 24,000 keys.
    let fixture = churned(24_000);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let response = session.app_resident_history().unwrap();

    assert_eq!(response["coverage"]["context_app_count"], 24_000);
    assert_eq!(response["points"].as_array().unwrap().len(), 2);
}

#[test]
fn resident_history_refuses_context_churn_above_the_population_bound() {
    // A valid second observation cannot add an unbounded context union behind a top-eight result.
    let fixture = churned(24_001);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let result = session.app_resident_history();

    assert!(matches!(result, Err(Error::Bound(_))));
}

fn churned(count: usize) -> ResidentFixture {
    assert!(count <= 24_001);
    let keys: Vec<_> = (0..count)
        .map(|index| format!("/Apps/{index:05}.app").into_bytes())
        .collect();
    let rows: Vec<_> = keys
        .iter()
        .map(|key| Resident {
            key,
            bytes: Some(1),
        })
        .collect();
    let mut fixture = ResidentFixture::with_capacity(
        12_001,
        collector::ring::DictionaryCapacity {
            entries: 24_001,
            arena_bytes: 2 * 1024 * 1024,
        },
    );
    fixture.publish(&rows[..12_000]);
    fixture.publish(&rows[12_000..]);

    fixture
}

#[test]
fn resident_history_preserves_context_gaps_after_compact_facts_are_evicted() {
    // Byte pressure may evict App facts, but cannot remove their observed times or invent zeros.
    let (fixture, session, first) = dense_context();

    let response = session.app_resident_history().unwrap();

    assert_eq!(response["points"].as_array().unwrap().len(), 400);
    assert_eq!(response["points"][0]["observation"], first);
    assert_eq!(
        response["points"][0]["observation"]["stamp"]["publication"],
        "1"
    );
    assert_eq!(response["points"][0]["source_available"], false);
    assert_eq!(response["points"][0]["membership_complete"], Value::Null);
    assert_eq!(response["points"][0]["usage_refused"], Value::Null);
    assert_eq!(
        response["points"][0]["apps"],
        json!([null, null, null, null, null, null, null, null])
    );
    assert_eq!(response["points"][0]["other"], Value::Null);
    assert_eq!(response["points"][399]["source_available"], true);
    assert_eq!(response["points"][399]["apps"][0]["known"], "1");
    assert_eq!(response["coverage"]["retention_limited"], true);
    drop(session);
    drop(fixture);
}

fn dense_context() -> (ResidentFixture, LiveSession, Value) {
    let keys: Vec<_> = (0..2000)
        .map(|index| format!("/Apps/{index:04}.app").into_bytes())
        .collect();
    let residents: Vec<_> = keys
        .iter()
        .map(|key| Resident {
            key,
            bytes: Some(1),
        })
        .collect();
    let mut fixture = ResidentFixture::with_capacity(
        2000,
        collector::ring::DictionaryCapacity {
            entries: 4096,
            arena_bytes: 256_000,
        },
    );
    fixture.publish(&residents);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();
    let first = session
        .app_detail(
            crate::live_app_response::KeyKind::Bundle,
            b"/Apps/0000.app",
            0,
        )
        .unwrap()["selected"]
        .clone();
    for _observation in 1..400 {
        fixture.publish(&residents);
        session.poll().unwrap();
    }
    session.follow_latest().unwrap();

    (fixture, session, first)
}
