use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::allocation_tests::observe;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::{APPS_PAGE_ROWS_MAX, Page, RESPONSE_BYTES_MAX};
use crate::selection::ChartPoint;
use crate::store::ChartPoints;
use crate::store::chart_fixture::Charted;
use crate::store::response::JSON_BYTES_MAX;
use crate::store::{ByteBudget, QueryContext, QueryOutcome, QueryRequest, QuerySelection, Store};
use crate::{Cancellation, LiveSession, Selection, TrendPoint, Trends};

#[test]
fn a_native_selection_matches_the_saved_minute_after_its_source_is_gone() {
    // Goal: the Store's selection response contains exactly the saved minute's selected facts.
    // Method: read the saved minute's selection, then remove every producer before formatting.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let context = retained(&fixture);
    fixture.seal();
    let mut saved = LiveSession::open(&fixture.root).unwrap();
    let mut expected = crate::live::select_newest(&mut saved);
    drop(saved);
    drop(fixture);

    let mut actual = context
        .selection(&Arc::new(Cancellation::default()), 0)
        .unwrap()
        .json();

    let context_error = actual.as_object_mut().unwrap().remove("context_error");
    losses_comparable(&mut actual);
    losses_comparable(&mut expected);
    assert_eq!(context_error, Some(Value::Null));
    assert_eq!(actual, expected);
}

#[test]
fn a_cancelled_selection_returns_no_response_or_new_retained_owner() {
    // Goal: superseded formatting cannot retain a partial result.
    // Method: cancel the operation before reading one complete canonical context.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let context = retained(&fixture);
    let cancellation = Arc::new(Cancellation::default());
    cancellation.cancel().unwrap();

    let result = context.selection(&cancellation, 0);

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
}

#[test]
fn supplied_wrong_clocks_cannot_relabel_a_retained_observation() {
    // Goal: the presentation boundary keeps exact clocks alongside run/boot/publication.
    // Method: alter only the wall clock of an otherwise matching real selected identity.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let context = retained(&fixture);
    let mut identity = context.catalog()["latest_observation"].clone();
    identity["wall_ns"] = serde_json::json!(100_000_000_001_u64);

    assert!(context.observation_for_identity(100, &identity).is_err());
}

#[test]
fn an_apps_page_lists_every_app_one_second_holds() {
    // Goal: the retained context admits a page as long as a second's most Apps, so the App
    // picker sorts and searches every App rather than one page of them.
    // Method: retain a real context, then request its selected second's Apps on one such page.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let context = retained(&fixture);
    let identity = context.catalog()["latest_observation"].clone();
    let observation = context.observation_for_identity(100, &identity).unwrap();
    let page = Page {
        offset: 0,
        limit: APPS_PAGE_ROWS_MAX,
    };

    let response = context
        .apps_page_for(&observation, page, &Arc::new(Cancellation::default()), 0)
        .unwrap();

    assert_eq!(
        response["page"]["total"],
        response["apps"].as_array().unwrap().len()
    );
    assert_eq!(response["page"]["next_offset"], Value::Null);
}

#[test]
fn a_selection_is_bounded_by_the_bytes_its_json_takes_written_out() {
    // Goal: a selection whose chart points travel apart from its fields is bounded by the bytes
    // the one JSON value it stands for takes written out, so the Store admits no selection that
    // bound refuses.
    // Method: read a followed context of three seconds, and compare the bytes the bound counts
    // with the bytes of the selection's JSON written out.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.publish_with_system_rows(102);
    let selection = retained(&fixture)
        .selection(&Arc::new(Cancellation::default()), 0)
        .unwrap();

    let counted = counted_serialized_bytes(&selection);

    assert_eq!(selection.trends().unwrap().len(), 3);
    assert_eq!(counted, written_bytes(&selection));
}

