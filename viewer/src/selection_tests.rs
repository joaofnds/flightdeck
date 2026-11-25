use serde_json::{Value, json};

use super::{Selection, Trends};
use crate::allocation_tests::observe;

fn selection_json() -> Value {
    json!({"second": 1000, "context_last_second": 1000, "selected": {"publication": 3},
        "trends": [
            {"wall_ns": 999_000_000_000_u64, "publication": 2, "segment": 1, "cpu_percent": 25.0},
            {"wall_ns": 1_000_000_000_000_u64, "publication": 3, "segment": 2,
             "cpu_percent": null}],
        "summary": {"cpu_percent": null}, "context_seconds": 900})
}

#[test]
fn a_split_selection_flattens_to_the_json_it_was_made_from() {
    // Goal: splitting a selection loses nothing, so its JSON output is today's.
    // Method: split a selection of two points and flatten it again.
    let value = selection_json();

    let selection = Selection::from_json(value.clone());

    assert_eq!(selection.json(), value);
}

#[test]
fn a_split_point_holds_its_segment_beside_it() {
    // Goal: a shared point carries no segment, which depends on the points beside it.
    // Method: split a selection and read each point and its segment.
    let selection = Selection::from_json(selection_json());

    let trends = selection.trends().unwrap();

    assert_eq!(trends.len(), 2);
    assert_eq!(trends[0].segment(), Some(1));
    assert_eq!(trends[1].segment(), Some(2));
    assert_eq!(trends[0].point().get("segment"), None);
    assert_eq!(trends[1].point()["publication"], 3);
    assert_eq!(trends.last().unwrap().json()["segment"], 2);
}

#[test]
fn a_split_selection_reads_its_other_fields_as_before() {
    // Goal: every field but the chart points reads as the JSON object's field did.
    // Method: index the split selection by key, including a key it does not have.
    let selection = Selection::from_json(selection_json());

    assert_eq!(selection["second"], 1000);
    assert_eq!(selection["selected"]["publication"], 3);
    assert_eq!(selection["earlier_trends"], Value::Null);
    assert_eq!(selection.get("context_seconds"), Some(&json!(900)));
}

#[test]
#[should_panic(expected = "A selection's chart points are read through Selection::trends")]
fn a_read_of_the_chart_points_by_key_stops_the_program() {
    // Goal: the chart points cannot be read by key by mistake, which would read nothing.
    // Method: index a split selection by "trends".
    let selection = Selection::from_json(selection_json());

    assert!(selection["trends"].is_null());
}

#[test]
#[should_panic(expected = "A selection's chart points are read through Selection::trends")]
fn a_get_of_the_chart_points_by_key_stops_the_program() {
    // Goal: `get` refuses the chart points' key, which would read nothing. Method: get
    // "trends" from a split selection.
    let selection = Selection::from_json(selection_json());

    assert!(selection.get("trends").is_none());
}

#[test]
#[should_panic(expected = "A selection's chart points are read through Selection::trends")]
fn a_write_of_the_chart_points_by_key_stops_the_program() {
    // Goal: a write by key cannot put chart points among the fields, where `json` would meet
    // them beside the split points. Method: write "trends" through the index of a split
    // selection.
    let mut selection = Selection::from_json(selection_json());

    selection["trends"] = json!([]);
}

#[test]
#[should_panic(expected = "A selection's chart points are read through Selection::trends")]
fn an_insert_of_the_chart_points_among_the_fields_stops_the_program() {
    // Goal: the fields refuse the chart points' key, as the index does. Method: insert
    // "trends" into the fields of a split selection.
    let mut selection = Selection::from_json(selection_json());

    selection
        .fields_mut()
        .unwrap()
        .insert("trends".to_owned(), json!([]));
}

#[test]
#[should_panic(expected = "A selection's chart points are read through Selection::trends")]
fn an_extend_that_carries_the_chart_points_stops_the_program() {
    // Goal: extending the fields refuses a map that carries the chart points' key, since the
    // selection holds its points apart from its fields. Method: extend the fields of a split
    // selection with a map holding "alternatives" and "trends".
    let mut selection = Selection::from_json(selection_json());
    let Value::Object(map) = json!({"alternatives": [], "trends": []}) else {
        unreachable!("The map is an object");
    };

    selection.fields_mut().unwrap().extend(map);
}

#[test]
fn the_fields_take_and_give_up_other_keys() {
    // Goal: the fields add, extend and remove other keys as the JSON object did. Method: insert
    // one field, extend with another, remove the first, then read both.
    let mut selection = Selection::from_json(selection_json());
    let Value::Object(map) = json!({"alternatives": [1]}) else {
        unreachable!("The map is an object");
    };

    let mut fields = selection.fields_mut().unwrap();
    fields.insert("earlier_error".to_owned(), json!("gone"));
    fields.extend(map);
    let removed = fields.remove("earlier_error");

    assert_eq!(removed, Some(json!("gone")));
    assert_eq!(selection.get("earlier_error"), None);
    assert_eq!(selection["alternatives"], json!([1]));
    assert_eq!(selection.trends().unwrap().len(), 2);
}

