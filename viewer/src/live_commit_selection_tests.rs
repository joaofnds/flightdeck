use crate::live::identity_fixture::Fixture;

#[test]
fn repeated_commit_handoffs_preserve_the_nonfirst_same_second_observation() {
    // Goal: paused raw fallback must retain an exact repeated-second identity across refreshes.
    // Method: seal two siblings, evict raw rows, then seal a later minute and compare tick pages.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1000);
    fixture.seal();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let selected = live.select(1000, 1).unwrap();
    assert_eq!(selected["selected"]["publication"], 2);
    let before = live.page("ticks", 0).unwrap();
    fixture.seal();

    fixture.seal();
    live.list_saved().unwrap();
    let first = live.page("ticks", 0).unwrap();
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1060);
    fixture.seal_minute(1);
    fixture.seal();
    live.list_saved().unwrap();
    let second = live.page("ticks", 0).unwrap();

    assert_eq!(first["rows"], before["rows"]);
    assert_eq!(second["rows"], before["rows"]);
    assert!(same(&first["observation"], &selected["selected"]));
    assert!(same(&second["observation"], &selected["selected"]));
    assert_eq!(
        second["observation"]["run_tick"],
        selected["selected"]["run_tick"]
    );
}

#[test]
fn selecting_a_dropped_second_reads_a_minute_sealed_since_the_listing() {
    // Goal: a selection whose context reaches past the listed minutes reads the minutes sealed
    // since, though its own second is in a listed one and the ring has dropped both.
    // Method: seal a minute the session has not listed, empty the ring's readings, then select
    // the listed minute's only second and read where its context ends.
    let (_fixture, mut live) = open_before_an_unlisted_minute();

    let selected = live.select(1001, 0).unwrap();

    assert_eq!(selected["context_last_second"], 1061);
    assert_eq!(live.catalog()["windows"], 2);
}

/// A session that listed minute 0, whose ring holds no readings, with minute 1 sealed since.
fn open_before_an_unlisted_minute() -> (Fixture, LiveSession) {
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal_minute(0);
    let live = LiveSession::open(&fixture.root).unwrap();
    fixture.skip_run_ticks(59);
    fixture.publish(1061);
    fixture.seal_minute(1);
    assert_eq!(live.catalog()["windows"], 1);
    (fixture, live)
}