#[test]
fn a_selection_whose_segments_take_two_digits_is_bounded_by_its_json_written_out() {
    // Goal: the bound counts each segment's digits, so a chart broken more than nine times is
    // bounded by what its JSON takes written out.
    // Method: take a second, then twelve times miss a tick and take one, so the segments run to
    // 13, and compare the bytes the bound counts with the bytes of the JSON written out.
    let mut store = Charted::new();
    store.take(1_000);
    for second in 1_001..1_013 {
        store.miss();
        store.take(second);
    }
    let selection = store
        .followed()
        .selection(&Arc::new(Cancellation::default()), 0)
        .unwrap();

    let counted = counted_serialized_bytes(&selection);

    assert_eq!(segments(&selection).last(), Some(&13));
    assert_eq!(counted, written_bytes(&selection));
}

#[test]
fn a_selection_of_the_summary_alone_has_no_chart_points() {
    // Goal: a selection that keeps the context the window holds carries the selected second's
    // summary and builds no chart.
    // Method: read the selected second of a followed context of three seconds as a hover does.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.publish_with_system_rows(102);
    let context = retained(&fixture);

    let summary = context
        .selection_for(
            &context.selected().observation,
            true,
            &mut ChartPoints::default(),
            &Arc::new(Cancellation::default()),
            0,
        )
        .unwrap();

    assert_eq!(summary.trends(), None);
}

#[test]
fn a_selection_whose_json_passes_the_response_bound_is_refused() {
    // Goal: the Store refuses a selection whose JSON written out passes the bound on a
    // response, as it refused one built as a single JSON value, where the fields alone fit.
    // Method: give the selection's response no fields and one chart point that takes the whole
    // bound written out, which with the chart's key passes it.
    let budget = Arc::new(ByteBudget::new(super::POINT_BYTES_MAX));
    let point_reservation = budget.reserve(1).unwrap();
    let point = ChartPoint::admitted(json!({}), point_reservation, RESPONSE_BYTES_MAX);
    let trends = Trends::of(vec![TrendPoint::shared(Arc::new(point), 1)]);
    let reservation = budget.reserve(1024).unwrap();

    let refused = super::selection_for_response(reservation, super::object([]), trends);

    assert!(matches!(
        refused,
        Err(persistence::Error::Bound("live App response bytes"))
    ));
}

#[test]
fn a_selection_that_charges_past_the_json_bound_is_refused() {
    // Goal: the Store refuses a selection whose fields, list of points and points together
    // charge more than one JSON value may, as it refused one built as a single JSON value.
    // Method: give the selection's response one chart point whose reservation charges a byte
    // past the bound, which holds no memory of its own.
    let points = Arc::new(ByteBudget::new(2 * JSON_BYTES_MAX));
    let point = ChartPoint::admitted(json!({}), points.reserve(JSON_BYTES_MAX + 1).unwrap(), 2);
    let trends = Trends::of(vec![TrendPoint::shared(Arc::new(point), 1)]);
    let budget = Arc::new(ByteBudget::new(super::POINT_BYTES_MAX));
    let reservation = budget.reserve(super::POINT_BYTES_MAX).unwrap();

    let refused = super::selection_for_response(reservation, super::object([]), trends);

    assert!(matches!(
        refused,
        Err(persistence::Error::Bound("Store JSON retained bytes"))
    ));
}

#[test]
fn a_response_object_takes_its_chart_without_copying_it() {
    // Goal: a selection's chart reaches its response without a second copy of every point.
    // Method: count the allocations of one object over a chart of one point and of 1,000.
    let point = json!({"cpu_percent": 1.0, "run": "1"});
    let short = Value::Array(vec![point.clone()]);
    let long = Value::Array(vec![point; 1000]);

    let (_short, short_requests) = observe(|| super::object([("trends", short)]));
    let (_long, long_requests) = observe(|| super::object([("trends", long)]));

    assert_eq!(long_requests, short_requests);
}

