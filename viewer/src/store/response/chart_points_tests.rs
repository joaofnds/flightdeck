//! Tests that a reading builds only the chart points of the seconds the reading before did not
//! build from the same complete reading, shows what a reading from nothing shows, holds no
//! point its window left, and keeps every point it shares charged.
use std::sync::{Arc, Weak};

use super::ChartPoints;
use crate::allocation_tests::observe;
use crate::selection::ChartPoint;
use crate::store::chart_fixture::{
    Charted, FIRST_SECOND, charted, charted_point_count, shared_count,
};
use crate::store::{QueryContext, Response};
use crate::{CHARTED_SECONDS, Cancellation, Selection, TrendPoint};

/// The selection `context` reads of its selected second, sharing what `chart_points` holds.
fn read(context: &QueryContext, chart_points: &mut ChartPoints) -> Response<Selection> {
    context
        .selection_for(
            &context.selected().observation,
            false,
            chart_points,
            &Arc::new(Cancellation::default()),
            0,
        )
        .unwrap()
}

/// The selection `context` reads of its selected second from an empty cache.
fn read_from_nothing(
    context: &QueryContext,
    _chart_points: &mut ChartPoints,
) -> Response<Selection> {
    read(context, &mut ChartPoints::default())
}

#[test]
fn a_reading_a_second_later_shares_every_point_but_the_newest() {
    // Goal: while a page follows the newest second, the Store builds each second's chart point
    // once, so the reading a second later builds only the new second's point.
    // Method: read a whole chart, take in one more second, read again with what the first
    // reading left, and count the points the two readings share.
    let mut store = charted();
    let mut chart_points = ChartPoints::default();
    let first = read(&store.followed(), &mut chart_points);
    store.take(FIRST_SECOND + CHARTED_SECONDS);

    let second = read(&store.followed(), &mut chart_points);

    assert_eq!(first.trends().unwrap().len(), charted_point_count());
    assert_eq!(second.trends().unwrap().len(), charted_point_count());
    assert_eq!(shared_count(&first, &second), charted_point_count() - 1);
}

#[test]
fn a_reading_a_second_later_allocates_far_less_than_a_reading_from_nothing() {
    // Goal: a point the reading shares is a point it does not build, so the reading a second
    // later does the work of one new point and not of the whole chart.
    // Method: read a whole chart, take in one more second, then count the allocations of a
    // reading of the new context from an empty cache and of one with what the first left.
    let mut store = charted();
    let mut chart_points = ChartPoints::default();
    drop(read(&store.followed(), &mut chart_points));
    store.take(FIRST_SECOND + CHARTED_SECONDS);
    let context = store.followed();

    let (_from_nothing, from_nothing) = observe(|| read_from_nothing(&context, &mut chart_points));
    let (_shared, shared) = observe(|| read(&context, &mut chart_points));

    assert!(
        shared * 10 < from_nothing,
        "{shared} allocations sharing, {from_nothing} from nothing"
    );
}

/// A change a step of a test makes to the store before it reads.
type ChartedChange = fn(&mut Charted);

#[test]
fn a_reading_shows_what_a_reading_from_nothing_shows() {
    // Goal: the points a reading takes from the reading before change nothing it shows or
    // charges, the segments included, across a break that leaves the window, a restart of the
    // run, a second whose complete reading the store replaced with one of other content, and a
    // second with no complete reading.
    // Method: read a sequence of followed contexts with one cache, and compare each reading
    // with the reading of the same context from an empty cache.
    let mut store = Charted::new();
    for second in 1_000..1_011 {
        store.take(second);
    }
    store.miss();
    for second in 1_012..1_021 {
        store.take(second);
    }
    store.take_incomplete(1_021);
    let mut chart_points = ChartPoints::default();
    let steps: [(&str, ChartedChange); 4] = [
        ("the first reading", |_| {}),
        ("the window starts past the missed tick", |store| {
            store.take(1_970);
        }),
        ("the run restarted", |store| {
            store.restart();
            store.take(1_971);
            store.take(1_972);
        }),
        ("a complete reading was replaced", |store| {
            store.replace(1_015, 99.5);
        }),
    ];

    for (step, change) in steps {
        change(&mut store);
        let context = store.followed();
        let expected = read(&context, &mut ChartPoints::default());

        let shown = read(&context, &mut chart_points);

        assert_eq!(*shown, *expected, "after {step}");
        assert_eq!(shown.json(), expected.json(), "after {step}");
        assert_eq!(shown.bytes(), expected.bytes(), "after {step}");
    }
}

