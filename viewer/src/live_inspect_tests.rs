use crate::live::identity_fixture::Fixture;

/// The observation of `second` in a selection's context, named by its run, boot and
/// publication alone, as a hover names the one it shows.
fn hover_identity(selection: &Value, second: u64) -> Value {
    let point = selection["trends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["wall_ns"].as_u64().unwrap() / NANOSECONDS_PER_SECOND == second)
        .unwrap();

    json!({"run": point["run"], "boot": point["boot"], "publication": point["publication"]})
}

/// A selection's summary without the segment its context numbers, which an inspection of the
/// same observation brings.
fn unsegmented(summary: &Value) -> Value {
    let mut summary = summary.clone();
    assert!(summary.as_object_mut().unwrap().remove("segment").is_some());
    summary
}

/// Seals one second in each of `minutes` run minutes, from second 1000 on, one minute apart.
fn saved_minutes(minutes: u64) -> Fixture {
    let mut fixture = Fixture::new();
    for minute in 0..minutes {
        if minute > 0 {
            fixture.skip_run_ticks(59);
        }
        fixture.publish_with_system_rows(1000 + minute * 60);
        fixture.seal_minute(minute);
    }

    fixture
}

/// A session over `fixture` that holds none of its observations live, so it reads each second
/// from the saved minutes or the ring.
fn evicted_session(fixture: &Fixture) -> LiveSession {
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.poll().unwrap();
    live.poll_evict(u64::MAX).unwrap();

    live
}

/// The saved tables a hover's read of the last of `minutes` saved minutes reads, with the
/// selection on the first.
fn saved_inspection_reads(minutes: u64) -> u64 {
    let fixture = saved_minutes(minutes);
    let last = 1000 + (minutes - 1) * 60;
    let identity = hover_identity(
        &LiveSession::open(&fixture.root)
            .unwrap()
            .select(last, 0)
            .unwrap(),
        last,
    );
    let mut live = evicted_session(&fixture);
    live.select(1000, 0).unwrap();
    let reads = live.history.as_ref().unwrap().source_reads.get();

    live.inspect(last, &identity).unwrap();

    live.history.as_ref().unwrap().source_reads.get() - reads
}

#[test]
fn reads_as_many_saved_tables_however_many_minutes_surround_its_second() {
    // Goal: a hover's read of a saved second reads that second's window and its predecessor's,
    // never the 900 seconds around it.
    // Method: read the last second of two saved minutes, then of seven, with the selection on
    // the first, and count the saved tables each read takes.
    let two = saved_inspection_reads(2);

    let seven = saved_inspection_reads(7);

    assert_ne!(two, 0);
    assert_eq!(seven, two);
}

#[test]
fn selects_the_saved_observation_it_names() {
    // Goal: a hover's read of a saved second selects the observation it names, for the read and
    // for the pages read after it.
    // Method: seal two minutes, commit to the first, and read the second by its trend point.
    let fixture = saved_minutes(2);
    let mut live = evicted_session(&fixture);
    let identity = hover_identity(&live.select(1060, 0).unwrap(), 1060);
    live.select(1000, 0).unwrap();

    let inspected = live.inspect(1060, &identity).unwrap();

    assert_eq!(inspected["second"], 1060);
    assert!(same(&inspected["selected"], &identity));
    assert!(same(
        &live.page("ticks", 0).unwrap()["observation"],
        &identity
    ));
}

#[test]
fn brings_the_summary_a_selection_of_its_saved_second_brings() {
    // Goal: a hover on a saved second shows the readings a click on that second shows.
    // Method: seal two minutes, select the second, commit to the first, and read the second.
    let fixture = saved_minutes(2);
    let mut live = evicted_session(&fixture);
    let selected = live.select(1060, 0).unwrap();
    let identity = hover_identity(&selected, 1060);
    live.select(1000, 0).unwrap();

    let inspected = live.inspect(1060, &identity).unwrap();

    assert_eq!(inspected["summary"], unsegmented(&selected["summary"]));
}

#[test]
fn selects_the_named_saved_observation_of_a_repeated_second() {
    // Goal: a hover on the later of two saved observations in one second reads that one.
    // Method: seal two observations of second 1000, hold neither live, commit to the first,
    // and read the second.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1000);
    fixture.seal();
    let mut live = evicted_session(&fixture);
    let sibling = live.select(1000, 0).unwrap()["alternatives"][1].clone();

    let inspected = live.inspect(1000, &sibling).unwrap();

    assert_eq!(inspected["selected"]["publication"], 2);
    assert_eq!(
        live.page("ticks", 0).unwrap()["observation"]["publication"],
        2
    );
}

#[test]
fn selects_the_named_live_observation_of_a_repeated_second() {
    // Goal: a hover on the later of two live observations in one second reads that one.
    // Method: seal two observations of second 1000, hold both live, commit to the first, and
    // read the second.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1000);
    fixture.seal();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let sibling = live.select(1000, 0).unwrap()["alternatives"][1].clone();

    let inspected = live.inspect(1000, &sibling).unwrap();

    assert_eq!(inspected["selected"]["publication"], 2);
    assert_eq!(
        live.page("ticks", 0).unwrap()["observation"]["publication"],
        2
    );
}

#[test]
fn refuses_an_identity_the_second_does_not_hold() {
    // Goal: a hover never reads a neighbour in place of the observation it names.
    // Method: inspect second 1001 by the identity of second 1000's observation.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let identity = hover_identity(&live.select(1001, 0).unwrap(), 1000);

    let inspected = live.inspect(1001, &identity);

    assert!(matches!(
        inspected,
        Err(Error::Bound("inspected observation"))
    ));
    assert!(matches!(
        live.page("ticks", 0),
        Err(Error::Bound("no selected observation"))
    ));
}

#[test]
fn reads_a_second_the_ring_alone_holds() {
    // Goal: a hover on a published second the session no longer holds live, and no minute
    // keeps, reads it from the ring.
    // Method: publish two seconds without sealing them, hold neither live, commit to the
    // newer, and read the older.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    let mut live = evicted_session(&fixture);
    let identity = hover_identity(&live.select(1001, 0).unwrap(), 1000);

    let inspected = live.inspect(1000, &identity).unwrap();

    assert!(same(&inspected["selected"], &identity));
}

#[test]
fn refuses_a_ring_observation_at_another_second() {
    // Goal: a hover never reads a neighbour the ring holds in place of the second it names.
    // Method: publish two seconds without sealing them, hold neither live, and inspect second
    // 1001 by the identity of second 1000's observation.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    let mut live = evicted_session(&fixture);
    let identity = hover_identity(&live.select(1001, 0).unwrap(), 1000);

    let inspected = live.inspect(1001, &identity);

    assert!(matches!(
        inspected,
        Err(Error::Bound("inspected observation second"))
    ));
}

#[test]
fn reads_a_live_second_without_the_saved_minutes() {
    // Goal: a hover on a second the session holds live reads it from the live readings, never
    // the saved minutes, though a minute keeps it too.
    // Method: seal two seconds and hold both live, commit to the older, read the newer, and
    // count the saved tables the read takes.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.poll().unwrap();
    let identity = hover_identity(&live.select(1001, 0).unwrap(), 1001);
    live.select(1000, 0).unwrap();
    let reads = live.history.as_ref().unwrap().source_reads.get();

    let inspected = live.inspect(1001, &identity).unwrap();

    assert!(same(&inspected["selected"], &identity));
    assert_eq!(live.history.as_ref().unwrap().source_reads.get(), reads);
}

#[test]
fn brings_the_summary_a_selection_of_its_live_second_brings() {
    // Goal: a hover on a live second shows the readings a click on that second shows.
    // Method: publish two seconds without sealing them, select the older, commit to the newer,
    // and read the older.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.poll().unwrap();
    let selected = live.select(1000, 0).unwrap();
    let identity = hover_identity(&selected, 1000);
    live.select(1001, 0).unwrap();

    let inspected = live.inspect(1000, &identity).unwrap();

    assert_eq!(inspected["summary"], unsegmented(&selected["summary"]));
}
