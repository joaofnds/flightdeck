use serde_json::Value;

use super::cache::{publish_exit_only, publish_unknown_program};
use super::{Fixture, LiveSession, usage};
use persistence::Error;

#[test]
fn resident_history_from_the_parts_sealed_windows_keep_is_the_history_their_replay_gives() {
    // Goal: the resident history from the parts each sealed window keeps is the history a
    // replay of the windows' minutes gives, and reads none of the windows' saved sources.
    // Method: seal four minutes with their Apps' parts, take the history, and compare it with
    // the history a session gives once the parts are of a version this build does not read.
    let fixture = sealed_minutes([true; 4]);
    let session = resident_at(&fixture, 1005);

    let (kept, reads) = resident_history(&session);

    assert_eq!(reads, 0);
    assert_eq!(kept["points"].as_array().unwrap().len(), 6);
    assert_eq!(kept, replayed_at(session, &fixture, 1005).0);
}

#[test]
fn resident_history_replays_the_earlier_windows_sealed_without_parts() {
    // Goal: windows sealed without parts before windows that keep them give the history a
    // replay of them all gives, and the windows that keep them are not replayed.
    // Method: seal the first two minutes without parts and the rest with them, and compare the
    // history and the saved sources it read with a replay's.
    assert_mixed_history_is_the_replay([false, false, true, true]);
}

#[test]
fn resident_history_replays_the_later_windows_sealed_without_parts() {
    // Goal: windows sealed without parts after windows that keep them give the history a
    // replay of them all gives, and the windows that keep them are not replayed.
    // Method: seal the first two minutes with parts and the rest without them, and compare the
    // history and the saved sources it read with a replay's.
    assert_mixed_history_is_the_replay([true, true, false, false]);
}

#[test]
fn resident_history_from_the_parts_of_an_earlier_second_is_the_history_its_replay_gives() {
    // Goal: a history ending inside a window that keeps its parts leaves out the observations
    // after its end, as a replay does.
    // Method: seal four minutes with their parts, take the history at the fourth of six
    // observations, and compare it with the replay's history at the same observation.
    let fixture = sealed_minutes([true; 4]);
    let session = resident_at(&fixture, 1003);

    let (kept, _reads) = resident_history(&session);

    assert_eq!(kept["selected"]["stamp"]["publication"], "4");
    assert_eq!(kept, replayed_at(session, &fixture, 1003).0);
}

#[test]
fn resident_history_from_points_cached_from_parts_is_the_history_the_first_query_gave() {
    // Goal: a second query takes the points the first took from the windows' parts out of the
    // cache and gives the same history.
    // Method: take the history twice in one session over windows that keep their parts.
    let fixture = sealed_minutes([true; 4]);
    let session = resident_at(&fixture, 1005);
    let first = session.app_resident_history().unwrap();

    let second = session.app_resident_history().unwrap();

    assert_eq!(second, first);
}

#[test]
fn resident_history_refuses_replaced_parts_a_cached_point_was_taken_from() {
    // Goal: a cached point stays pinned to the parts file it was taken from.
    // Method: take the history over windows that keep their parts, replace the first window's
    // parts with the same bytes under a new inode, and take it again.
    let fixture = sealed_minutes([true; 4]);
    let session = resident_at(&fixture, 1005);
    session.app_resident_history().unwrap();
    let saved = session.history.as_ref().unwrap();
    let path = fixture
        .root
        .join(&saved.windows[0].data_directory)
        .join("app_parts.parquet");
    let original = fixture.root.join("app-parts-original.parquet");
    std::fs::rename(&path, &original).unwrap();
    std::fs::copy(&original, &path).unwrap();

    let result = session.app_resident_history();

    assert!(
        matches!(result, Err(Error::Bound("resident cache file changed"))),
        "{result:?}"
    );
}

#[test]
fn resident_history_refuses_parts_whose_window_marker_disappeared() {
    // Goal: a history from the parts a window keeps revalidates that window's commitment, as a
    // history from its replayed sources does.
    // Method: seal four minutes with their parts, remove the first window's marker, and take
    // the history.
    let fixture = sealed_minutes([true; 4]);
    let session = resident_at(&fixture, 1005);
    let saved = session.history.as_ref().unwrap();
    let marker =
        persistence::layout::WindowKey::from(&saved.windows[0].identity).marker(&fixture.root);
    std::fs::remove_file(&marker).unwrap();

    let result = session.app_resident_history();

    assert!(
        matches!(&result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound),
        "{result:?}"
    );
}

