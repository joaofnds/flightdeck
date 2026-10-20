//! The histories of an App and of its process groups that the parts a window keeps give.
use super::tests::{points, selected};
use super::*;
use crate::live::identity_fixture::Fixture;
use crate::live_app_history::GroupTarget;

const CURRENT: Target<'static> = Target {
    kind: KeyKind::Bundle,
    bytes: b"/Apps/Current.app",
};

#[test]
fn restores_the_history_a_replay_gives_from_the_parts_a_window_keeps() {
    // Goal: an App's history from the parts its window keeps is the history a replay of the
    // window's minute gives.
    // Method: seal two observations with their Apps' parts, restore an App's history, and
    // compare it with the history a session restores once the parts are of a version it does
    // not read.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_derived(&[], &SealedApps);
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);

    let kept = session
        .with_app_history(&selected, &CURRENT, points)
        .unwrap();

    assert_eq!(kept[1]["cpu_ns"]["known"], "1");
    assert_eq!(kept, replayed_history(session, &selected, &CURRENT, points));
}

#[test]
fn restores_a_window_that_keeps_parts_without_reading_its_saved_sources() {
    // Goal: an App's history reads none of the ticks, processes or events of a window that
    // keeps its Apps' parts, all of which a replay of its minute reads.
    // Method: seal two observations with their Apps' parts, restore an App's history, and count
    // the saved sources the restore read.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_derived(&[], &SealedApps);
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);

    session
        .with_app_history(&selected, &CURRENT, points)
        .unwrap();

    assert_eq!(session.source_reads.get(), 0);
}

#[test]
fn restores_the_history_of_an_app_that_ran_at_no_second_of_a_window_that_keeps_parts() {
    // Goal: an App that ran at no second of a window has a point at each second without its
    // facts, as a replay gives it.
    // Method: seal two observations with their Apps' parts, and compare the history of an App
    // neither ran with the history a replay gives.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_derived(&[], &SealedApps);
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let absent = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Applications/Absent.app",
    };

    let kept = session
        .with_app_history(&selected, &absent, points)
        .unwrap();

    assert_eq!(kept[1]["members"], Value::Null);
    assert_eq!(kept, replayed_history(session, &selected, &absent, points));
}

#[test]
fn restores_the_process_group_history_a_replay_gives_from_the_parts_a_window_keeps() {
    // Goal: a process group's history from the parts its window keeps is the history a replay
    // gives, with the counts only a group's history shows.
    // Method: seal two observations with their Apps' parts, restore the history of the group
    // of an App's processes named "current", and compare it with the history a replay gives.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_derived(&[], &SealedApps);
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let group = GroupTarget {
        app: CURRENT,
        executable: b"current",
    };

    let kept = session
        .with_app_history(&selected, &group, group_points)
        .unwrap();

    assert_eq!(kept[1]["members"], 1);
    assert_eq!(
        kept,
        replayed_history(session, &selected, &group, group_points)
    );
}

#[test]
fn restores_a_process_group_of_a_window_that_keeps_parts_without_reading_its_saved_sources() {
    // Goal: a process group's history reads none of the ticks, processes or events of a window
    // that keeps its Apps' parts, all of which a replay of its minute reads.
    // Method: seal two observations with their Apps' parts, restore the history of the group
    // of an App's processes named "current", and count the saved sources the restore read.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_derived(&[], &SealedApps);
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let group = GroupTarget {
        app: CURRENT,
        executable: b"current",
    };

    session
        .with_app_history(&selected, &group, group_points)
        .unwrap();

    assert_eq!(session.source_reads.get(), 0);
}

#[test]
fn restores_a_replayed_minute_after_a_minute_that_keeps_parts() {
    // Goal: the first observation of a minute sealed without parts takes its rates from the
    // last observation of the minute before, which keeps its parts, as a replay of both gives.
    // Method: seal run tick 59 with its Apps' parts and tick 60 without, and compare the
    // history of tick 60 with the history a replay of both minutes gives.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_minute_derived(0, &SealedApps);
    fixture.seal_minute(1);
    let session = Session::open(&fixture.root).unwrap();
    let mut selected = selected(&session, 1002);
    selected.run_tick = 60;
    selected.stamp.continuous_ns = Some(60_000_000_000);
    selected.stamp.awake_ns = Some(60_000_000_000);

    let restored = session
        .with_app_history(&selected, &CURRENT, points)
        .unwrap();

    assert_eq!(restored[1]["cpu_ns"]["known"], "1");
    assert_eq!(
        restored,
        replayed_history(session, &selected, &CURRENT, points)
    );
}

#[test]
fn rejects_a_warm_history_from_kept_parts_once_their_window_marker_disappears() {
    // Goal: a history from the parts a window keeps revalidates that window's commitment, as a
    // history from its replayed sources does.
    // Method: restore an App's history from the parts a window keeps, remove that window's
    // marker, and ask for the same history again.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_derived(&[], &SealedApps);
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    session
        .with_app_history(&selected, &CURRENT, points)
        .unwrap();
    let marker =
        persistence::layout::WindowKey::from(&session.windows[0].identity).marker(&fixture.root);
    std::fs::remove_file(&marker).unwrap();

    let result = session.with_app_history(&selected, &CURRENT, points);

    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(session.app_history_cache.borrow().is_none());
}

fn group_points(values: &[TracePoint]) -> Result<Value> {
    crate::live_app_response::bounded(Value::Array(
        values.iter().map(|point| point.json(0, true)).collect(),
    ))
}

/// The history of `target` at `selected` that a replay gives, from a session opened in place
/// of `session` once each window's parts are of a version this build does not read.
fn replayed_history(
    session: Session,
    selected: &Observation,
    target: &dyn Selection,
    consume: fn(&[TracePoint]) -> Result<Value>,
) -> Value {
    session.unread_kept_parts();
    let root = session.root.clone();
    drop(session);
    let replaying = Session::open(&root).unwrap();

    let replayed = replaying
        .with_app_history(selected, target, consume)
        .unwrap();

    assert!(replaying.source_reads.get() > 0);
    replayed
}
