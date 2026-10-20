use crate::live::identity_fixture::Fixture;

/// The trend point of `second` in a selection's context, which names its observation as a
/// hover names the one it shows.
fn context_point(selection: &Value, second: u64) -> Value {
    selection["trends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["wall_ns"].as_u64().unwrap() / NANOSECONDS_PER_SECOND == second)
        .unwrap()
        .clone()
}

/// A selection's summary without the segment its context numbers, which an inspection of the
/// same observation brings.
fn unsegmented(summary: &Value) -> Value {
    let mut summary = summary.clone();
    assert!(summary.as_object_mut().unwrap().remove("segment").is_some());
    summary
}

#[test]
fn inspect_reads_a_saved_second_without_its_context() {
    // Goal: a hover's read of a saved second reads that second's window, never the 900 seconds
    // around it, and brings the summary a selection of it brings.
    // Method: seal one second in each of seven run minutes, which the session holds only as
    // saved, commit to the first, inspect the last by the identity its trend point carries, and
    // count the saved tables the inspection reads against those a selection of it reads.
    let mut fixture = Fixture::new();
    for minute in 0..7 {
        if minute > 0 {
            fixture.skip_run_ticks(59);
        }
        fixture.publish_with_system_rows(1000 + minute * 60);
        fixture.seal_minute(minute);
    }
    let identity = context_point(
        &LiveSession::open(&fixture.root)
            .unwrap()
            .select(1360, 0)
            .unwrap(),
        1360,
    );
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.poll().unwrap();
    live.poll_evict(u64::MAX).unwrap();
    live.select(1000, 0).unwrap();
    let reads = live.history.as_ref().unwrap().source_reads.get();

    let inspected = live.inspect(1360, &identity).unwrap();

    let inspect_reads = live.history.as_ref().unwrap().source_reads.get() - reads;
    assert!(same(&inspected["selected"], &identity));
    assert!(same(
        &live.page("ticks", 0).unwrap()["observation"],
        &identity
    ));
    let reads = live.history.as_ref().unwrap().source_reads.get();
    let selected = live.select(1360, 0).unwrap();
    let select_reads = live.history.as_ref().unwrap().source_reads.get() - reads;
    assert_eq!(inspected["second"], 1360);
    assert_eq!(inspected["summary"], unsegmented(&selected["summary"]));
    assert!(inspect_reads * 2 < select_reads);
}

#[test]
fn inspect_selects_the_named_observation_of_a_repeated_second() {
    // Goal: a hover on the second of two observations in one second reads that observation.
    // Method: seal two observations of second 1000, commit to the first, inspect the second.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1000);
    fixture.seal();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let committed = live.select(1000, 0).unwrap();
    let sibling = committed["alternatives"][1].clone();

    let inspected = live.inspect(1000, &sibling).unwrap();

    assert_eq!(inspected["selected"]["publication"], 2);
    assert_eq!(
        live.page("ticks", 0).unwrap()["observation"]["publication"],
        2
    );
}

#[test]
fn inspect_refuses_an_identity_the_second_does_not_hold() {
    // Goal: a hover never reads a neighbour in place of the observation it names.
    // Method: inspect second 1001 by the identity of second 1000's observation.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let committed = live.select(1001, 0).unwrap();
    let identity = context_point(&committed, 1000);

    let inspected = live.inspect(1001, &identity);

    assert!(inspected.is_err());
    assert!(live.page("ticks", 0).is_err());
}

#[test]
fn inspect_reads_a_live_second_without_the_saved_minutes() {
    // Goal: a hover on a second the collector has published but not yet sealed reads it from the
    // live readings, never the saved minutes.
    // Method: publish two seconds without sealing them, commit to the newer, inspect the older,
    // and count the saved tables the inspection reads.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.poll().unwrap();
    let committed = live.select(1001, 0).unwrap();
    let identity = context_point(&committed, 1000);
    let reads = live
        .history
        .as_ref()
        .map(|history| history.source_reads.get());

    let inspected = live.inspect(1000, &identity).unwrap();

    assert_eq!(
        live.history
            .as_ref()
            .map(|history| history.source_reads.get()),
        reads
    );
    assert!(same(&inspected["selected"], &identity));
    assert!(same(
        &live.page("ticks", 0).unwrap()["observation"],
        &identity
    ));
    assert_eq!(
        inspected["summary"],
        unsegmented(&live.select(1000, 0).unwrap()["summary"])
    );
}
