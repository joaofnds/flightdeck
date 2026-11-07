/// The wall second of the first reading the fixture publishes.
const EARLIER_BASE: u64 = 10_000;
/// The limit of the tail most tests read: its readings span minutes 1 to 12, and minute 0 holds
/// the reading before the first of them.
const EARLIER_LIMIT: u64 = EARLIER_BASE + 3000;

#[test]
fn a_session_listing_the_saved_minutes_learns_of_a_new_one_and_reads_no_ring() {
    // Goal: a session whose live readings a Store takes in sees what is saved after it opened
    // without reading the ring. Method: open a session that reads no ring on a running recording,
    // save another minute, list, and read what the session knows.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..1, earlier_wall);
    fixture.seal_held();
    let mut session =
        LiveSession::open_with(&fixture.root, Arc::new(Cancellation::default()), 0)
            .unwrap();
    let listed = session.catalog()["last_second"].clone();
    earlier_publish_minutes(&mut fixture, 1..2, earlier_wall);
    assert_eq!(session.catalog()["last_second"], listed);

    session.list_saved().unwrap();

    let catalog = session.catalog();
    assert_ne!(catalog["last_second"], listed);
}

#[test]
fn a_session_refused_a_listing_keeps_the_minutes_it_knows_and_says_why() {
    // Goal: a list the recording refuses leaves a page drawing what it has, with the reason beside
    // it, rather than failing the page. Method: open a session that reads no ring, make the
    // recording's saved minutes unreadable, list, and read what the session knows.
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..2, earlier_wall);
    fixture.seal_held();
    let mut session =
        LiveSession::open_with(&fixture.root, Arc::new(Cancellation::default()), 0)
            .unwrap();
    let listed = session.catalog()["last_second"].clone();
    let data = fixture.root.join("data");
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o000)).unwrap();

    let answer = session.list_saved();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();

    answer.unwrap();
    let catalog = session.catalog();
    assert_eq!(catalog["last_second"], listed);
    assert!(catalog["history_error"].is_string());
}

#[test]
fn a_session_lists_the_saved_minutes_once_its_context_has_moved_a_minute() {
    // Goal: a page read every second opens the recording's list about once a minute, as the
    // recording saves one. Method: list for a context, save another minute, list for a context a
    // second short of a minute later and for one a minute later.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..1, earlier_wall);
    fixture.seal_held();
    let mut session =
        LiveSession::open_with(&fixture.root, Arc::new(Cancellation::default()), 0)
            .unwrap();
    session.list_saved_for(EARLIER_BASE).unwrap();
    let listed = session.catalog()["last_second"].clone();
    earlier_publish_minutes(&mut fixture, 1..2, earlier_wall);

    session.list_saved_for(EARLIER_BASE + 59).unwrap();
    let within_the_minute = session.catalog()["last_second"].clone();
    session.list_saved_for(EARLIER_BASE + 60).unwrap();

    assert_eq!(within_the_minute, listed);
    assert_ne!(session.catalog()["last_second"], listed);
}