#[test]
fn a_reader_holds_no_point_of_a_second_its_window_left() {
    // Goal: the chart points a reader keeps are those of its last chart alone, so what it holds
    // is bounded by the window and not by how long it has read.
    // Method: read a whole chart, take in one more second so the oldest leaves the window, read
    // again, and see that nothing holds the oldest second's point once the first reading is gone.
    let mut store = charted();
    let mut chart_points = ChartPoints::default();
    let first = read(&store.followed(), &mut chart_points);
    let left = first.trends().unwrap()[0].downgrade();
    store.take(FIRST_SECOND + CHARTED_SECONDS);

    let second = read(&store.followed(), &mut chart_points);

    drop(first);
    assert!(left.upgrade().is_none());
    assert_eq!(chart_points.point_count(), charted_point_count());
    assert_eq!(second.trends().unwrap().len(), charted_point_count());
}

#[test]
fn a_hover_between_two_readings_leaves_the_points_of_the_first() {
    // Goal: a hover's reading, which keeps the context the window holds and builds no chart,
    // leaves the reader's chart points as they were.
    // Method: read a whole chart, read a hover of an older second with the same cache, take in
    // one more second, read again, and count the points the two charts share.
    let mut store = charted();
    let mut chart_points = ChartPoints::default();
    let context = store.followed();
    let first = read(&context, &mut chart_points);
    let hovered = context.seconds()[100].observation;
    let hover = context
        .selection_for(
            &hovered,
            true,
            &mut chart_points,
            &Arc::new(Cancellation::default()),
            0,
        )
        .unwrap();
    drop(context);
    store.take(FIRST_SECOND + CHARTED_SECONDS);

    let second = read(&store.followed(), &mut chart_points);

    assert_eq!(hover.trends(), None);
    assert_eq!(shared_count(&first, &second), charted_point_count() - 1);
}

#[test]
fn a_point_the_cache_let_go_of_stays_charged_while_a_reading_holds_it() {
    // Goal: the budget counts a shared chart point for as long as anything holds it, after the
    // reader's cache let go of it too.
    // Method: read a whole chart, read again a second later so the cache lets go of the oldest
    // point, then drop the first reading and see the budget fall by its own bytes and the
    // oldest point's charge, which is that of its JSON with its segment in it.
    let mut store = charted();
    let mut chart_points = ChartPoints::default();
    let first = read(&store.followed(), &mut chart_points);
    let oldest = &first.trends().unwrap()[0];
    let left = oldest.downgrade();
    let left_bytes = crate::store::response::json_charge(&oldest.json()).unwrap();
    store.take(FIRST_SECOND + CHARTED_SECONDS);
    let second = read(&store.followed(), &mut chart_points);
    let budget = store.responses();
    let held = budget.used();
    let first_bytes = first.bytes();

    drop(first);

    assert!(left.upgrade().is_none());
    assert_eq!(held - budget.used(), first_bytes + left_bytes);
    assert_eq!(second.trends().unwrap().len(), charted_point_count());
}

#[test]
fn ten_followed_readings_hold_no_more_than_the_first_and_peak_below_ten_from_nothing() {
    // Goal: following the newest second holds one set of chart points, so the bytes held stay
    // where the first reading left them and the peak stays below that of readings that each
    // build every point while the reading before is still held.
    // Method: read ten followed contexts a second apart, holding the last reading, once with
    // one cache and once from an empty cache each time, each over a store of its own.
    let (first, last, peak) = ten_followed_readings(read);
    let (_, _, peak_from_nothing) = ten_followed_readings(read_from_nothing);

    assert!(last <= first);
    assert!(peak < peak_from_nothing);
}

/// The bytes the response budget holds after the first and the tenth of ten followed readings
/// a second apart, each read with `read_followed` while the reading before is held, and the
/// budget's peak.
fn ten_followed_readings(
    read_followed: fn(&QueryContext, &mut ChartPoints) -> Response<Selection>,
) -> (usize, usize, usize) {
    let mut store = charted();
    let mut chart_points = ChartPoints::default();
    let budget = store.responses();
    store.take(FIRST_SECOND + CHARTED_SECONDS);
    let mut held = read_followed(&store.followed(), &mut chart_points);
    let first = budget.used();
    for step in 1..10 {
        store.take(FIRST_SECOND + CHARTED_SECONDS + step);
        let reading = read_followed(&store.followed(), &mut chart_points);
        drop(std::mem::replace(&mut held, reading));
    }
    let last = budget.used();
    drop(held);

    (first, last, budget.used_peak())
}

impl TrendPoint {
    /// A pointer to the point that holds it no longer than the point lives.
    fn downgrade(&self) -> Weak<ChartPoint> {
        Arc::downgrade(self.shared_point())
    }
}