#[test]
fn resident_history_from_parts_counts_only_the_live_processes_outside_every_app() {
    // Goal: the parts keep how many live processes belong to no App, which a process that
    // exited outside every App does not join, as a replay counts them.
    // Method: publish a live process of an unknown program, then its exit with another's,
    // seal both with their parts, and compare the history with a replay's.
    let mut fixture = Fixture::new();
    publish_unknown_program(&mut fixture);
    publish_exit_only(&mut fixture);
    fixture.seal_derived(&[], &crate::SealedApps);
    let session = resident_at(&fixture, 1001);

    let (kept, reads) = resident_history(&session);

    assert_eq!(reads, 0);
    assert_eq!(kept["points"][0]["ungrouped_members"], 1);
    assert_eq!(kept["points"][1]["ungrouped_members"], 0);
    assert_eq!(kept, replayed_at(session, &fixture, 1001).0);
}

#[test]
fn resident_history_from_parts_leaves_out_the_apps_whose_members_all_exited() {
    // Goal: an App the parts keep only for members that exited, with no resident memory,
    // joins neither the cohort nor Other, as a replay leaves it out.
    // Method: publish a measured member, then its exit, seal both with their parts, and compare
    // the history with a replay's.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(11));
    publish_exit_only(&mut fixture);
    fixture.seal_derived(&[], &crate::SealedApps);
    let session = resident_at(&fixture, 1001);

    let (kept, reads) = resident_history(&session);

    assert_eq!(reads, 0);
    assert_eq!(kept["points"][1]["other"]["app_count"], 0);
    assert_eq!(kept, replayed_at(session, &fixture, 1001).0);
}

#[test]
fn resident_history_without_cache_room_is_the_history_a_replay_gives() {
    // Goal: with no room to cache a point, the parts of the sealed windows that keep them and
    // the replay of the others give the history a replay of every minute gives.
    // Method: seal two minutes with their parts and two without, take the history with no
    // cache room, and compare it with a replay's.
    let fixture = sealed_minutes([true, true, false, false]);
    let session = resident_at(&fixture, 1005);
    let live_bytes = session.app_history.total_bytes();

    let uncached = resident_history_within(&session, 0);

    assert_eq!(session.app_history.total_bytes(), live_bytes);
    assert_eq!(uncached, replayed_at(session, &fixture, 1005).0);
}

#[test]
fn resident_history_drops_the_cached_points_a_smaller_cache_room_cannot_hold() {
    // Goal: points cached under a larger room leave the cache once a query has no room for
    // them, and the history stays the one they gave.
    // Method: take the history over windows that keep their parts, then take it again with no
    // cache room, and compare the two and the cache's bytes.
    let fixture = sealed_minutes([true; 4]);
    let session = resident_at(&fixture, 1005);
    let live_bytes = session.app_history.total_bytes();
    let (cached, _reads) = resident_history(&session);

    let uncached = resident_history_within(&session, 0);

    assert_eq!(session.app_history.total_bytes(), live_bytes);
    assert_eq!(uncached, cached);
}

#[test]
fn resident_replay_gives_again_every_observation_left_out_before_a_reset() {
    // Goal: a reset gives back each observation the replay left out, from the windows it
    // replays and from those that keep their parts, as the uncached fallback needs once a
    // cache that covered some of them runs out of room.
    // Method: seal two minutes with their parts and two without, leave out every observation of
    // the replay, reset it, and count the observations a visit gives.
    let fixture = sealed_minutes([true, true, false, false]);
    let session = resident_at(&fixture, 1005);
    let selected = session.selected_in_context().unwrap();
    let saved = session.history.as_ref().unwrap();

    let visited = saved
        .with_app_resident_replay(&selected, |replay| {
            replay.exclude(&session.operation, |_observation| Ok(true))?;
            replay.reset();
            let mut visited = 0;
            saved.visit_app_resident(replay, |_saved| {
                visited += 1;
                Ok(())
            })?;
            assert_eq!(visited, replay.count());
            Ok(serde_json::json!(visited))
        })
        .unwrap();

    assert_eq!(visited, 6);
}

