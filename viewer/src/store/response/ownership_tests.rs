use std::sync::Arc;

use persistence::Error;
use serde_json::{Value, json};

use super::Response;
use crate::store::ByteBudget;

#[test]
fn mapped_presentations_keep_admission_until_the_ui_value_is_released() {
    // Goal: converting JSON to a typed chart cannot release its admission prematurely.
    // Method: map one reserved JSON allocation to its parsed count and inspect the budget.
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let reservation = budget.reserve(1024 * 1024).unwrap();
    let response = Response::json(reservation, json!({"values": [1, 2]})).unwrap();
    let bytes = response.bytes();

    let chart = response.map(|value| value["values"].as_array().unwrap().len());

    assert_eq!(*chart, 2);
    assert_eq!(budget.used(), bytes);
    drop(chart);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_chart_parsing_releases_its_admission() {
    // Goal: a rejected typed presentation cannot leave a retained reservation behind.
    // Method: fail a reserved response's parser before a UI value is produced.
    let budget = Arc::new(ByteBudget::new(1024));
    let response = Response::reserved(budget.reserve(1024).unwrap(), Value::Null);

    let result = response.try_map(|_| Err::<usize, _>("fixture parse refusal"));

    assert!(matches!(result, Err("fixture parse refusal")));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_tiny_fan_shape_keeps_its_actual_charge_after_preflight() {
    // Goal: a small sensor presentation cannot retain a maximum-sized preflight reservation.
    // Method: finalize six fan cells under a large permitted operation budget.
    let budget = Arc::new(ByteBudget::new(32 * 1024 * 1024));
    let reservation = budget.reserve(32 * 1024 * 1024).unwrap();
    let value = json!({"fans": [1000, 1100, 1200, 1300, 1400, 1500]});

    let response = Response::json(reservation, value).unwrap();

    assert!(response.bytes() < 64 * 1024);
    assert_eq!(budget.used(), response.bytes());
}

#[test]
fn an_insufficient_preflight_is_explicit_and_releases_its_owner() {
    // Goal: accounting cannot silently retain an allocation beyond the admitted operation.
    // Method: finalize a nonempty object under an intentionally insufficient byte guard.
    let budget = Arc::new(ByteBudget::new(1024));
    let reservation = budget.reserve(1).unwrap();

    let result = Response::json(reservation, json!({"value": "retained"}));

    assert!(matches!(
        result,
        Err(Error::Bound("Store presentation admission bytes"))
    ));
    assert_eq!(budget.used(), 0);
}
