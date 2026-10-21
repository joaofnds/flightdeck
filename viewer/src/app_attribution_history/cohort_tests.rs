use std::sync::Arc;

use serde_json::Value;

use super::{AppAttributionMetric, Capture, Reading, Reduction};
use crate::Cancellation;
use crate::apps::{AppKey, Stamp};
use crate::live::resident_history::{Capture as Coverage, Source};
use crate::live_app_response::{Measure, Observation};

const KEYS: [&[u8]; 10] = [
    b"/A.app", b"/B.app", b"/C.app", b"/D.app", b"/E.app", b"/F.app", b"/G.app", b"/H.app",
    b"/I.app", b"/J.app",
];

struct Sample {
    publication: u64,
    elapsed_ns: u64,
    amounts: Vec<Option<u64>>,
}

#[test]
fn attribution_cohort_keeps_interval_leaders_and_other_when_the_current_leader_changes() {
    // Two projected observations make the selected leader differ from the stable eight leaders.
    let mut first = vec![Some(199); 9];
    first[8] = Some(0);
    let mut last = vec![Some(1); 9];
    last[8] = Some(100);

    let response = response(&[
        Sample {
            publication: 1,
            elapsed_ns: 1_000_000_000,
            amounts: first,
        },
        Sample {
            publication: 2,
            elapsed_ns: 1_000_000_000,
            amounts: last,
        },
    ]);

    assert_eq!(response["cohort"].as_array().unwrap().len(), 8);
    assert_eq!(response["cohort"][7]["key"]["display"], "/H.app");
    assert_eq!(response["points"][1]["other"]["cpu_ns"]["known"], "100");
    assert_eq!(response["points"][1]["other"]["app_count"], 1);
}

#[test]
fn attribution_cohort_compares_exact_duration_weighted_means_and_keeps_absence_missing() {
    // A's long idle interval lowers its weighted rate below B's single observed interval.
    let response = response(&[
        Sample {
            publication: 1,
            elapsed_ns: 1,
            amounts: vec![Some(100), None],
        },
        Sample {
            publication: 2,
            elapsed_ns: 9,
            amounts: vec![Some(0), Some(100)],
        },
    ]);

    assert_eq!(response["cohort"][0]["key"]["display"], "/B.app");
    assert_eq!(response["points"][0]["apps"][0], Value::Null);
    assert_eq!(
        response["points"][1]["apps"][1]["cpu_percent_one_core"],
        0.0
    );
}

#[test]
fn attribution_cohort_distinguishes_unsigned_totals_above_float_precision() {
    // Exact totals one unit apart must not become an alphabetical floating-point tie.
    let response = response(&[Sample {
        publication: 1,
        elapsed_ns: 1,
        amounts: vec![Some(u64::MAX - 1), Some(u64::MAX)],
    }]);

    assert_eq!(response["cohort"][0]["key"]["display"], "/B.app");
}

#[test]
fn attribution_other_preserves_overflow_and_never_emits_a_wrapped_rate() {
    // Ten maximal Apps force an overflowing Other pair after the deterministic eight-way tie.
    let response = response(&[Sample {
        publication: 1,
        elapsed_ns: 1,
        amounts: vec![Some(u64::MAX); 10],
    }]);

    assert_eq!(response["points"][0]["other"]["cpu_ns"]["overflowed"], true);
    assert_eq!(
        response["points"][0]["other"]["cpu_percent_one_core"],
        Value::Null
    );
}

fn response(samples: &[Sample]) -> Value {
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let selected = capture(samples.last().unwrap()).coverage.observation;
    let mut reduction = Reduction::new(
        &crate::live_app_response::Selected::ending_at(selected).unwrap(),
        AppAttributionMetric::Cpu,
        &operation,
    );
    for sample in samples {
        reduction
            .observe(&capture(sample), readings(sample), Source::Live)
            .unwrap();
    }
    reduction.choose().unwrap();
    for sample in samples {
        reduction
            .project(&capture(sample), readings(sample), Source::Live)
            .unwrap();
    }
    reduction.response().unwrap()
}

fn capture(sample: &Sample) -> Capture {
    Capture {
        elapsed_ns: Some(sample.elapsed_ns),
        coverage: Coverage {
            observation: Observation {
                recording: [1, 2],
                stamp: Stamp {
                    boot: [3, 4],
                    run: [5, 6],
                    publication: sample.publication,
                    continuous_ns: Some(sample.publication),
                    awake_ns: Some(sample.publication),
                },
                run_tick: sample.publication,
                wall_ns: Some(sample.publication * 1_000_000_000),
            },
            membership_complete: true,
            ungrouped_members: 0,
            usage_refused: 0,
            usage_gone: 0,
        },
    }
}

fn readings(sample: &Sample) -> impl Iterator<Item = Reading<'static>> + '_ {
    sample
        .amounts
        .iter()
        .enumerate()
        .filter_map(|(index, amount)| {
            amount.map(|known| Reading {
                key: AppKey::Bundle(KEYS[index]),
                measure: Measure {
                    known,
                    measured_members: 1,
                    unavailable_members: 0,
                    overflowed: false,
                },
                members: 1,
                membership_complete: true,
            })
        })
}
