use std::collections::BTreeSet;

use serde_json::Value;

use super::{ContextRange, NS, Session, context_reader_count};
use crate::live::identity_fixture::Fixture;
use crate::metrics;

/// The run tick of each reading `sealed_minutes` records, a second apart from `SECOND_FIRST`
/// on. They fall in seven sealed minutes, which a context's readers read in runs of two, so
/// four readers read them. The first reading of every minute after the first takes its
/// rates from the last reading of the minute before where their run ticks follow on: from
/// the minute a reader read before, between minutes 0 and 1, and from a minute another reader
/// read, between minutes 1 and 2, 3 and 4, and 5 and 6.
const RUN_TICKS: [u64; 8] = [59, 60, 119, 120, 239, 240, 359, 360];
const MINUTE_COUNT: usize = 7;
const READER_COUNT: usize = 4;
/// The second of the first reading `sealed_minutes` records.
const SECOND_FIRST: u64 = 1000;
/// The seconds each sealed minute holds, from the first up to but not including the end.
const MINUTE_SECONDS: [(u64, u64); MINUTE_COUNT] = [
    (1000, 1001),
    (1001, 1003),
    (1003, 1004),
    (1004, 1005),
    (1005, 1006),
    (1006, 1007),
    (1007, 1008),
];

const _: () = assert!(context_reader_count(MINUTE_COUNT) == READER_COUNT);

#[test]
fn context_reads_a_minute_s_tables_once_and_again_before_each_later_reader() {
    // Goal: a context's readers read each sealed minute's tables once, and each reader after the
    // first reads the minute before its first minute too, since that minute holds the reading
    // its first rates come from.
    // Method: read the context of seven sealed minutes, which four readers read, and count the
    // table queries it made: seven minutes, and three more for the three later readers.
    let fixture = sealed_minutes();
    let session = Session::open(&fixture.root).unwrap();
    let queries = session.table_queries.get();

    let points = context(&session, SECOND_FIRST, seconds_end());

    assert_eq!(points.len(), RUN_TICKS.len());
    let table_count = u64::try_from(metrics::TABLE_NAMES.len()).unwrap();
    assert_eq!(session.table_queries.get() - queries, 10 * table_count);
}

#[test]
fn context_over_several_readers_gives_the_points_one_reader_gives() {
    // Goal: a context's points, rates included, do not depend on how its minutes were split
    // between readers.
    // Method: read the context of seven sealed minutes, which four readers read, then take the
    // points of the same minutes in turn on one reader with one set of tables.
    let fixture = sealed_minutes();
    let session = Session::open(&fixture.root).unwrap();

    let whole = context(&session, SECOND_FIRST, seconds_end());
    let reader = session.reader();
    let mut tables = metrics::Tables::new();
    let mut alone = Vec::new();
    for (first, end) in MINUTE_SECONDS {
        let observations = session.observations(range(first, end)).unwrap();
        alone.extend(metrics::trends(&reader, &observations, &mut tables).unwrap());
    }

    let rated = whole
        .iter()
        .map(|point| point["cpu_percent"].is_number())
        .collect::<Vec<_>>();
    assert_eq!(rated, [false, true, false, true, false, true, false, true]);
    assert_eq!(without_segments(whole), without_segments(alone));
}

