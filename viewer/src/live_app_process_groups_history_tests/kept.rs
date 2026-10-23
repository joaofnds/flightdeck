//! The process groups history that the parts a sealed window keeps give.
use super::{equal, named, retire, usage};
use crate::LiveSession;
use crate::app_process_groups_history::AppProcessGroupsHistory;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::KeyKind;

#[test]
fn restores_the_groups_a_replay_gives_from_the_parts_a_window_keeps() {
    // Goal: an App's process groups history from the parts its window keeps is the history a
    // replay of the window's minute gives.
    // Method: seal two observations of an App running two executables with their Apps' parts,
    // and compare the App's groups with the groups a session restores once the parts are of a
    // version it does not read.
    let mut fixture = Fixture::new();
    publish_app(&mut fixture, &[1, 2]);
    fixture.seal_derived(&[], &persistence::sealed_apps::SealedApps::default());
    retire(&fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let kept = groups(&mut session, 1002);

    assert_eq!(kept.named_group_count, 2);
    assert_eq!(kept.points[1].readings[0].members, 1);
    equal(&kept, &replayed_groups(session, &fixture, 1002));
}

#[test]
fn reads_no_saved_source_for_the_groups_of_a_window_that_keeps_parts() {
    // Goal: an App's process groups history reads none of the ticks, processes or events of a
    // window that keeps its Apps' parts, all of which a replay of its minute reads.
    // Method: seal two observations with their Apps' parts, open the Apps page at the later one,
    // and count the saved sources the App's process groups history reads.
    let mut fixture = Fixture::new();
    publish_app(&mut fixture, &[1, 2]);
    fixture.seal_derived(&[], &persistence::sealed_apps::SealedApps::default());
    retire(&fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1002, 0).unwrap();
    session.apps_page(0).unwrap();
    let before = session.history.as_ref().unwrap().source_reads.get();

    session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(session.history.as_ref().unwrap().source_reads.get(), before);
}

#[test]
fn restores_the_groups_of_an_app_that_ran_at_no_second_of_a_window_that_keeps_parts() {
    // Goal: an App that ran at no second of a window has a point at each second with no member
    // outside its groups, as a replay gives it.
    // Method: seal two observations with their Apps' parts, and compare the groups of an App
    // neither ran with the groups a replay gives.
    let mut fixture = Fixture::new();
    publish_app(&mut fixture, &[1, 2]);
    fixture.seal_derived(&[], &persistence::sealed_apps::SealedApps::default());
    retire(&fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let kept = absent_groups(&mut session);

    assert_eq!(kept.points[1].unnamed_members, Some(0));
    equal(&kept, &replayed(session, &fixture, absent_groups));
}

#[test]
fn restores_a_replayed_minute_after_a_minute_that_keeps_parts() {
    // Goal: the first observation of a minute sealed without parts takes its groups' rates from
    // the last observation of the minute before, which keeps its parts, as a replay of both
    // gives.
    // Method: seal run tick 59 with its Apps' parts and tick 60 without, and compare the groups
    // at tick 60 with the groups a replay of both minutes gives.
    let mut fixture = Fixture::new();
    publish_app(&mut fixture, &[59, 60]);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    fixture.seal_minute(1);
    retire(&fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let restored = groups(&mut session, 1060);

    assert_eq!(restored.points[1].readings[0].members, 1);
    equal(&restored, &replayed_groups(session, &fixture, 1060));
}

/// Publishes, at each run tick of `ticks`, an App whose "worker" and "helper" processes each
/// spent more CPU time than at the tick before.
fn publish_app(fixture: &mut Fixture, ticks: &[u64]) {
    let mut usage = usage();
    for tick in ticks {
        usage.user_ns += 120_000_000;
        let rows = [
            named::member(7, b"worker", b"/A.app", usage),
            named::member(8, b"helper", b"/A.app", usage),
        ];
        named::publish(fixture, *tick, &rows);
    }
}

fn absent_groups(session: &mut LiveSession) -> Box<AppProcessGroupsHistory> {
    session.select(1002, 0).unwrap();
    session
        .app_process_groups_history(KeyKind::Bundle, b"/Absent.app")
        .unwrap()
}

fn groups(session: &mut LiveSession, second: u64) -> Box<AppProcessGroupsHistory> {
    session.select(second, 0).unwrap();
    session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap()
}

/// The groups at `second` that a replay gives, from a session opened in place of `session`
/// once each window's parts are of a version this build does not read.
fn replayed_groups(
    session: LiveSession,
    fixture: &Fixture,
    second: u64,
) -> Box<AppProcessGroupsHistory> {
    replayed(session, fixture, |replaying| groups(replaying, second))
}

/// What `read` gives a session opened in place of `session` once each window's parts are of a
/// version this build does not read.
fn replayed(
    session: LiveSession,
    fixture: &Fixture,
    read: impl FnOnce(&mut LiveSession) -> Box<AppProcessGroupsHistory>,
) -> Box<AppProcessGroupsHistory> {
    session.history.as_ref().unwrap().unread_kept_parts();
    drop(session);
    let mut replaying = LiveSession::open(&fixture.root).unwrap();

    let replayed = read(&mut replaying);

    assert!(replaying.history.as_ref().unwrap().source_reads.get() > 0);
    replayed
}
