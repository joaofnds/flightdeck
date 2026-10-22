use std::collections::BTreeSet;

use serde_json::Value;

use super::{ContextRange, NS, Session};
use crate::live::identity_fixture::Fixture;
use crate::metrics;

/// The seconds `sealed_minutes` records readings in, from the first up to but not including the
/// end.
const SECOND_FIRST: u64 = 1000;
const SECOND_END: u64 = 1006;
/// The seconds of each sealed minute `sealed_minutes` records, in the same form.
const MINUTE_SECONDS: [(u64, u64); 4] = [(1000, 1001), (1001, 1003), (1003, 1005), (1005, 1006)];

#[test]
fn context_reads_the_tables_of_each_sealed_minute_once() {
    // Goal: a context over several sealed minutes reads each minute's tables once, though the
    // first reading of each minute takes its rates from the last reading of the minute before.
    // Method: read the context of four sealed minutes and count the table queries it made.
    let fixture = sealed_minutes();
    let session = Session::open(&fixture.root).unwrap();
    let queries = session.table_queries.get();

    let points = context(&session, SECOND_FIRST, SECOND_END);

    assert_eq!(points.len(), 6);
    let minute_count = u64::try_from(MINUTE_SECONDS.len()).unwrap();
    let table_count = u64::try_from(metrics::TABLE_NAMES.len()).unwrap();
    assert_eq!(
        session.table_queries.get() - queries,
        minute_count * table_count
    );
}

#[test]
fn context_over_several_sealed_minutes_gives_each_minute_the_points_it_gives_alone() {
    // Goal: a minute's points, rates included, do not depend on which minutes a context read
    // before it.
    // Method: read the context of four sealed minutes at once, then each minute on its own.
    let fixture = sealed_minutes();
    let session = Session::open(&fixture.root).unwrap();

    let whole = context(&session, SECOND_FIRST, SECOND_END);
    let alone = MINUTE_SECONDS
        .iter()
        .flat_map(|(first, end)| context(&session, *first, *end))
        .collect::<Vec<_>>();

    let rated = whole
        .iter()
        .map(|point| point["cpu_percent"].is_number())
        .collect::<Vec<_>>();
    assert_eq!(rated, [false, true, false, true, false, true]);
    assert_eq!(without_segments(whole), without_segments(alone));
}

#[test]
fn context_keeps_the_tables_of_a_minute_and_of_the_minute_before_only() {
    // Goal: a context over many sealed minutes holds the tables of two minutes at most, since
    // only the minute before a minute holds the reading its first rates come from.
    // Method: take the points of four sealed minutes in turn with one set of tables, and list
    // the minutes whose tables it keeps after each.
    let fixture = sealed_minutes();
    let session = Session::open(&fixture.root).unwrap();
    let mut tables = metrics::Tables::new();

    let kept = MINUTE_SECONDS
        .iter()
        .map(|(first, end)| {
            let observations = session.observations(first * NS, end * NS).unwrap();
            metrics::trends(&session, &observations, &mut tables).unwrap();
            tables
                .keys()
                .map(|(window, _)| *window)
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        kept,
        [
            BTreeSet::from([0]),
            BTreeSet::from([0, 1]),
            BTreeSet::from([1, 2]),
            BTreeSet::from([2, 3]),
        ]
    );
}

/// Six readings, seconds 1000 to 1005, in four sealed run minutes, of which the first and the
/// last hold one reading and the others two. The two readings of a minute stand 58 run ticks
/// apart, so only the first reading of each minute after the first has rates, which it takes
/// from the last reading of the minute before.
fn sealed_minutes() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1002);
    fixture.publish_with_system_rows(1003);
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1004);
    fixture.publish_with_system_rows(1005);
    for minute in 0..4 {
        fixture.seal_minute(minute);
    }

    fixture
}

/// The context points of the seconds from `first` up to `end`.
fn context(session: &Session, first: u64, end: u64) -> Vec<Value> {
    let mut points = Vec::new();
    session
        .context(
            ContextRange {
                first_ns: first * NS,
                end_ns: end * NS,
            },
            &mut points,
        )
        .unwrap();

    points
}

/// `points` without their segment numbers, which count the breaks from the first point of
/// one context, so contexts that start at different points number them differently.
fn without_segments(mut points: Vec<Value>) -> Vec<Value> {
    for point in &mut points {
        point.as_object_mut().unwrap().remove("segment");
    }

    points
}