#[test]
fn context_numbers_segments_across_its_readers() {
    // Goal: a context numbers its segments from its first point, whichever reader read each.
    // Method: read the context of seven sealed minutes, which four readers read, where readings
    // come in pairs of consecutive run ticks with missed ticks between the pairs.
    let fixture = sealed_minutes();
    let session = Session::open(&fixture.root).unwrap();

    let points = context(&session, SECOND_FIRST, seconds_end());

    let segments = points
        .iter()
        .map(|point| point["segment"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(segments, [1, 1, 2, 2, 3, 3, 4, 4]);
}

#[test]
fn context_keeps_the_tables_of_a_minute_and_of_the_minute_before_only() {
    // Goal: a reader of many sealed minutes holds the tables of two minutes at most, since only
    // the minute before a minute holds the reading its first rates come from.
    // Method: take the points of the sealed minutes in turn with one set of tables, and list the
    // minutes whose tables it keeps after each.
    let fixture = sealed_minutes();
    let session = Session::open(&fixture.root).unwrap();
    let reader = session.reader();
    let mut tables = metrics::Tables::new();

    let kept = MINUTE_SECONDS
        .iter()
        .map(|(first, end)| {
            let observations = session.observations(range(*first, *end)).unwrap();
            metrics::trends(&reader, &observations, &mut tables).unwrap();
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
            BTreeSet::from([3, 4]),
            BTreeSet::from([4, 5]),
            BTreeSet::from([5, 6]),
        ]
    );
}

#[test]
fn select_reads_no_minute_again_for_the_context_it_read_last() {
    // Goal: selecting a second again, as a launch's next step does, reads none of the sealed
    // minutes its context read the first time, and gives the same response.
    // Method: select a second of seven sealed minutes twice and count the saved reads of the
    // second time.
    let fixture = sealed_minutes();
    let mut session = Session::open(&fixture.root).unwrap();
    let first = session.select(SECOND_FIRST + 3, 0).unwrap();
    let reads = session.source_reads.get();

    let again = session.select(SECOND_FIRST + 3, 0).unwrap();

    assert_eq!(session.source_reads.get(), reads);
    assert_eq!(again, first);
}

#[test]
fn select_reads_its_own_context_after_a_select_of_another_context() {
    // Goal: a select whose context differs from the one the last select read gives what a
    // session that selected nothing before gives.
    // Method: seal two minutes too far apart to share a context, select a second of the first
    // and then one of the second, and compare the second response with a new session's.
    const EARLIER: u64 = 1000;
    const LATER: u64 = 3000;
    const { assert!(EARLIER + crate::CONTEXT_AFTER_SECONDS < LATER) };
    const { assert!(EARLIER + crate::CHARTED_SECONDS <= LATER) };
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(EARLIER);
    fixture.skip_run_ticks(LATER - EARLIER - 1);
    fixture.publish_with_system_rows(LATER);
    fixture.seal_minute(0);
    fixture.seal_minute((LATER - EARLIER) / 60);
    let mut session = Session::open(&fixture.root).unwrap();
    session.select(EARLIER, 0).unwrap();

    let later = session.select(LATER, 0).unwrap();

    let mut fresh = Session::open(&fixture.root).unwrap();
    assert_eq!(later, fresh.select(LATER, 0).unwrap());
}

/// The readings of `RUN_TICKS`, sealed by minute.
fn sealed_minutes() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(RUN_TICKS[0]);
    let mut second = SECOND_FIRST;
    fixture.publish_with_system_rows(second);
    for pair in RUN_TICKS.windows(2) {
        fixture.skip_run_ticks(pair[1] - pair[0] - 1);
        second += 1;
        fixture.publish_with_system_rows(second);
    }
    for minute in 0..MINUTE_COUNT {
        fixture.seal_minute(u64::try_from(minute).unwrap());
    }

    fixture
}

/// The second after the last reading `sealed_minutes` records.
fn seconds_end() -> u64 {
    SECOND_FIRST + u64::try_from(RUN_TICKS.len()).unwrap()
}

/// The context points of the seconds from `first` up to `end`.
fn context(session: &Session, first: u64, end: u64) -> Vec<Value> {
    let mut points = Vec::new();
    session.context(range(first, end), &mut points).unwrap();

    points
}

/// The range of the seconds from `first` up to `end`.
fn range(first: u64, end: u64) -> ContextRange {
    ContextRange {
        first_ns: first * NS,
        end_ns: end * NS,
    }
}

/// `points` without their segment numbers, which count the breaks from the first point of
/// one context, so contexts that start at different points number them differently.
fn without_segments(mut points: Vec<Value>) -> Vec<Value> {
    for point in &mut points {
        point.as_object_mut().unwrap().remove("segment");
    }

    points
}