#[test]
fn resident_history_replays_the_windows_whose_parts_an_earlier_build_sealed() {
    // Goal: parts sealed by the build before this one, which kept no count of the live
    // processes outside every App, give way to a replay of their minutes.
    // Method: seal four minutes with their parts, rewrite them to the earlier version, and
    // compare the history and the saved sources it read with a replay's.
    let fixture = sealed_minutes([true; 4]);
    let session = resident_at(&fixture, 1005);
    session
        .history
        .as_ref()
        .unwrap()
        .previous_version_kept_parts();
    drop(session);
    let session = resident_at(&fixture, 1005);

    let (previous, reads) = resident_history(&session);

    let (replayed, replayed_reads) = replayed_at(session, &fixture, 1005);
    assert_eq!(previous, replayed);
    assert_eq!(reads, replayed_reads);
}

/// Asserts that the minutes sealed with their parts where `kept` says so give the history a
/// replay of them all gives, from fewer saved sources.
fn assert_mixed_history_is_the_replay(kept: [bool; 4]) {
    let fixture = sealed_minutes(kept);
    let session = resident_at(&fixture, 1005);

    let (mixed, mixed_reads) = resident_history(&session);

    let (replayed, replayed_reads) = replayed_at(session, &fixture, 1005);
    assert_eq!(mixed, replayed);
    assert!(mixed_reads < replayed_reads);
}

/// Six observations, seconds 1000 to 1005, whose adjacent pairs cross four run minutes, each
/// minute sealed with its Apps' parts where `kept` says so. The measured process's resident
/// memory differs at each observation.
fn sealed_minutes(kept: [bool; 4]) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    for minute in 0..3 {
        for second in 0..2 {
            let index = minute * 2 + second;
            fixture.publish_with_usage(1000 + index, &usage(11 + 7 * index * index));
        }
        if minute < 2 {
            fixture.skip_run_ticks(58);
        }
    }
    for (minute, kept) in (0..).zip(kept) {
        if kept {
            fixture.seal_minute_derived(minute, &crate::SealedApps);
        } else {
            fixture.seal_minute(minute);
        }
    }

    fixture
}

/// A session of `fixture`'s saved windows alone at `second`.
fn resident_at(fixture: &Fixture, second: u64) -> LiveSession {
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(second, 0).unwrap();

    session
}

/// The resident history of `session`, without the bytes its cache takes, since a point from
/// the parts keeps no room for an App without resident memory, and the saved sources the
/// history read past those the selected second's own sample reads.
fn resident_history(session: &LiveSession) -> (Value, u64) {
    let reads = || session.history.as_ref().unwrap().source_reads.get();
    session.selected_in_context().unwrap();
    let first = reads();

    let history = session.app_resident_history().unwrap();

    (without_cache_bytes(history), reads() - first)
}

/// The resident history of `session` whose points from the recording take at most `available`
/// bytes of the cache, without the bytes its cache takes.
fn resident_history_within(session: &LiveSession, available: usize) -> Value {
    let history = session
        .operation
        .run(|| {
            let selected = session.selected_in_context()?;
            session.app_history.resident_response_within(
                &selected,
                session.history.as_ref(),
                &session.operation,
                available,
            )
        })
        .unwrap();

    without_cache_bytes(history)
}

fn without_cache_bytes(mut history: Value) -> Value {
    history["coverage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_bytes")
        .unwrap();

    history
}

/// The resident history at `second` from a session opened in place of `session` once each
/// window's parts are of a version this build does not read, and the saved sources it read.
fn replayed_at(session: LiveSession, fixture: &Fixture, second: u64) -> (Value, u64) {
    session.history.as_ref().unwrap().unread_kept_parts();
    drop(session);
    let replaying = resident_at(fixture, second);

    let (replayed, reads) = resident_history(&replaying);

    assert!(reads > 0);
    (replayed, reads)
}
