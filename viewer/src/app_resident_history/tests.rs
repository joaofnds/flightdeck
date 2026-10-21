use collector::processes::Usage;

use crate::LiveSession;
use crate::live::identity_fixture::Fixture;

#[path = "tests/cache.rs"]
mod cache;
#[path = "tests/cases.rs"]
mod cases;
#[path = "tests/fixture.rs"]
mod fixture;
#[path = "tests/sealed.rs"]
mod sealed;

#[test]
fn resident_history_keeps_exact_gauge_bytes_without_a_counter_baseline() {
    // Capture one real publication: a resident gauge needs no predecessor or float conversion.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(9_007_199_254_740_993));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let response = session.app_resident_history().unwrap();

    assert_eq!(response["selected"]["stamp"]["publication"], "1");
    assert_eq!(response["cohort"].as_array().unwrap().len(), 1);
    assert_eq!(response["cohort"][0]["sum_bytes"], "9007199254740993");
    assert_eq!(response["cohort"][0]["samples"], 1);
    assert_eq!(response["cohort"][0]["key"]["display"], "/Apps/Current.app");
    assert_eq!(
        response["points"][0]["apps"][0]["known"],
        "9007199254740993"
    );
    assert_eq!(response["points"][0]["apps"][0]["measured_members"], 1);
    assert_eq!(response["points"][0]["other"]["app_count"], 1);
    assert_eq!(
        response["points"][0]["other"]["resident_bytes"]["measured_members"],
        0
    );
    assert_eq!(
        response["points"][0]["other"]["resident_bytes"]["unavailable_members"],
        1
    );
}

#[test]
fn resident_history_restores_the_same_cohort_and_points_after_the_ring_retires() {
    // Reopen real sealed data without registered rings and compare the complete value contract.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(11));
    fixture.publish_with_usage(1001, &usage(29));
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.follow_latest().unwrap();
    let before = live.app_resident_history().unwrap();
    assert_eq!(before["cohort"][0]["sum_bytes"], "40");
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    drop(live);
    let mut saved = LiveSession::open(&fixture.root).unwrap();
    saved.select(1001, 0).unwrap();

    let after = saved.app_resident_history().unwrap();

    assert_eq!(after["cohort"], before["cohort"]);
    assert_eq!(after["points"], before["points"]);
    assert_eq!(after["selected"], before["selected"]);
    assert_eq!(after["coverage"]["committed_observations"], 2);
}

#[test]
fn resident_history_counts_a_saved_live_overlap_once_without_changing_the_cohort() {
    // Seal the same real publications while their live facts remain cached, then merge them.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(11));
    fixture.publish_with_usage(1001, &usage(29));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();
    let before = session.app_resident_history().unwrap();
    fixture.seal();
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let after = session.app_resident_history().unwrap();

    assert_eq!(after["cohort"], before["cohort"]);
    assert_eq!(after["points"], before["points"]);
    assert_eq!(after["cohort"][0]["samples"], 2);
    assert_eq!(after["coverage"]["committed_observations"], 2);
}

fn usage(resident_bytes: u64) -> Usage {
    Usage {
        awake_ns_at_start: 0,
        user_ns: 0,
        system_ns: 0,
        energy_nj: 0,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes,
        footprint_bytes: 0,
        disk_read_bytes: 0,
        disk_written_bytes: 0,
    }
}
