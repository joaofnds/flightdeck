use serde_json::Value;

use super::*;
use crate::live::identity_fixture::Fixture;
use crate::live::window::fixture::{observation_key, saved_only};
use crate::live_app_response::{Key, KeyKind, PAGE_ROWS_MAX, Page};

#[test]
fn part_window_gives_each_saved_second_the_detail_its_sample_gives() {
    // Goal: a hover on the Apps page finds the selected App's detail at a saved second other
    // than the selected one, the same detail a read of that second gives.
    // Method: read the App's detail at the earlier of two saved seconds, select the later one,
    // and keep the App's parts of their window.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let sibling = session.select(1000, 0).unwrap()["selected"].clone();
    let app = selected_app(&session);
    let expected = session.app_detail(app.kind, &app.bytes, 0).unwrap();
    assert_ne!(expected, Value::Null);
    session.select(1001, 0).unwrap();

    session.part_window(app.kind, &app.bytes).unwrap();

    let parts = session.app_parts();
    let part = parts.get(&observation_key(&sibling)).unwrap();
    assert_eq!(part.detail(first_page()).unwrap(), expected);
}

#[test]
fn part_window_keeps_sharing_the_parts_of_a_covered_window() {
    // Goal: reading the same App's window again replays no saved minute and copies no part.
    // Method: keep an App's parts of a saved window, take them, and keep them again.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let app = selected_app(&session);
    session.part_window(app.kind, &app.bytes).unwrap();
    let shared = session.app_parts();

    session.part_window(app.kind, &app.bytes).unwrap();

    assert!(Arc::ptr_eq(&shared, &session.app_parts()));
}

#[test]
fn part_window_drops_the_parts_of_an_app_of_other_bytes() {
    // Goal: a hover never shows the App selected before as the App selected now.
    // Method: keep one App's parts of a saved window, then those of other bytes of its kind.
    assert_part_window_drops_the_parts_kept_before(|app| (app.kind, b"other".to_vec()));
}

#[test]
fn part_window_drops_the_parts_of_an_app_of_the_other_kind() {
    // Goal: a bundle and an executable with the same bytes are two Apps to a hover.
    // Method: keep one App's parts of a saved window, then those of its bytes as the other
    // kind.
    assert_part_window_drops_the_parts_kept_before(|app| {
        let kind = match app.kind {
            KeyKind::Bundle => KeyKind::Executable,
            KeyKind::Executable => KeyKind::Bundle,
        };
        (kind, app.bytes.clone())
    });
}

#[test]
fn poll_parts_gives_each_live_second_the_kept_apps_detail() {
    // Goal: a live second has the selected App's detail as soon as the session polls it.
    // Method: keep an App's parts over one live second, poll a second one, and compare its
    // part with a read of it.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();
    let app = selected_app(&session);
    session.part_window(app.kind, &app.bytes).unwrap();
    fixture.publish_with_system_rows(1001);

    session.poll().unwrap();

    let latest = session.follow_latest().unwrap()["selected"].clone();
    let parts = session.app_parts();
    let part = parts.get(&observation_key(&latest)).unwrap();
    let expected = session.app_detail(app.kind, &app.bytes, 0).unwrap();
    assert_eq!(part.detail(first_page()).unwrap(), expected);
}

#[test]
fn poll_parts_keeps_nothing_until_an_app_is_kept() {
    // Goal: a session whose Apps page never asked for an App spends no memory on parts.
    // Method: poll two live seconds without keeping any App's parts.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);

    let mut session = LiveSession::open(&fixture.root).unwrap();

    let latest = session.follow_latest().unwrap()["selected"].clone();
    assert!(session.app_parts().get(&observation_key(&latest)).is_none());
}

#[test]
fn poll_keeps_the_parts_of_a_selected_earlier_window() {
    // Goal: a page that polls live while an earlier second is selected leaves the kept App's
    // parts of that window, so returning to it replays nothing.
    // Method: keep an App's parts of a saved window, publish a live second over fifteen
    // minutes past it, poll, and look the selected second up.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    fixture.publish_with_system_rows(3000);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    let app = selected_app(&session);
    session.part_window(app.kind, &app.bytes).unwrap();
    fixture.publish_with_system_rows(3001);

    session.poll().unwrap();

    assert!(
        session
            .app_parts()
            .get(&observation_key(&selected))
            .is_some()
    );
}

/// Keeps the selected App's parts of a saved window, then those of the App `other` names, and
/// asserts the selected second holds only the other App's part, which has no detail there.
fn assert_part_window_drops_the_parts_kept_before(other: impl FnOnce(&Key) -> (KeyKind, Vec<u8>)) {
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    let app = selected_app(&session);
    session.part_window(app.kind, &app.bytes).unwrap();
    let (kind, bytes) = other(&app);

    session.part_window(kind, &bytes).unwrap();

    let parts = session.app_parts();
    let part = parts.get(&observation_key(&selected)).unwrap();
    assert!(part.is_of(kind, &bytes));
    assert_eq!(part.detail(first_page()).unwrap(), Value::Null);
}

fn selected_app(session: &LiveSession) -> Key {
    let mut key = None;
    session
        .with_selected_apps(|sample| {
            key = Some(sample.apps()[0].key.clone());
            Ok(Value::Null)
        })
        .unwrap();

    key.unwrap()
}

fn first_page() -> Page {
    Page {
        offset: 0,
        limit: PAGE_ROWS_MAX,
    }
}
