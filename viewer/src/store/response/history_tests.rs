use std::sync::Arc;

use serde_json::Value;

use super::tests::{native_context, saved_context};
use crate::live::identity_fixture::Fixture;
use crate::live::window::fixture::saved_only;
use crate::{AppAttributionMetric, Cancellation, LiveSession};

#[test]
fn resident_measurements_and_cohort_match_the_saved_live_session() {
    // Goal: resident histories preserve the existing means, Other, identity and gap semantics.
    // Method: compare every measurement field against the real saved LiveSession response.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let expected = session.app_resident_history().unwrap();
    let context = saved_context(&fixture);
    let cancellation = Arc::new(Cancellation::default());
    drop(session);
    drop(fixture);

    let shown = context.app_resident_history(&cancellation, 0).unwrap();

    assert_eq!(measurements(&shown), measurements(&expected));
    assert_eq!(shown["coverage"]["retention_limited"], false);
    assert_eq!(shown["coverage"]["observations_retained"], 2);
    assert_eq!(shown["coverage"]["cache_bytes"], Value::Null);
}

#[test]
fn resident_measurements_and_cohort_match_the_native_live_session() {
    // Goal: canonical native rows feed the same resident reducer after their source is gone.
    // Method: compare a real two-publication native context with LiveSession output.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.poll().unwrap();
    session.select(101, 0).unwrap();
    let expected = session.app_resident_history().unwrap();
    let context = native_context(&fixture);
    let cancellation = Arc::new(Cancellation::default());
    drop(session);
    drop(fixture);

    let shown = context.app_resident_history(&cancellation, 0).unwrap();

    assert_eq!(measurements(&shown), measurements(&expected));
}

macro_rules! attribution_case {
    ($name:ident, $metric:expr) => {
        #[test]
        fn $name() {
            // Goal: this metric keeps the legacy cohort, rate formula order and availability.
            // Method: compare complete measurement JSON from the same real saved fixture.
            let fixture = saved_only();
            let mut session = LiveSession::open(&fixture.root).unwrap();
            session.select(1001, 0).unwrap();
            let expected = session.app_attribution_history($metric).unwrap();
            let context = saved_context(&fixture);
            let cancellation = Arc::new(Cancellation::default());
            drop(session);
            drop(fixture);

            let shown = context
                .app_attribution_history($metric, &cancellation, 0)
                .unwrap();

            assert_eq!(measurements(&shown), measurements(&expected));
            assert_eq!(shown["metric"], expected["metric"]);
            assert_eq!(shown["coverage"]["retention_limited"], false);
        }
    };
}

attribution_case!(
    cpu_measurements_match_saved_history,
    AppAttributionMetric::Cpu
);
attribution_case!(
    power_measurements_match_saved_history,
    AppAttributionMetric::Power
);
attribution_case!(
    disk_measurements_match_saved_history,
    AppAttributionMetric::Disk
);
attribution_case!(
    gpu_measurements_match_saved_history,
    AppAttributionMetric::Gpu
);

#[test]
fn a_superseded_history_request_returns_no_partial_response() {
    // Goal: a cancelled generation cannot format a stale history response.
    // Method: cancel before entering the retained resident reducer.
    let fixture = saved_only();
    let context = saved_context(&fixture);
    let cancellation = Arc::new(Cancellation::default());
    cancellation.cancel().unwrap();

    let result = context.app_resident_history(&cancellation, 0);

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
}

fn measurements(response: &Value) -> [&Value; 4] {
    [
        &response["selected"],
        &response["context_seconds"],
        &response["cohort"],
        &response["points"],
    ]
}
