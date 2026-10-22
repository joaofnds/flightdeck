/// The wall second of the first reading the fixture publishes.
const EARLIER_BASE: u64 = 10_000;
/// The limit of the tail most tests read: its readings span minutes 1 to 12, and minute 0 holds
/// the reading before the first of them.
const EARLIER_LIMIT: u64 = EARLIER_BASE + 3000;

#[test]
fn reads_each_minute_of_the_older_tail_once() {
    // Goal: following the newest second reads each sealed minute of the older tail from saved
    // history once. Method: seal an hour of minutes, read the tail, then count the saved
    // sources read for it and for the strides after it that stay inside the same minutes.
    let mut fixture = RingFixture::new_following();
    for window in earlier_publish(&mut fixture) {
        seal_window(&fixture.root, &window);
    }
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

    assert_eq!(filled - opened, 13 * earlier_tables());
    assert_eq!(earlier_reads(&session), filled);
}

#[test]
fn reads_each_minute_once_where_its_manifest_holds_no_wall_bounds() {
    // Goal: a minute the persister sealed without wall bounds, since one of its ticks had no
    // wall time, is read once like any other. Method: seal every minute without bounds, read
    // the tail, and count the saved sources read for it.
    let mut fixture = RingFixture::new_following();
    for window in earlier_publish(&mut fixture) {
        seal_window_with(&fixture.root, &window, None);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let opened = earlier_reads(&session);

    session
        .earlier_context(earlier_context_last(EARLIER_LIMIT))
        .unwrap();

    assert_eq!(earlier_reads(&session) - opened, 13 * earlier_tables());
}

#[test]
fn reads_only_the_sources_of_a_newly_needed_minute() {
    // Goal: a stride whose tail reaches one more sealed minute reads that minute alone. Method:
    // read the tail, move its limit until the join reaches the first reading of minute 13, at
    // EARLIER_BASE + 3120, and count the saved sources read for the move.
    let mut fixture = RingFixture::new_following();
    for window in earlier_publish(&mut fixture) {
        seal_window(&fixture.root, &window);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session
        .earlier_context(earlier_context_last(EARLIER_LIMIT))
        .unwrap();
    let filled = earlier_reads(&session);

    session
        .earlier_context(earlier_context_last(EARLIER_BASE + 3110))
        .unwrap();

    assert_eq!(earlier_reads(&session) - filled, earlier_tables());
}

#[test]
fn reads_each_minute_once_while_minutes_seal() {
    // Goal: a newly sealed minute, and the refreshed list that lists it, make the tail read
    // only the minutes it newly needs. Method: follow the limits `earlier_follow` steps through,
    // which reach minutes 0 to 14 in all, and count the saved sources every session read.
    let mut fixture = RingFixture::new_following();
    let windows = earlier_publish(&mut fixture);
    for window in &windows[..12] {
        seal_window(&fixture.root, window);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let (_, reads) = earlier_follow(&fixture.root, &mut session, &windows[12..]);

    assert_eq!(reads, 15 * earlier_tables());
}

#[test]
fn rereads_only_the_minute_after_a_removed_one() {
    // Goal: removing a minute changes the reading before the next minute's first, so that
    // minute alone is read again. Method: read the tail, remove minute 5's marker, refresh the
    // list, and count the saved sources the next read takes.
    let mut fixture = RingFixture::new_following();
    let windows = earlier_publish(&mut fixture);
    for window in &windows {
        seal_window(&fixture.root, window);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    session.earlier_context(last).unwrap();
    earlier_remove(&fixture.root, &windows[5]);
    session.refresh_history().unwrap();
    let refreshed = earlier_reads(&session);

    session.earlier_context(last).unwrap();

    assert_eq!(earlier_reads(&session) - refreshed, earlier_tables());
}

#[test]
fn reads_a_recovered_minute_and_the_minute_after_it() {
    // Goal: a minute sealed late is read, and so is the minute after it, whose first reading
    // now has the reading before it, and no other. Method: read the tail without minute 5, seal
    // it, refresh the list, and count the saved sources the next read takes.
    let mut fixture = RingFixture::new_following();
    let windows = earlier_publish(&mut fixture);
    for window in windows.iter().filter(|window| window.identity.window != 5) {
        seal_window(&fixture.root, window);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    session.earlier_context(last).unwrap();
    seal_window(&fixture.root, &windows[5]);
    session.refresh_history().unwrap();
    let refreshed = earlier_reads(&session);

    session.earlier_context(last).unwrap();

    assert_eq!(earlier_reads(&session) - refreshed, 2 * earlier_tables());
}

#[test]
fn drops_the_minutes_a_tail_leaves() {
    // Goal: the tail keeps only the minutes its range needs, so going back and returning reads
    // the newest minutes again. Method: read the tail, read it 500 seconds earlier, which leaves
    // minutes 13 and 14, return, and count the saved sources the return takes.
    let mut fixture = RingFixture::new_following();
    for window in earlier_publish(&mut fixture) {
        seal_window(&fixture.root, &window);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let newest = earlier_context_last(EARLIER_BASE + 3400);
    session.earlier_context(newest).unwrap();
    session
        .earlier_context(earlier_context_last(EARLIER_BASE + 2900))
        .unwrap();
    let back = earlier_reads(&session);

    session.earlier_context(newest).unwrap();

    assert_eq!(earlier_reads(&session) - back, 2 * earlier_tables());
}

#[test]
fn numbers_one_segment_for_each_run_of_consecutive_seconds() {
    // Goal: the tail draws one line through each run of consecutive seconds, across the edge
    // of two minutes, and breaks it at each missing stretch. Method: read the tail, whose
    // readings run from second 210 of each minute to second 29 of the next, and group its
    // seconds by segment.
    let mut fixture = RingFixture::new_following();
    for window in earlier_publish(&mut fixture) {
        seal_window(&fixture.root, &window);
    }
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
    let windows = earlier_publish(&mut fixture);
    for window in &windows[..12] {
        seal_window(&fixture.root, window);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let (kept, _) = earlier_follow(&fixture.root, &mut session, &windows[12..]);

    let reopened: Vec<(u64, Value)> = kept
        .iter()
        .map(|(last, _)| (*last, earlier_reopened(&fixture.root, *last)))
        .collect();
    assert_eq!(kept, reopened);
}

#[test]
fn gives_the_tail_a_reopened_session_reads_after_a_minute_is_removed() {
    // Goal: a minute removed from inside the hour leaves the tail and makes the next minute's
    // first point again. Method: read the tail, remove minute 5's marker, refresh the list, and
    // compare the tail with a newly opened session's.
    let mut fixture = RingFixture::new_following();
    let windows = earlier_publish(&mut fixture);
    for window in &windows {
        seal_window(&fixture.root, window);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    let before = session.earlier_context(last).unwrap();
    earlier_remove(&fixture.root, &windows[5]);
    session.refresh_history().unwrap();

    let kept = session.earlier_context(last).unwrap();

    assert_ne!(kept, before);
    assert_eq!(kept, earlier_reopened(&fixture.root, last));
}

#[test]
fn gives_the_tail_a_reopened_session_reads_after_a_minute_is_recovered() {
    // Goal: a minute sealed late, before minutes the tail keeps, joins the tail and makes the
    // next minute's first point again. Method: read the tail without minute 5, seal it, refresh
    // the list, and compare the tail with a newly opened session's.
    let mut fixture = RingFixture::new_following();
    let windows = earlier_publish(&mut fixture);
    for window in windows.iter().filter(|window| window.identity.window != 5) {
        seal_window(&fixture.root, window);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    let before = session.earlier_context(last).unwrap();
    seal_window(&fixture.root, &windows[5]);
    session.refresh_history().unwrap();

    let kept = session.earlier_context(last).unwrap();

    assert_ne!(kept, before);
    assert_eq!(kept, earlier_reopened(&fixture.root, last));
}

#[test]
fn gives_the_tail_a_reopened_session_reads_after_the_limit_moves_back() {
    // Goal: a minute kept only to hold the reading before a needed one gets the point of its own
    // first reading once a tail needs it. Method: read the tail at a limit whose oldest minute
    // is 2, so minute 1 is read without the minute before it, then read the tail 500 seconds
    // earlier, which holds the first reading of minute 1, and compare it with a newly opened
    // session's.
    let mut fixture = RingFixture::new_following();
    for window in earlier_publish(&mut fixture) {
        seal_window(&fixture.root, &window);
    }
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
    for window in earlier_publish(&mut fixture) {
        seal_window(&fixture.root, &window);
    }
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

#[test]
fn makes_each_point_from_the_reading_before_it_in_its_own_run() {
    // Goal: where a recording continues across a collector restart, the point of a minute's
    // first reading takes the reading before it from its own run, though the other run also
    // holds that publication. Method: seal eight minutes of two runs, the second continuing
    // after the first, read the tail, and compare its points with the full-detail context of a
    // second in each run.
    let mut fixture = RingFixture::new_following();
    let windows = earlier_publish(&mut fixture);
    let mut restarted = RingFixture::new_following();
    let recording = windows[0].identity.run.recording;
    let later = earlier_publish_into(&mut restarted, recording, 8, |tick| {
        earlier_wall(tick) + 8 * 240
    });
    for window in windows[..8].iter().chain(&later) {
        seal_window(&fixture.root, window);
    }
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
    // saved sources read. The 69th minute brings the kept readings past OBSERVATIONS_MAX.
    let mut fixture = RingFixture::new_following();
    let wall = |tick: u64| EARLIER_BASE + 300 + tick;
    let recording = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .identity()
        .unwrap()
        .recording;
    let mut windows = earlier_publish_into(&mut fixture, recording, 15, wall);
    let mut runs = Vec::new();
    for _ in 0..4 {
        let mut run = RingFixture::new_following();
        windows.extend(earlier_publish_into(&mut run, recording, 15, wall));
        runs.push(run);
    }
    for window in &windows {
        seal_window(&fixture.root, window);
    }
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    let opened = earlier_reads(&session);

    let first = session.earlier_context(last);
    let second = session.earlier_context(last);

    assert!(matches!(first, Err(Error::Bound("context observations"))));
    assert!(matches!(second, Err(Error::Bound("context observations"))));
    let minutes = u64::try_from(OBSERVATIONS_MAX / 60 + 1).unwrap();
    assert_eq!(earlier_reads(&session) - opened, minutes * earlier_tables());
}

#[test]
fn answers_a_kept_tail_after_its_read_is_canceled() {
    // Goal: a hover that cancels the page's read still gets the tail the session keeps, since
    // answering it reads no saved history. Method: read the tail, cancel the epoch, and read it
    // again.
    let mut fixture = RingFixture::new_following();
    for window in earlier_publish(&mut fixture) {
        seal_window(&fixture.root, &window);
    }
    let control = Arc::new(Cancellation::default());
    let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
    let last = earlier_context_last(EARLIER_LIMIT);
    let tail = session.earlier_context(last).unwrap();
    control.cancel().unwrap();

    let canceled = session.earlier_context(last).unwrap();

    assert_eq!(canceled, tail);
}

#[test]
fn keeps_the_minutes_a_canceled_fill_finished() {
    // Goal: a canceled read of the tail loses only the minute it was reading. Method: cancel
    // once the first minute is kept, read the tail again in the next epoch, and count the saved
    // sources both reads took.
    let mut fixture = RingFixture::new_following();
    for window in earlier_publish(&mut fixture) {
        seal_window(&fixture.root, &window);
    }
    let control = Arc::new(Cancellation::default());
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
    assert_eq!(earlier_reads(&session) - opened, 13 * earlier_tables());
}

/// Follows ten limits a minute apart from `EARLIER_BASE + 2860`, sealing `windows` in turn
/// before the first, the fourth and the tenth and refreshing the list, and gives each limit's
/// context last second and tail with the saved sources every session read. Minute 13 seals two
/// limits before a tail reaches it, and minute 14 at the first limit whose tail reaches it.
fn earlier_follow(
    root: &Path,
    session: &mut LiveSession,
    windows: &[Window],
) -> (Vec<(u64, Value)>, u64) {
    let seals = [0, 3, 9];
    assert_eq!(windows.len(), seals.len());
    let mut tails = Vec::new();
    let mut reads = 0;
    for step in 0..10 {
        if let Some(seal) = seals.iter().position(|seal| *seal == step) {
            seal_window(root, &windows[seal]);
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
    point.as_object_mut().unwrap().remove("segment");
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

/// Removes the sealed `window` from the recording's list, as deleting its marker does.
fn earlier_remove(root: &Path, window: &Window) {
    let marker = persistence::layout::WindowKey::from(&window.identity).marker(root);
    std::fs::remove_file(marker).unwrap();
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

/// Publishes fifteen minutes of readings at `earlier_wall` and gives each minute unsealed.
fn earlier_publish(fixture: &mut RingFixture) -> Vec<Window> {
    let recording = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .identity()
        .unwrap()
        .recording;
    earlier_publish_into(fixture, recording, 15, earlier_wall)
}

/// Publishes `minutes` minutes of readings, each at the wall second `wall` gives its tick, and
/// gives each minute unsealed as a minute of `recording`, so a second fixture's minutes are a
/// later run of the first fixture's recording, as after a collector restart.
fn earlier_publish_into(
    fixture: &mut RingFixture,
    recording: collector::ring::Identity,
    minutes: u64,
    wall: impl Fn(u64) -> u64,
) -> Vec<Window> {
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    let identity = RunIdentity {
        recording,
        ..reader.identity().unwrap()
    };
    let limits = Limits::from_shape(&reader.shape()).unwrap();
    let mut windows = Vec::new();
    for minute in 0..minutes {
        let mut window = Window::new(
            WindowIdentity {
                run: identity,
                window: minute,
            },
            limits,
        );
        for offset in 1..=60 {
            let publication = minute * 60 + offset;
            fixture.publish_following_wall(
                u32::try_from(publication).unwrap(),
                wall(publication),
            );
            follow_restoration_stage(&mut reader, &mut tick, &mut window, publication);
        }
        windows.push(window);
    }

    windows
}

/// The last second of the context whose older tail ends at `limit_second`.
fn earlier_context_last(limit_second: u64) -> u64 {
    limit_second + CHARTED_SECONDS - 1
}

/// The saved sources the session's history has read.
fn earlier_reads(session: &LiveSession) -> u64 {
    session.history.as_ref().unwrap().source_reads.get()
}

/// The saved sources one minute's readings come from.
fn earlier_tables() -> u64 {
    u64::try_from(metrics::TABLE_NAMES.len()).unwrap()
}