#[test]
fn a_chart_starts_a_segment_at_a_missed_second_a_restart_and_a_reboot() {
    // Goal: the chart's points carry the segment numbers of the one rule, which starts a segment
    // wherever a reading does not follow the one before it in one run of one boot.
    // Method: take five seconds of one run, miss a tick, take two more, restart the run and take
    // two, reboot and take two, then read the chart of the followed context.
    let mut store = Charted::new();
    for second in 1_000..1_005 {
        store.take(second);
    }
    store.miss();
    store.take(1_006);
    store.take(1_007);
    store.restart();
    store.take(1_008);
    store.take(1_009);
    store.reboot();
    store.take(1_010);
    store.take(1_011);

    let selection = store
        .followed()
        .selection(&Arc::new(Cancellation::default()), 0)
        .unwrap();

    assert_eq!(segments(&selection), [1, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4]);
}

#[test]
fn a_fixed_chart_numbers_its_segments_from_the_first_second_it_draws() {
    // Goal: a fixed context's chart, whose window is shorter than the seconds the context holds,
    // numbers its segments from its own first point, whatever readings lie before it.
    // Method: take three seconds, miss a tick and take six more, of which the first two lie
    // before the chart's 900 seconds, miss a tick, take two more, then the newest second after a
    // gap, and read the chart of the context fixed on the newest second. Numbering every second
    // of the context would start the chart at 2.
    let mut store = Charted::new();
    for second in 1_000..1_003 {
        store.take(second);
    }
    store.miss();
    for second in 1_004..1_010 {
        store.take(second);
    }
    store.miss();
    store.take(1_011);
    store.take(1_012);
    store.take(1_905);
    let context = store.fixed(1_905);

    let selection = context
        .selection(&Arc::new(Cancellation::default()), 0)
        .unwrap();

    assert_eq!(context.seconds().len(), 12);
    assert_eq!(segments(&selection), [1, 1, 1, 1, 2, 2, 3]);
}

/// The bytes the bound counts of `selection` written out as one JSON value.
fn counted_serialized_bytes(selection: &Selection) -> usize {
    let mut fields = selection.json();
    fields.as_object_mut().unwrap().remove("trends");

    super::selection_for_serialized_bytes(&fields, selection.trends().unwrap()).unwrap()
}

/// The bytes of `selection`'s JSON written out.
fn written_bytes(selection: &Selection) -> usize {
    serde_json::to_vec(&selection.json()).unwrap().len()
}

/// The segment of each of `selection`'s chart points, in their order.
fn segments(selection: &Selection) -> Vec<u64> {
    selection
        .trends()
        .unwrap()
        .iter()
        .map(|point| point.segment().unwrap())
        .collect()
}

fn retained(fixture: &Fixture) -> Arc<QueryContext> {
    retained_with_budget(fixture, &Arc::new(ByteBudget::new(256 * 1024 * 1024)))
}

fn retained_with_budget(fixture: &Fixture, budget: &Arc<ByteBudget>) -> Arc<QueryContext> {
    let store = Store::start_with_budget(&fixture.root, Arc::clone(budget)).unwrap();
    let handle = store.query_handle();
    let request = QueryRequest {
        epoch: 0,
        source: None,
        selection: QuerySelection::FollowLatest,
    };
    handle.submit(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    for _ in 0..500 {
        if let Some(reply) = handle.take() {
            match reply.outcome {
                QueryOutcome::Ready(context) => return context,
                QueryOutcome::NeedsLoad(_) => {
                    handle.submit(request).unwrap();
                }
                QueryOutcome::Unavailable(reason) => panic!("unavailable context: {reason:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "the actual Store did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    panic!("query bound exhausted");
}

/// Names a source's losses alike on both routes, since a saved minute reports the losses of its
/// window and the Store those of the reader that took its readings.
pub(in crate::store) fn losses_comparable(value: &mut Value) {
    let mut values = vec![value];
    while let Some(value) = values.pop() {
        match value {
            Value::Object(fields) => {
                if fields.contains_key("window_losses") || fields.contains_key("reader_losses") {
                    let window = fields.remove("window_losses").unwrap_or(Value::Null);
                    let reader = fields.remove("reader_losses").unwrap_or(Value::Null);
                    let losses = if window.is_null() { reader } else { window };
                    fields.insert("losses".to_owned(), losses);
                }
                values.extend(fields.values_mut());
            }
            Value::Array(items) => values.extend(items.iter_mut()),
            _ => {}
        }
    }
}
