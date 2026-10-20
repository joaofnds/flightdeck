use std::time::Duration;

use serde_json::{Value, json};

use super::{HOVERS_MAX, HoverTiming, HoverTimings};

#[test]
fn reports_a_hover_with_its_page_its_second_and_its_time() {
    // Goal: a reader of the report learns where each hover was and how long it took.
    // Method: record one finished hover and read the report.
    let mut timings = HoverTimings::new();

    timings.push(timing(999, Some(Duration::from_micros(1500))));

    assert_eq!(
        timings.report(),
        json!({"count":1,"latest":[{"route":"cpu","second":999,"complete_ms":1.5}]})
    );
}

#[test]
fn reports_a_hover_that_ended_before_its_readings_without_a_time() {
    // Goal: a hover that never held its readings does not pass for a fast one.
    // Method: record a hover with no time and read the report.
    let mut timings = HoverTimings::new();

    timings.push(timing(999, None));

    assert_eq!(
        timings.report(),
        json!({"count":1,"latest":[{"route":"cpu","second":999,"complete_ms":null}]})
    );
}

#[test]
fn reports_the_latest_hovers_oldest_first_and_counts_every_hover() {
    // Goal: the report stays bounded and still tells a reader how many hovers it dropped.
    // Method: record one hover more than the report keeps, then read its first and last.
    let mut timings = HoverTimings::new();
    let hovers = u64::try_from(HOVERS_MAX).unwrap() + 1;

    for second in 1..=hovers {
        timings.push(timing(second, None));
    }

    let report = timings.report();
    assert_eq!(report["count"], hovers);
    assert_eq!(report["latest"].as_array().unwrap().len(), HOVERS_MAX);
    assert_eq!(report["latest"][0]["second"], 2);
    assert_eq!(report["latest"][HOVERS_MAX - 1]["second"], hovers);
}

/// A hover as the report gives it, with whether the page held its readings, for tests that
/// drive hovers through a page.
#[derive(Debug, PartialEq)]
pub(crate) struct TimedHover {
    route: String,
    second: u64,
    complete: bool,
}

impl TimedHover {
    pub(crate) fn complete(route: &str, second: u64) -> Self {
        Self {
            route: route.into(),
            second,
            complete: true,
        }
    }

    pub(crate) fn unfinished(route: &str, second: u64) -> Self {
        Self {
            route: route.into(),
            second,
            complete: false,
        }
    }

    /// The hovers of `timings`' report, oldest first.
    pub(crate) fn latest(timings: &HoverTimings) -> Vec<Self> {
        let report = timings.report();
        report["latest"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hover| Self {
                route: hover["route"].as_str().unwrap().into(),
                second: hover["second"].as_u64().unwrap(),
                complete: match &hover["complete_ms"] {
                    Value::Null => false,
                    Value::Number(_) => true,
                    other => panic!("complete_ms {other}"),
                },
            })
            .collect()
    }
}

fn timing(second: u64, complete: Option<Duration>) -> HoverTiming {
    HoverTiming {
        route: "cpu".into(),
        second,
        complete,
    }
}