#[test]
fn a_canceled_listing_lists_again_at_the_next_read() {
    // Goal: a listing a newer request canceled costs the page no minute of waiting for what the
    // recording saved. Method: save a minute, list with the epoch canceled, then list for the
    // same context in the next epoch.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..1, earlier_wall);
    let control = Arc::new(Cancellation::default());
    fixture.seal_held();
    let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
    session.list_saved_for(EARLIER_BASE).unwrap();
    let listed = session.catalog()["last_second"].clone();
    earlier_publish_minutes(&mut fixture, 1..2, earlier_wall);
    control.cancel().unwrap();

    let canceled = session.list_saved_for(EARLIER_BASE + 60);
    session.begin(control.epoch()).unwrap();
    session.list_saved_for(EARLIER_BASE + 60).unwrap();

    assert!(matches!(canceled, Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_ne!(session.catalog()["last_second"], listed);
}

#[test]
fn reads_each_minute_of_the_older_tail_once() {
    // Goal: following the newest second reads each sealed minute of the older tail from saved
    // history once. Method: seal an hour of minutes, read the tail, then count the saved
    // sources read for it and for the strides after it that stay inside the same minutes.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let opened = earlier_reads(&session);

    session
        .earlier_context(earlier_context_last(EARLIER_LIMIT))
        .unwrap();
    let filled = earlier_reads(&session);
    for stride in 1..=9 {
        let second = earlier_context_last(EARLIER_LIMIT + stride * EARLIER_STRIDE_SECONDS);
        session.earlier_context(second).unwrap();
    }

    assert_eq!(filled - opened, 13);
    assert_eq!(earlier_reads(&session), filled);
}

#[test]
fn reads_each_minute_once_where_its_manifest_holds_no_wall_bounds() {
    // Goal: a minute the persister sealed without wall bounds, since one of its ticks had no
    // wall time, is read once like any other. Method: publish one reading without clocks in every
    // minute, save them, read the tail, and count the saved minutes read for it.
    let mut fixture = RingFixture::new_following();
    for minute in 0..15 {
        for offset in 1..=60 {
            let tick = minute * 60 + offset;
            if offset == 30 {
                fixture.publish_following_without_clocks(u32::try_from(tick).unwrap());
            } else {
                fixture.publish_following_wall(u32::try_from(tick).unwrap(), earlier_wall(tick));
            }
        }
        fixture.seal_through(minute);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let opened = earlier_reads(&session);

    session
        .earlier_context(earlier_context_last(EARLIER_LIMIT))
        .unwrap();

    assert_eq!(earlier_reads(&session) - opened, 13);
}

#[test]
fn the_older_tail_leaves_out_what_expired() {
    // Goal: a chart reaching past the full-detail context shows no expired reading, and every
    // reading still covered keeps its place.
    // Method: read the tail of a saved hour, place the cutoff among its minutes at the clock of
    // tick 361, the first of minute 6, and read the tail again.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    let last = earlier_context_last(EARLIER_LIMIT);
    fixture.seal_held();
    let whole = LiveSession::open(&fixture.root)
        .unwrap()
        .earlier_context(last)
        .unwrap();
    crate::session::retention_tests::expire_before(&fixture.root, 360 * NANOSECONDS_PER_SECOND);

    let retained = LiveSession::open(&fixture.root)
        .unwrap()
        .earlier_context(last)
        .unwrap();

    let cut_ns = earlier_wall(361) * NANOSECONDS_PER_SECOND;
    let walls = |tail: &Value| -> Vec<u64> {
        tail["earlier_trends"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| point["wall_ns"].as_u64().unwrap())
            .collect()
    };
    let whole_walls = walls(&whole);
    assert!(whole_walls[0] < cut_ns);
    assert_eq!(
        walls(&retained),
        whole_walls
            .into_iter()
            .filter(|wall| *wall >= cut_ns)
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_older_tail_starts_inside_a_stretch_the_period_cut_from_its_first_second() {
    // Goal: a retention cutoff moves a second at a time, so it often falls inside a stretch of the
    // older tail, and the tail still reads, leaving out the stretch whose point has expired.
    // Method: place the cutoff five seconds past the start of a stretch, and read the tail.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    let tick = (361..371)
        .find(|tick| earlier_wall(*tick) % EARLIER_STRIDE_SECONDS == 5)
        .unwrap();
    let cut_ns = (tick - 1) * NANOSECONDS_PER_SECOND;
    crate::session::retention_tests::expire_before(&fixture.root, cut_ns);

    fixture.seal_held();
    let tail = LiveSession::open(&fixture.root)
        .unwrap()
        .earlier_context(earlier_context_last(EARLIER_LIMIT))
        .unwrap();

    let first = tail["earlier_trends"][0]["wall_ns"].as_u64().unwrap();
    assert_eq!(first / NANOSECONDS_PER_SECOND, earlier_wall(tick) + 5);
}

#[test]
fn the_older_tail_reads_on_past_a_minute_removal_took_from_under_its_list() {
    // Goal: a session whose list still names a minute removal took reads the tail without calling
    // a healthy recording damaged, and shows what a new session shows.
    // Method: read a later tail, so the session keeps only later minutes, take a removal pass over
    // the first five minutes, and step back over them.
    let mut fixture = RingFixture::new_following();
    let boot = fixture.writer.identity().unwrap().boot;
    crate::session::retention_tests::expire_boot_before(&fixture.root, boot, 0);
    earlier_publish(&mut fixture);
    let last = earlier_context_last(EARLIER_LIMIT);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session
        .earlier_context(earlier_context_last(EARLIER_LIMIT + 600))
        .unwrap();
    let removed = crate::session::retention_tests::expire_and_remove(
        &fixture.root,
        420 * NANOSECONDS_PER_SECOND,
    );

    let stepped = session.earlier_context(last).unwrap();

    assert_eq!(removed, 5);
    let new_session = LiveSession::open(&fixture.root)
        .unwrap()
        .earlier_context(last)
        .unwrap();
    assert_eq!(stepped, new_session);
}

#[test]
fn reads_only_the_sources_of_a_newly_needed_minute() {
    // Goal: a stride whose tail reaches one more sealed minute reads that minute alone. Method:
    // read the tail, move its limit until the join reaches the first reading of minute 13, at
    // EARLIER_BASE + 3120, and count the saved minutes read for the move.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session
        .earlier_context(earlier_context_last(EARLIER_LIMIT))
        .unwrap();
    let filled = earlier_reads(&session);

    session
        .earlier_context(earlier_context_last(EARLIER_BASE + 3110))
        .unwrap();

    assert_eq!(earlier_reads(&session) - filled, 1);
}

#[test]
fn reads_each_minute_once_while_minutes_seal() {
    // Goal: a newly sealed minute, and the refreshed list that lists it, make the tail read
    // only the minutes it newly needs. Method: follow the limits `earlier_follow` steps through,
    // which reach minutes 0 to 14 in all, and count the saved minutes every session read.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..12, earlier_wall);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let (_, reads) = earlier_follow(&mut fixture, &mut session);

    assert_eq!(reads, 15);
}

#[test]
fn rereads_only_the_minute_after_a_removed_one() {
    // Goal: removing a minute changes the reading before the next minute's first, so that
    // minute alone is read again. Method: read the tail, remove minute 5's proof, refresh the
    // list, and count the saved minutes the next read takes.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    session.earlier_context(last).unwrap();
    earlier_remove(&fixture, 5);
    session.refresh_history().unwrap();
    let refreshed = earlier_reads(&session);

    session.earlier_context(last).unwrap();

    assert_eq!(earlier_reads(&session) - refreshed, 1);
}

#[test]
fn reads_a_recovered_minute_and_no_other() {
    // Goal: a minute saved late is read and no other, since the minute after it belongs to a later
    // run, whose first reading takes the reading before it from its own run. Method: stop a run
    // with minute 5 unsaved, record the rest in the next run, read the tail, save minute 5, refresh
    // the list, and count the saved minutes the next read takes.
    let (older, _newer) = earlier_recording_missing_minute_five();
    let mut session = LiveSession::open(&older.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    session.earlier_context(last).unwrap();
    older.seal_through(5);
    session.refresh_history().unwrap();
    let refreshed = earlier_reads(&session);

    session.earlier_context(last).unwrap();

    assert_eq!(earlier_reads(&session) - refreshed, 1);
}

#[test]
fn reads_each_of_two_recovered_minutes_once() {
    // Goal: a fill that reads two minutes saved late reads each once, though it reads them
    // together. Method: stop one run with its last minute, 5, unsaved and leave the next run with
    // its last, 12, unsaved, read the tail, save both, refresh the list, count the saved minutes
    // the next read takes, and compare its tail with a reopened session's.
    let mut first = RingFixture::new_following();
    earlier_publish_minutes(&mut first, 0..5, earlier_wall);
    earlier_publish_minute(&mut first, 5, earlier_wall);
    let mut second = first.next_run();
    earlier_publish_minutes(&mut second, 0..6, |tick| earlier_wall(tick + 6 * 60));
    earlier_publish_minute(&mut second, 6, |tick| earlier_wall(tick + 6 * 60));
    let mut session = LiveSession::open(&first.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    session.earlier_context(last).unwrap();
    first.seal_through(5);
    second.seal_through(6);
    session.refresh_history().unwrap();
    let refreshed = earlier_reads(&session);

    let tail = session.earlier_context(last).unwrap();

    assert_eq!(earlier_reads(&session) - refreshed, 2);
    assert_eq!(tail, earlier_reopened(&first.root, last));
}

#[test]
fn drops_the_minutes_a_tail_leaves() {
    // Goal: the tail keeps only the minutes its range needs, so going back and returning reads
    // the newest minutes again. Method: read the tail, read it 500 seconds earlier, which leaves
    // minutes 13 and 14, return, and count the saved minutes the return takes.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let newest = earlier_context_last(EARLIER_BASE + 3400);
    session.earlier_context(newest).unwrap();
    session
        .earlier_context(earlier_context_last(EARLIER_BASE + 2900))
        .unwrap();
    let back = earlier_reads(&session);

    session.earlier_context(newest).unwrap();

    assert_eq!(earlier_reads(&session) - back, 2);
}

#[test]
fn numbers_one_segment_for_each_run_of_consecutive_seconds() {
    // Goal: the tail draws one line through each run of consecutive seconds, across the edge
    // of two minutes, and breaks it at each missing stretch. Method: read the tail, whose
    // readings run from second 210 of each minute to second 29 of the next, and group its
    // seconds by segment.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let tail = session
        .earlier_context(earlier_context_last(EARLIER_LIMIT))
        .unwrap();

    let expected: Vec<Vec<u64>> = (1..=11)
        .map(|minute| {
            (0..6)
                .map(|stretch| EARLIER_BASE + minute * 240 + 210 + stretch * 10)
                .collect()
        })
        .collect();
    assert_eq!(earlier_segments(&tail["earlier_trends"]), expected);
}

#[test]
fn gives_the_tail_a_reopened_session_reads_while_minutes_seal() {
    // Goal: the tail a session assembles from the minutes it keeps is the one a newly opened
    // session reads from saved history. Method: follow the limits `earlier_follow` steps
    // through, sealing minutes 12 to 14 on the way, and compare each tail with a newly opened
    // session's.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..12, earlier_wall);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let (kept, _) = earlier_follow(&mut fixture, &mut session);

    let reopened: Vec<(u64, Value)> = kept
        .iter()
        .map(|(last, _)| (*last, earlier_reopened(&fixture.root, *last)))
        .collect();
    assert_eq!(kept, reopened);
}

#[test]
fn gives_the_tail_a_reopened_session_reads_after_a_minute_is_removed() {
    // Goal: a minute removed from inside the hour leaves the tail and makes the next minute's
    // first point again. Method: read the tail, remove minute 5's proof, refresh the list, and
    // compare the tail with a newly opened session's.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    let before = session.earlier_context(last).unwrap();
    earlier_remove(&fixture, 5);
    session.refresh_history().unwrap();

    let kept = session.earlier_context(last).unwrap();

    assert_ne!(kept, before);
    assert_eq!(kept, earlier_reopened(&fixture.root, last));
}

#[test]
fn gives_the_tail_a_reopened_session_reads_after_a_minute_is_recovered() {
    // Goal: a minute saved late, before minutes the tail keeps, joins the tail. Method: stop a run
    // with minute 5 unsaved, record the rest in the next run, read the tail, save minute 5, refresh
    // the list, and compare the tail with a newly opened session's.
    let (older, _newer) = earlier_recording_missing_minute_five();
    let mut session = LiveSession::open(&older.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    let before = session.earlier_context(last).unwrap();
    older.seal_through(5);
    session.refresh_history().unwrap();

    let kept = session.earlier_context(last).unwrap();

    assert_ne!(kept, before);
    assert_eq!(kept, earlier_reopened(&older.root, last));
}

#[test]
fn gives_the_tail_a_reopened_session_reads_after_the_limit_moves_back() {
    // Goal: a minute kept only to hold the reading before a needed one gets the point of its own
    // first reading once a tail needs it. Method: read the tail at a limit whose oldest minute
    // is 2, so minute 1 is read without the minute before it, then read the tail 500 seconds
    // earlier, which holds the first reading of minute 1, and compare it with a newly opened
    // session's.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session
        .earlier_context(earlier_context_last(EARLIER_BASE + 3400))
        .unwrap();
    let last = earlier_context_last(EARLIER_BASE + 2900);

    let kept = session.earlier_context(last).unwrap();

    assert_eq!(kept, earlier_reopened(&fixture.root, last));
}

#[test]
fn makes_each_point_as_the_full_detail_context_makes_the_same_reading() {
    // Goal: the tail's point of a reading, made from the minute that holds the reading before
    // it, equals the full-detail context's point of that reading, made from saved history.
    // Method: read the tail, select a second inside it so the full-detail context spans
    // minutes 2 to 6, and compare every point the two share apart from its segment.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let tail = session
        .earlier_context(earlier_context_last(EARLIER_LIMIT))
        .unwrap();

    let context = Session::open(&fixture.root)
        .unwrap()
        .select(EARLIER_BASE + 1200, 0)
        .unwrap();

    let (older, detailed) = earlier_shared(&tail["earlier_trends"], &[&context["trends"]]);
    assert_eq!(older.len(), 24);
    assert_eq!(older, detailed);
}

/// The lowest and highest `name` the readings of `points` in `point`'s stretch read.
fn earlier_stretch_reach(points: &Value, point: &Value, name: &str) -> (f64, f64) {
    let stretch = |point: &Value| {
        point["wall_ns"].as_u64().unwrap() / NANOSECONDS_PER_SECOND / EARLIER_STRIDE_SECONDS
    };
    let read = points
        .as_array()
        .unwrap()
        .iter()
        .filter(|other| stretch(other) == stretch(point))
        .map(|other| other[name].as_f64().unwrap());

    read.fold((f64::MAX, f64::MIN), |(low, high), value| {
        (low.min(value), high.max(value))
    })
}

#[test]
fn each_point_reaches_what_the_readings_of_its_stretch_read() {
    // Goal: a point that stands for ten seconds says what all ten read, so a chart can show a
    // reading that lasted one of them. Method: read the tail, select a second inside it so the
    // full-detail context holds every second of those stretches, and compare what each point says
    // with what they read.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..3, |tick| EARLIER_BASE + tick);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let tail = session
        .earlier_context(earlier_context_last(EARLIER_BASE + 180))
        .unwrap();

    let context = Session::open(&fixture.root)
        .unwrap()
        .select(EARLIER_BASE + 120, 0)
        .unwrap();

    let walls = |point: &Value| point["wall_ns"].as_u64().unwrap();
    let held = &context["trends"];
    let first = walls(&held[0]) / NANOSECONDS_PER_SECOND;
    let shared: Vec<&Value> = tail["earlier_trends"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|point| walls(point) / NANOSECONDS_PER_SECOND > first + EARLIER_STRIDE_SECONDS)
        .filter(|point| walls(point) < walls(held.as_array().unwrap().last().unwrap()))
        .collect();
    assert!(shared.len() > 10);
    for point in shared {
        let (low, high) = earlier_stretch_reach(held, point, "memory_active_bytes");
        let reach = &point["reach"]["leaves"]["memory_active_bytes"];
        assert_eq!(reach[0].as_f64(), Some(low), "{}", point["wall_ns"]);
        assert_eq!(reach[1].as_f64(), Some(high), "{}", point["wall_ns"]);
        assert!(low < high);
    }
}

#[test]
fn a_stretch_two_minutes_share_reaches_the_readings_of_both() {
    // Goal: a stretch does not start with a minute, so its reach adds up what each minute holds of
    // it. Method: readings placed so that a stretch holds the last ticks of one minute and the
    // first of the next, whose memory rises with the tick.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..3, |tick| EARLIER_BASE + tick + 5);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let tail = session
        .earlier_context(earlier_context_last(EARLIER_BASE + 120))
        .unwrap();

    // Ticks 55 to 64 are the stretch of 10,060, which minute 0 ends and minute 1 begins.
    let shared = tail["earlier_trends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| {
            point["wall_ns"].as_u64().unwrap() / NANOSECONDS_PER_SECOND == EARLIER_BASE + 60
        })
        .unwrap();
    let mib = 1024.0 * 1024.0;
    assert_eq!(
        shared["reach"]["leaves"]["memory_active_bytes"].as_array().unwrap()[..2],
        [serde_json::json!(55.0 * mib), serde_json::json!(64.0 * mib)]
    );
}

/// The reading `point` names as the peak of `name`: its identity, as a hover names one, and its
/// wall second.
fn earlier_peak_of(point: &Value, name: &str) -> (Value, u64) {
    let peak = crate::number_reach(&point["reach"], name).unwrap().unwrap().peak;
    let identity = serde_json::json!({
        "run": identity_text(collector::ring::Identity(peak.run)).to_string(),
        "boot": identity_text(collector::ring::Identity(peak.boot)).to_string(),
        "publication": peak.publication,
    });

    (identity, peak.second())
}

#[test]
fn inspecting_the_place_a_point_names_loads_the_reading_that_read_its_peak() {
    // Goal: picking a peak loads the exact reading the peak was read at, through the route a hold
    // reads a second by, and not the one reading the point stands for the stretch with.
    // Method: read the tail, whose readings grow so each stretch peaks at its last second, inspect
    // what a point names, and compare the reading with what the point says it reached.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..2, |tick| EARLIER_BASE + tick);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let tail = session
        .earlier_context(earlier_context_last(EARLIER_BASE + 120))
        .unwrap();
    let point = &tail["earlier_trends"][5];
    let (identity, second) = earlier_peak_of(point, "memory_active_bytes");
    assert_ne!(identity["publication"], point["publication"]);

    let inspected = session.inspect(second, &identity).unwrap();

    assert_eq!(inspected["selected"]["publication"], identity["publication"]);
    assert_eq!(
        inspected["summary"]["memory_active_bytes"].as_f64(),
        point["reach"]["leaves"]["memory_active_bytes"][1].as_f64()
    );
}

#[test]
fn a_stretch_two_runs_read_names_the_run_that_read_its_peak() {
    // Goal: where two runs read the same wall seconds the peak's reading is named by its own run,
    // so a pick loads it and not the newer run's reading of that second.
    // Method: a run whose readings climb, then one read over its last seconds from low readings,
    // the tail's point of those seconds, and an inspection of what the point names.
    let mut fixture = RingFixture::new_following();
    let first_run = fixture.writer.identity().unwrap().run;
    earlier_publish_minutes(&mut fixture, 0..2, |tick| EARLIER_BASE + tick);
    let mut restarted = fixture.next_run();
    earlier_publish_minutes(&mut restarted, 0..2, |tick| EARLIER_BASE + tick + 100);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let tail = session
        .earlier_context(earlier_context_last(EARLIER_BASE + 300))
        .unwrap();
    let shared_second = EARLIER_BASE + 115;
    let point = tail["earlier_trends"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|point| {
            point["wall_ns"].as_u64().unwrap() / NANOSECONDS_PER_SECOND / EARLIER_STRIDE_SECONDS
                == shared_second / EARLIER_STRIDE_SECONDS
        })
        .unwrap();
    let (identity, second) = earlier_peak_of(point, "memory_active_bytes");
    assert_eq!(identity["run"], identity_text(first_run).to_string());

    let inspected = session.inspect(second, &identity).unwrap();

    assert_eq!(inspected["selected"]["run"], identity["run"]);
    assert_eq!(
        inspected["summary"]["memory_active_bytes"].as_f64(),
        point["reach"]["leaves"]["memory_active_bytes"][1].as_f64()
    );
}

#[test]
fn a_point_reaches_no_reading_the_period_no_longer_covers() {
    // Goal: a peak that has expired leaves the chart with its reading, though a later run of the
    // recording read the same wall seconds. Method: a run whose wall clock stepped back over the
    // seconds of the run before, and a cutoff between the two.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..1, |tick| EARLIER_BASE + tick + 2_000);
    let boot_cut = 30 * NANOSECONDS_PER_SECOND;
    let mut restarted = fixture.next_run();
    earlier_publish_minutes(&mut restarted, 1..2, |tick| EARLIER_BASE + tick - 60 + 2_000);
    let seconds = EARLIER_BASE + 2_100;
    crate::session::retention_tests::expire_before(&fixture.root, boot_cut);

    fixture.seal_held();
    let tail = LiveSession::open(&fixture.root)
        .unwrap()
        .earlier_context(earlier_context_last(seconds))
        .unwrap();

    let mib = 1024.0 * 1024.0;
    let points = tail["earlier_trends"].as_array().unwrap();
    assert!(points.len() > 2, "{points:?}");
    for point in points {
        let low = point["reach"]["leaves"]["memory_active_bytes"][0].as_f64().unwrap();
        assert!(low >= 31.0 * mib, "{}", point["wall_ns"]);
    }
}

#[test]
fn a_reading_that_gives_no_point_costs_its_stretch_only_its_own_reach() {
    // Goal: the chart draws one reading of a stretch and reaches over the rest, so a damaged
    // reading it does not draw costs only what that reading read. Method: drop the row of a late
    // reading of a stretch from a saved minute, which also costs the reading after it, and read
    // the stretch's reach.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..2, |tick| EARLIER_BASE + tick);
    let saved = Session::open(&fixture.root).unwrap().windows[0].clone();
    crate::recording::damage_table_file(
        &fixture.root.join(saved.data_directory).join("memory.parquet"),
        |frame| {
            let publications = frame.column("publication").unwrap().u64().unwrap();
            let first = publications.into_no_null_iter().min().unwrap();
            let kept = polars::prelude::ChunkCompareEq::not_equal(publications, first + 27);
            frame.filter(&kept).unwrap()
        },
    );

    fixture.seal_held();
    let tail = LiveSession::open(&fixture.root)
        .unwrap()
        .earlier_context(earlier_context_last(EARLIER_BASE + 60))
        .unwrap();

    let mib = 1024.0 * 1024.0;
    let wall_ns = (EARLIER_BASE + 20) * NANOSECONDS_PER_SECOND;
    let point = tail["earlier_trends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["wall_ns"].as_u64() == Some(wall_ns))
        .unwrap();
    assert_eq!(
        point["reach"]["leaves"]["memory_active_bytes"].as_array().unwrap()[..2],
        [json!(20.0 * mib), json!(27.0 * mib)]
    );
}

#[test]
fn a_minute_whose_clock_runs_backwards_refuses_the_tail_and_does_not_abort() {
    // Goal: a saved file is another program's word, so a stretch left with no point by a clock no
    // run reads is an error to report and no reason to stop the app. Method: rewrite a minute so
    // the second reading of a stretch reads the stretch's first wall second on a clock before the
    // cutoff, which leaves the point on the reading that expired and the reading before it kept.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..2, |tick| EARLIER_BASE + tick);
    let saved = Session::open(&fixture.root).unwrap().windows[0].clone();
    crate::recording::damage_table_file(
        &fixture.root.join(saved.data_directory).join("ticks.parquet"),
        |mut frame| {
            let values = |frame: &polars::prelude::DataFrame, name: &str| -> Vec<u64> {
                let column = frame.column(name).unwrap().u64().unwrap();
                column.into_no_null_iter().collect()
            };
            let mut walls = values(&frame, "wall_ns");
            walls.swap(19, 20);
            let mut clocks = values(&frame, "continuous_ns");
            clocks[20] = 0;
            let column = polars::prelude::Column::new;
            frame.replace("wall_ns", column("wall_ns".into(), walls)).unwrap();
            frame.replace("continuous_ns", column("continuous_ns".into(), clocks)).unwrap();
            frame
        },
    );
    crate::session::retention_tests::expire_before(&fixture.root, 5 * NANOSECONDS_PER_SECOND);

    fixture.seal_held();
    let tail = LiveSession::open(&fixture.root)
        .unwrap()
        .earlier_context(earlier_context_last(EARLIER_BASE + 60));

    assert!(matches!(tail, Err(Error::Bound("older tail point"))), "{tail:?}");
}

#[test]
fn the_last_point_says_what_the_seconds_at_the_join_reached() {
    // Goal: the seconds between the tail's limit and the first full-detail second lie in no point,
    // so a peak there stays findable until it ages into a stretch of the tail.
    // Method: a context whose first charted second lies inside a stretch, and read what the tail's
    // last point says of that stretch.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..2, |tick| EARLIER_BASE + tick);
    let limit = EARLIER_BASE + 60;
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let tail = session
        .earlier_context(earlier_context_last(limit + 4))
        .unwrap();

    let last = tail["earlier_trends"].as_array().unwrap().last().unwrap();
    let mib = 1024.0 * 1024.0;
    assert_eq!(
        last["reach_after"]["leaves"]["memory_active_bytes"].as_array().unwrap()[..2],
        [serde_json::json!(60.0 * mib), serde_json::json!(69.0 * mib)]
    );
}

#[test]
fn makes_each_point_from_the_reading_before_it_in_its_own_run() {
    // Goal: where a recording continues across a collector restart, the point of a minute's
    // first reading takes the reading before it from its own run, though the other run also
    // holds that publication. Method: seal eight minutes of two runs, the second continuing
    // after the first, read the tail, and compare its points with the full-detail context of a
    // second in each run.
    let mut fixture = RingFixture::new_following();
    earlier_publish_minutes(&mut fixture, 0..8, earlier_wall);
    let mut restarted = fixture.next_run();
    earlier_publish_minutes(&mut restarted, 0..8, |tick| earlier_wall(tick) + 8 * 240);
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let tail = session
        .earlier_context(earlier_context_last(EARLIER_LIMIT))
        .unwrap();

    let mut saved = Session::open(&fixture.root).unwrap();
    let first = saved.select(EARLIER_BASE + 1200, 0).unwrap();
    let second = saved.select(EARLIER_BASE + 2400, 0).unwrap();

    let (older, detailed) = earlier_shared(
        &tail["earlier_trends"],
        &[&first["trends"], &second["trends"]],
    );
    assert_eq!(older.len(), 48);
    assert_eq!(older, detailed);
}

#[test]
fn stops_reading_minutes_once_the_kept_readings_pass_the_bound() {
    // Goal: a range holding more readings than a context may hold fails without reading every
    // minute in it. Method: seal five runs of fifteen minutes whose readings all fall at the
    // same 900 seconds, as runs recorded at once would, read the tail twice, and count the
    // saved minutes read. The 69th minute brings the kept readings past OBSERVATIONS_MAX, and
    // the fill stops once it has kept it, having read the rest of its batch of six, the 72nd.
    // CPU history over the same range refuses as well, rather than reaching the assertion past
    // that refusal, which would abort the app.
    let mut fixture = RingFixture::new_following();
    let wall = |tick: u64| EARLIER_BASE + 300 + tick;
    earlier_publish_minutes(&mut fixture, 0..15, wall);
    let mut runs = Vec::new();
    for _ in 0..4 {
        let mut run = fixture.next_run();
        earlier_publish_minutes(&mut run, 0..15, wall);
        runs.push(run);
    }
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    let opened = earlier_reads(&session);

    let first = session.earlier_context(last);
    let second = session.earlier_context(last);
    let history = Session::open(&fixture.root)
        .unwrap()
        .cpu_history_restore(ContextRange::ending_at(wall(899)).unwrap());

    assert!(matches!(first, Err(Error::Bound("context observations"))));
    assert!(matches!(second, Err(Error::Bound("context observations"))));
    assert!(matches!(history, Err(Error::Bound("context observations"))));
    assert_eq!(earlier_reads(&session) - opened, 72);
}

#[test]
fn answers_a_kept_tail_after_its_read_is_canceled() {
    // Goal: a hover that cancels the page's read still gets the tail the session keeps, since
    // answering it reads no saved history. Method: read the tail, cancel the epoch, and read it
    // again.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    let control = Arc::new(Cancellation::default());
    fixture.seal_held();
    let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    let tail = session.earlier_context(last).unwrap();
    control.cancel().unwrap();

    let canceled = session.earlier_context(last).unwrap();

    assert_eq!(canceled, tail);
}

#[test]
fn keeps_the_minutes_a_canceled_fill_finished() {
    // Goal: a canceled read of the tail loses only the minutes of the batch it was reading.
    // Method: cancel once the first minute is kept, read the tail again in the next epoch, and
    // count the saved minutes both reads took. The first batch read six minutes, 0 to 5, so
    // the second read reads minutes 1 to 5 again.
    let mut fixture = RingFixture::new_following();
    earlier_publish(&mut fixture);
    let control = Arc::new(Cancellation::default());
    fixture.seal_held();
    let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
    let cancel = control.clone();
    session.earlier.after_minute = Some(Box::new(move || cancel.cancel().unwrap()));
    let last = earlier_context_last(EARLIER_LIMIT);
    let opened = earlier_reads(&session);

    let canceled = session.earlier_context(last);
    session.begin(control.epoch()).unwrap();
    session.earlier_context(last).unwrap();

    assert!(matches!(canceled, Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(earlier_reads(&session) - opened, 13 + 5);
}

/// Follows ten limits a minute apart from `EARLIER_BASE + 2860`, publishing and saving minutes 12,
/// 13 and 14 in turn before the first, the fourth and the tenth and refreshing the list, and gives
/// each limit's context last second and tail with the saved minutes every session read. Minute 13
/// seals two limits before a tail reaches it, and minute 14 at the first limit whose tail reaches
/// it.
fn earlier_follow(
    fixture: &mut RingFixture,
    session: &mut LiveSession,
) -> (Vec<(u64, Value)>, u64) {
    let seals = [0, 3, 9];
    let mut tails = Vec::new();
    let mut reads = 0;
    for step in 0..10 {
        if let Some(seal) = seals.iter().position(|seal| *seal == step) {
            let minute = 12 + u64::try_from(seal).unwrap();
            earlier_publish_minutes(fixture, minute..minute + 1, earlier_wall);
            session.refresh_history().unwrap();
        }
        let last = earlier_context_last(EARLIER_BASE + 2860 + step * 60);
        let before = earlier_reads(session);
        tails.push((last, session.earlier_context(last).unwrap()));
        reads += earlier_reads(session) - before;
    }

    (tails, reads)
}

/// The points of `older` whose reading one of `detailed` also holds, beside those of
/// `detailed`, each without its segment.
fn earlier_shared(older: &Value, detailed: &[&Value]) -> (Vec<Value>, Vec<Value>) {
    let mut shared_older = Vec::new();
    let mut shared_detailed = Vec::new();
    for point in older.as_array().unwrap() {
        let same = detailed
            .iter()
            .flat_map(|points| points.as_array().unwrap())
            .find(|other| other["wall_ns"] == point["wall_ns"]);
        if let Some(same) = same {
            shared_older.push(earlier_unsegmented(point));
            shared_detailed.push(earlier_unsegmented(same));
        }
    }

    (shared_older, shared_detailed)
}

fn earlier_unsegmented(point: &Value) -> Value {
    let mut point = point.clone();
    let fields = point.as_object_mut().unwrap();
    fields.remove("segment");
    // What the tail adds of the other readings of a stretch, which a point of one reading lacks.
    fields.remove("reach");
    fields.remove("reach_after");
    point
}

/// The wall seconds of `points`, grouped by segment in time order.
fn earlier_segments(points: &Value) -> Vec<Vec<u64>> {
    let mut segments: Vec<(u64, Vec<u64>)> = Vec::new();
    for point in points.as_array().unwrap() {
        let segment = point["segment"].as_u64().unwrap();
        let second = point["wall_ns"].as_u64().unwrap() / NANOSECONDS_PER_SECOND;
        match segments.iter_mut().find(|(number, _)| *number == segment) {
            Some((_, seconds)) => seconds.push(second),
            None => segments.push((segment, vec![second])),
        }
    }

    segments.into_iter().map(|(_, seconds)| seconds).collect()
}

/// The tail a newly opened session reads for the context ending at `last`.
fn earlier_reopened(root: &Path, last: u64) -> Value {
    LiveSession::open(root).unwrap().earlier_context(last).unwrap()
}

/// Removes the saved `window` from the recording's list, as deleting its proof does.
fn earlier_remove(fixture: &RingFixture, window: u64) {
    let run = fixture.writer.identity().unwrap();
    let saved = identity_fixture::saved_window(&fixture.root, &run, window);
    crate::session::reopen_tests::delete_window(&fixture.root, &saved);
}

/// The wall second of `tick`. Each minute holds two runs of thirty consecutive seconds with 180
/// missing seconds between them, and starts one second after the minute before it ends, so
/// fifteen minutes span an hour, every minute starts a stretch, and the reading before it is
/// one second older.
fn earlier_wall(tick: u64) -> u64 {
    let minute = (tick - 1) / 60;
    let offset = (tick - 1) % 60;
    let gap = if offset < 30 { 0 } else { 180 };

    EARLIER_BASE + minute * 240 + offset + gap
}

/// Publishes and saves fifteen minutes of readings at `earlier_wall`.
fn earlier_publish(fixture: &mut RingFixture) {
    earlier_publish_minutes(fixture, 0..15, earlier_wall);
}

/// Publishes and saves `minutes`, each reading at the wall second `wall` gives its tick.
fn earlier_publish_minutes(
    fixture: &mut RingFixture,
    minutes: std::ops::Range<u64>,
    wall: impl Fn(u64) -> u64,
) {
    for minute in minutes {
        earlier_publish_minute(fixture, minute, &wall);
        fixture.seal_through(minute);
    }
}

/// Publishes `minute`, each reading at the wall second `wall` gives its tick.
fn earlier_publish_minute(fixture: &mut RingFixture, minute: u64, wall: impl Fn(u64) -> u64) {
    for offset in 1..=60 {
        let tick = minute * 60 + offset;
        fixture.publish_following_wall(u32::try_from(tick).unwrap(), wall(tick));
    }
}

/// A recording whose first run stopped with minute 5 in its ring and unsaved, and whose next run
/// saved minutes 6 to 14, as when a persister catches up after a restart.
fn earlier_recording_missing_minute_five() -> (RingFixture, RingFixture) {
    let mut older = RingFixture::new_following();
    earlier_publish_minutes(&mut older, 0..5, earlier_wall);
    earlier_publish_minute(&mut older, 5, earlier_wall);
    let mut newer = older.next_run();
    earlier_publish_minutes(&mut newer, 0..9, |tick| earlier_wall(tick + 6 * 60));

    (older, newer)
}

/// The last second of the context whose older tail ends at `limit_second`.
fn earlier_context_last(limit_second: u64) -> u64 {
    limit_second + CHARTED_SECONDS - 1
}

/// The saved minutes the session's history has read.
fn earlier_reads(session: &LiveSession) -> u64 {
    session.history.as_ref().unwrap().source_reads.get()
}

/// The Mac's readings of the stretch at `at` of a store started on `fixture`, once it holds the
/// `seconds` seconds the fixture published.
fn earlier_store_stretch(
    fixture: &RingFixture,
    seconds: usize,
    at: usize,
) -> crate::store::MachineStretch {
    let store = crate::store::Store::start(&fixture.root).unwrap();
    let view = crate::store::tests::view_where(&store, |view| view.seconds.len() == seconds);

    *view.stretches[at].machine
}

#[test]
fn a_stretch_reaches_the_same_readings_live_and_saved() {
    // Goal: what a stretch finds while its minute is live it still finds once that minute is saved,
    // naming the same readings, so a peak neither moves nor goes as it ages out of the ring.
    // Method: a store started on the ring alone and one started after the first minute's seal,
    // over fans that read higher each second, so a stretch's peak is its last reading.
    let mut fixture = RingFixture::new_following();
    for tick in 1..=70 {
        fixture.publish_following_wall(tick, 10_000 + u64::from(tick));
    }
    let live = earlier_store_stretch(&fixture, 70, 3);

    fixture.seal_through(0);
    let saved = earlier_store_stretch(&fixture, 70, 3);

    assert_eq!(saved, live);
    let fans = saved.fan_mean_rpm.measured.unwrap();
    assert_eq!(fans.maximum.place.second(), 10_039);
    assert!(fans.minimum.value < fans.maximum.value);
}

#[test]
fn a_points_reach_holds_no_more_than_its_stretch_however_long_the_recording() {
    // Goal: what a point carries of its stretch is bounded by the stretch and not by how much the
    // recording holds, so a longer recording costs a chart more points and not larger ones.
    // Method: read the tail of a short recording and of one twice as long, and compare the reach
    // of each one's oldest whole stretch.
    let reach_of = |minutes: u64| {
        let mut fixture = RingFixture::new_following();
        earlier_publish_minutes(&mut fixture, 0..minutes, earlier_wall);
        fixture.seal_held();
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let limit = earlier_wall(minutes * 60);
        let tail = session
            .earlier_context(earlier_context_last(limit))
            .unwrap();

        tail["earlier_trends"][1]["reach"].clone()
    };

    let short = reach_of(1);
    let long = reach_of(2);

    assert!(short["places"].as_array().unwrap().len() <= 10);
    assert!(long["places"].as_array().unwrap().len() <= 10);
    assert_eq!(
        short["leaves"].as_object().unwrap().len(),
        long["leaves"].as_object().unwrap().len()
    );
}