#[test]
fn a_selection_without_chart_points_has_none() {
    // Goal: a selection with no "trends" key stays apart from one with no points, as its JSON
    // was, since a summary-only selection has none.
    // Method: split a selection without the key and one whose list is empty.
    let without = json!({"second": 1000, "summary": {"cpu_percent": 1.0}});
    let empty = json!({"second": 1000, "summary": {"cpu_percent": 1.0}, "trends": []});

    let without_selection = Selection::from_json(without.clone());
    let empty_selection = Selection::from_json(empty.clone());

    assert_eq!(without_selection.trends(), None);
    assert_eq!(without_selection.json(), without);
    assert_eq!(empty_selection.trends(), Some(&Trends::default()));
    assert_eq!(empty_selection.json(), empty);
    assert_ne!(without_selection, empty_selection);
}

#[test]
fn a_selection_that_is_not_an_object_has_no_chart_points() {
    // Goal: a page that shows no reading reads its null selection as before.
    // Method: split a null selection and index it.
    let selection = Selection::from_json(Value::Null);

    assert!(selection.is_null());
    assert_eq!(selection.trends(), None);
    assert_eq!(selection["selected"], Value::Null);
    assert_eq!(selection.json(), Value::Null);
}

#[test]
fn taking_the_chart_points_leaves_the_other_fields() {
    // Goal: a page left behind drops its points and keeps the selected second's reading.
    // Method: take the points of a split selection, then flatten what is left.
    let mut selection = Selection::from_json(selection_json());

    let trends = selection.take_trends();

    assert_eq!(trends.unwrap().len(), 2);
    assert_eq!(selection.trends(), None);
    let mut expected = selection_json();
    expected.as_object_mut().unwrap().remove("trends");
    assert_eq!(selection.json(), expected);
}

#[test]
fn taking_a_shared_older_tail_copies_none_of_its_points() {
    // Goal: a page that lets go of its older tail, as one whose points the View draws does each
    // reading, copies none of the points the session still shares with it. Method: give a
    // selection a tail another holder shares, take it out, and count the allocations and the
    // holders of the list.
    let points = std::sync::Arc::new(json!([{"wall_ns": 1}, {"wall_ns": 2}]));
    let mut selection = Selection::from_json(selection_json());
    selection
        .fields_mut()
        .unwrap()
        .insert_earlier_tail(super::EarlierTail::of(std::sync::Arc::clone(&points), 10));

    let (taken, requests) = observe(|| selection.take_earlier_trends());

    assert_eq!(requests, 0);
    assert!(std::sync::Arc::ptr_eq(&taken.unwrap(), &points));
    assert!(selection.get("earlier_trends").is_none());
    assert_eq!(std::sync::Arc::strong_count(&points), 1);
}

#[test]
#[should_panic(expected = "the older tail leaves by take_earlier_trends")]
fn removing_the_older_tail_as_a_field_is_refused() {
    // Goal: no edit copies the older tail out of the list the session shares by removing it as
    // a field. Method: give a selection a tail and remove it as a field.
    let points = std::sync::Arc::new(json!([{"wall_ns": 1}]));
    let mut selection = Selection::from_json(selection_json());
    let mut fields = selection.fields_mut().unwrap();
    fields.insert_earlier_tail(super::EarlierTail::of(points, 10));

    drop(fields.remove("earlier_trends"));
}

#[test]
#[should_panic(expected = "the older tail comes in by insert_earlier_tail")]
fn inserting_the_older_tail_as_a_field_is_refused() {
    // Goal: no field edit gives a selection an older tail that shares no list with the session,
    // so the tail comes in only as the list it shares. Method: insert a list of points under the
    // older tail's key.
    let mut selection = Selection::from_json(selection_json());

    selection
        .fields_mut()
        .unwrap()
        .insert("earlier_trends".to_owned(), json!([{"wall_ns": 1}]));
}

#[test]
#[should_panic(expected = "!fields.contains_key(EARLIER_TRENDS_KEY)")]
fn a_selection_whose_fields_hold_an_older_tail_stops_the_program() {
    // Goal: a selection the Store builds never hides an older tail among its fields, where
    // reading the tail would not find it. Method: make a selection of fields that hold the
    // older tail's key.
    let Value::Object(fields) = json!({"earlier_trends": []}) else {
        unreachable!("The fields are an object");
    };

    drop(Selection::of(fields, Trends::default()));
}

#[test]
fn a_point_without_a_segment_keeps_none() {
    // Goal: a point with no segment flattens without one, as a fixture's point was.
    // Method: split a selection whose only point has no segment.
    let value = json!({"second": 1000, "trends": [{"wall_ns": 1_000_000_000_000_u64}]});

    let selection = Selection::from_json(value.clone());

    assert_eq!(selection.trends().unwrap()[0].segment(), None);
    assert_eq!(selection.json(), value);
}

#[test]
fn splitting_a_selection_allocates_one_shared_point_for_each_point() {
    // Goal: the split costs a followed reading one allocation for each point's `Arc` and none
    // for a copy of a point's content. Method: count the allocations of splitting a selection
    // of one point and one of 1,000, whose difference is what 999 more points cost.
    let point = json!({"cpu_percent": 1.0, "run": "1", "segment": 0});
    let short = json!({"second": 1, "trends": [point.clone()]});
    let long = json!({"second": 1, "trends": vec![point; 1000]});

    let (_short, short_requests) = observe(|| Selection::from_json(short));
    let (_long, long_requests) = observe(|| Selection::from_json(long));

    assert_eq!(long_requests - short_requests, 999);
}
