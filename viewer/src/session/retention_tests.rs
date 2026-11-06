//! What a session returns once the retention period has passed some of its readings.
use std::path::Path;

use collector::recording::private::PrivateDirectory;
use collector::ring::Identity;
use persistence::Error;
use persistence::retention::{Clock, ClockReading, Period, boot_offset_save};
use serde_json::Value;

use super::{NS, Session};
use crate::LiveSession;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::KeyKind;

/// A clock ahead of any wall clock, saved by another boot, so it stands still while a test reads.
const CLOCK_NS: u64 = 1 << 62;
const PERIOD_SECONDS: u64 = 60;

/// A saved minute whose readings of seconds 1001 to 1008 lie 1 second apart on the continuous
/// clock from 0, so reading 1001 + n is at n seconds. A fixture's ring holds eight readings.
pub(crate) fn saved_seconds() -> Fixture {
    let mut fixture = Fixture::new();
    for second in 1001..=1008 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal();

    fixture
}

/// Places the retention cutoff at `continuous_ns` on the clock of the first window's boot.
pub(crate) fn expire_before(root: &Path, continuous_ns: u64) {
    let boot = Session::open(root).unwrap().windows[0].identity.run.boot;
    expire_boot_before(root, boot, continuous_ns);
}

/// Places the retention cutoff at `continuous_ns` on the clock of `boot`.
pub(crate) fn expire_boot_before(root: &Path, boot: Identity, continuous_ns: u64) {
    let directory = PrivateDirectory::open(root).unwrap();
    let offset_ns = CLOCK_NS - PERIOD_SECONDS * NS - continuous_ns;
    boot_offset_save(&directory, boot, offset_ns).unwrap();
    Period::from_seconds(PERIOD_SECONDS)
        .unwrap()
        .write(&directory)
        .unwrap();
    // The clock goes last, so a store reading along the way never finds it ahead of the offset.
    Clock::started_at(&ClockReading {
        boot: Identity([9, 9]),
        continuous_ns: 0,
        wall_ns: CLOCK_NS,
    })
    .write(&directory)
    .unwrap();
}

fn trend_seconds(selected: &Value) -> Vec<u64> {
    selected["trends"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| point["wall_ns"].as_u64().unwrap() / NS)
        .collect()
}

#[test]
fn a_session_returns_no_reading_the_period_no_longer_covers() {
    // Goal: inside a partly expired minute, a second before the cutoff opens to nothing and a
    // second after it opens with only retained readings around it.
    // Method: place the cutoff inside the one saved minute, and select a second either side.
    let fixture = saved_seconds();
    expire_before(&fixture.root, 5 * NS);
    let mut session = Session::open(&fixture.root).unwrap();

    let expired = session.select(1005, 0).unwrap();
    let retained = session.select(1006, 0).unwrap();

    assert!(expired["selected"].is_null());
    assert_eq!(retained["selected"]["wall_ns"], 1006 * NS);
    assert_eq!(trend_seconds(&retained), [1006, 1007, 1008]);
}

#[test]
fn the_first_retained_reading_keeps_the_rates_its_hidden_predecessor_gives() {
    // Goal: hiding a reading changes nothing about the reading after it, whose rates come from it.
    // Method: read a second's summary, expire the second before it, and read the summary again.
    let fixture = saved_seconds();
    let before = Session::open(&fixture.root)
        .unwrap()
        .select(1006, 0)
        .unwrap();
    expire_before(&fixture.root, 5 * NS);

    let after = Session::open(&fixture.root)
        .unwrap()
        .select(1006, 0)
        .unwrap();

    assert!(before["summary"]["cpu_percent"].is_number(), "{before}");
    assert_eq!(after["summary"], before["summary"]);
}

#[test]
fn a_context_read_again_leaves_out_what_expired_since() {
    // Goal: the context a select keeps stops being taken again once a reading of it expires.
    // Method: select a second, move the cutoff into the context it read, and select it again.
    let fixture = saved_seconds();
    let mut session = Session::open(&fixture.root).unwrap();
    let whole = session.select(1006, 0).unwrap();
    expire_before(&fixture.root, 5 * NS);

    let again = session.select(1006, 0).unwrap();

    assert!(trend_seconds(&whole).contains(&1005));
    assert_eq!(trend_seconds(&again), [1006, 1007, 1008]);
}

#[test]
fn a_selection_says_its_second_expired_when_it_asks_again() {
    // Goal: a held selection pins nothing, and says why its second no longer reads.
    // Method: select a second, expire it, and ask for its table and its CPU detail.
    let fixture = saved_seconds();
    let mut session = Session::open(&fixture.root).unwrap();
    session.select(1003, 0).unwrap();
    expire_before(&fixture.root, 5 * NS);

    let page = session.page("ticks", 0);
    let detail = session.cpu_detail();

    assert!(matches!(
        page,
        Err(Error::Bound("the selected second has expired"))
    ));
    assert!(matches!(
        detail,
        Err(Error::Bound("the selected second has expired"))
    ));
}

#[test]
fn the_catalog_begins_at_the_oldest_second_the_period_covers() {
    // Goal: the recording's first second is one a read can still open, while the expired minute
    // stays listed until physical removal reaches it.
    // Method: place the cutoff inside the saved minute and read the catalog.
    let fixture = saved_seconds();
    expire_before(&fixture.root, 5 * NS);

    let catalog = Session::open(&fixture.root).unwrap().catalog();

    assert_eq!(catalog["first_second"], 1006);
    assert_eq!(catalog["last_second"], 1008);
}

#[test]
fn a_recording_wholly_past_the_period_opens_to_its_newest_second_and_no_reading() {
    // Goal: a recording with nothing left to show still opens, with no reading to return.
    // Method: place the cutoff past every reading and select the newest second.
    let fixture = saved_seconds();
    expire_before(&fixture.root, 100 * NS);
    let mut session = Session::open(&fixture.root).unwrap();

    let selected = session.select(1008, 0).unwrap();

    assert_eq!(session.catalog()["first_second"], 1008);
    assert!(selected["selected"].is_null());
    assert_eq!(trend_seconds(&selected), [0_u64; 0]);
}

/// A live session that read seven unsaved seconds, 1001 to 1007, whose first five then expire,
/// and that has since read the eighth.
fn live_session_past_a_cutoff(fixture: &mut Fixture) -> LiveSession {
    for second in 1001..=1007 {
        fixture.publish_with_system_rows(second);
    }
    let boot = fixture.writer.identity().unwrap().boot;
    let mut session = LiveSession::open(&fixture.root).unwrap();
    expire_boot_before(&fixture.root, boot, 5 * NS);
    fixture.publish_with_system_rows(1008);
    session.poll().unwrap();

    session
}

#[test]
fn a_live_session_lets_go_of_a_ring_reading_as_it_expires() {
    // Goal: readings the ring still holds and no minute has saved expire like saved ones.
    // Method: read unsaved seconds, place the cutoff among them, read one more, and select.
    let mut fixture = Fixture::new();
    let mut session = live_session_past_a_cutoff(&mut fixture);

    let selected = session.select(1007, 0).unwrap();
    let expired = session.select(1005, 0).unwrap();

    assert_eq!(trend_seconds(&selected), [1006, 1007, 1008]);
    assert!(expired["selected"].is_null());
}

#[test]
fn a_live_session_follows_no_ring_reading_the_period_no_longer_covers() {
    // Goal: following the newest second draws no expired reading.
    // Method: as above, and follow.
    let mut fixture = Fixture::new();
    let mut session = live_session_past_a_cutoff(&mut fixture);

    let followed = session.follow_latest().unwrap();

    assert_eq!(trend_seconds(&followed), [1006, 1007, 1008]);
}

#[test]
fn a_live_session_shows_no_app_history_the_period_no_longer_covers() {
    // Goal: the App history a live session keeps in memory expires with its readings.
    // Method: read unsaved seconds, place the cutoff among them, read one more, and read an App's
    // history of the newest second.
    let mut fixture = Fixture::new();
    let mut session = live_session_past_a_cutoff(&mut fixture);
    session.follow_latest().unwrap();

    let history = session
        .app_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();

    assert_eq!(history["points"].as_array().unwrap().len(), 3, "{history}");
}

#[test]
fn a_live_session_follows_no_restored_reading_once_it_expires() {
    // Goal: readings a live session restored from saved minutes leave its chart as they expire,
    // though no new reading has come in and its range has not moved.
    // Method: follow a saved minute, place the cutoff inside it, and follow again.
    let fixture = saved_seconds();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let whole = session.follow_latest().unwrap();
    expire_before(&fixture.root, 5 * NS);

    let again = session.follow_latest().unwrap();

    assert_eq!(trend_seconds(&whole).len(), 8);
    assert_eq!(trend_seconds(&again), [1006, 1007, 1008]);
}

#[test]
fn a_ring_wholly_past_the_period_opens_with_nothing_to_follow() {
    // Goal: a ring its collector left long ago opens, and offers no reading it holds.
    // Method: publish seconds, place the cutoff past them all, open, and follow.
    let mut fixture = Fixture::new();
    for second in 1001..=1008 {
        fixture.publish_with_system_rows(second);
    }
    let boot = fixture.writer.identity().unwrap().boot;
    expire_boot_before(&fixture.root, boot, 100 * NS);
    let mut session = LiveSession::open(&fixture.root).unwrap();

    session.poll().unwrap();
    let followed = session.follow_latest();
    let selected = session.select(1008, 0).unwrap();

    assert!(matches!(followed, Err(Error::Bound("no live observation"))));
    assert!(selected["selected"].is_null());
}

#[test]
fn a_window_wholly_past_the_cutoff_is_not_read() {
    // Goal: physical removal can take a wholly expired window from under a session listing it.
    // Method: save two minutes, expire the first and the first reading of the second, take away
    // the first's ticks, and select a second whose context reaches back over them.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    for second in 1001..=1004 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    let oldest = Session::open(&fixture.root).unwrap().windows[0].clone();
    expire_before(&fixture.root, 61 * NS);
    let mut session = Session::open(&fixture.root).unwrap();
    std::fs::remove_file(
        fixture
            .root
            .join(&oldest.data_directory)
            .join("ticks.parquet"),
    )
    .unwrap();

    let selected = session.select(1003, 0).unwrap();

    assert_eq!(session.windows.len(), 2);
    assert_eq!(trend_seconds(&selected), [1003, 1004]);
}

#[test]
fn a_selection_restored_after_its_second_expired_says_so() {
    // Goal: a hold carried across a reopen says why its second no longer reads.
    // Method: select a second, expire it, and restore the selection in a new session.
    let fixture = saved_seconds();
    let selected = Session::open(&fixture.root)
        .unwrap()
        .select(1003, 0)
        .unwrap()["selected"]
        .clone();
    expire_before(&fixture.root, 5 * NS);

    let restored = Session::open(&fixture.root)
        .unwrap()
        .restore_selection(&selected);

    assert!(matches!(
        restored,
        Err(Error::Bound("the selected second has expired"))
    ));
}

#[test]
fn a_selected_second_gives_no_apps_once_it_has_expired() {
    // Goal: an App table asked for after its second expired reads nothing and says why.
    // Method: select a second, expire it, and ask for its Apps from a session with no sample yet.
    let fixture = saved_seconds();
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1003, 0).unwrap()["selected"].clone();
    expire_before(&fixture.root, 5 * NS);

    let apps = session.with_app_sample(&selected, |_sample| Ok(Value::Null));

    assert!(matches!(
        apps,
        Err(Error::Bound("the selected second has expired"))
    ));
}

/// Five readings of three saved minutes, with the tables `derive` derives, on a retention clock
/// that has expired none of them: 1001 at 59 seconds, 1002 at 60 and 1003 at 119, then 1004 at 120
/// and 1005 at 121.
fn saved_across_three_minutes(derive: &dyn persistence::worker::Derive) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    let boot = fixture.writer.identity().unwrap().boot;
    expire_boot_before(&fixture.root, boot, 0);
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1003);
    fixture.publish_with_system_rows(1004);
    fixture.publish_with_system_rows(1005);
    fixture.seal_minute_derived(0, derive);
    fixture.seal_minute_derived(1, derive);
    fixture.seal_minute_derived(2, derive);

    fixture
}

/// Moves the clock `saved_across_three_minutes` placed until the cutoff stands at `continuous_ns`,
/// takes a pass of physical removal, and says how many publications it removed.
fn expire_and_remove(root: &Path, continuous_ns: u64) -> u64 {
    let now = ClockReading {
        boot: Identity([9, 9]),
        continuous_ns: 0,
        wall_ns: CLOCK_NS + continuous_ns,
    };
    Clock::started_at(&now)
        .write(&PrivateDirectory::open(root).unwrap())
        .unwrap();
    let store = persistence::Store::open(root).unwrap();
    let budget = std::sync::Arc::new(persistence::resources::ByteBudget::new(256 * 1024 * 1024));

    store.retire_expired(&now, &[], &budget).unwrap()
}

/// A point without its segment, which numbers the stretches of a chart, so the same reading has
/// another number once the stretches before it have gone.
fn without_segment(mut point: Value) -> Value {
    point.as_object_mut().unwrap().remove("segment");

    point
}

/// What new readers say of `second`: its summary from the saved minutes alone, its summary with
/// the ring as well, and the App's history.
fn reading_of_second(root: &Path, second: u64) -> (Value, Value, Vec<Value>) {
    let saved = Session::open(root).unwrap().select(second, 0).unwrap();
    let mut session = LiveSession::open(root).unwrap();
    let selected = session.select(second, 0).unwrap();
    let history = session
        .app_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();

    (
        without_segment(saved["summary"].clone()),
        without_segment(selected["summary"].clone()),
        history["points"]
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(without_segment)
            .collect(),
    )
}

#[test]
fn the_first_retained_reading_stays_the_same_while_its_hidden_predecessor_keeps_a_minute() {
    // Goal: the first reading of a minute takes its rates from the last of the minute before, so
    // that minute stays, hidden, and the reading reads as it did.
    // Method: read the first second of the second minute, place the cutoff on it, take a removal
    // pass, and read that second in new sessions.
    let fixture = saved_across_three_minutes(&persistence::sealed_apps::SealedApps::default());
    let (saved, live, points) = reading_of_second(&fixture.root, 1002);

    let removed = expire_and_remove(&fixture.root, 60 * NS);
    let (retained_saved, retained_live, retained_points) = reading_of_second(&fixture.root, 1002);

    assert_eq!(removed, 0);
    assert!(saved["cpu_percent"].is_number(), "{saved}");
    assert!(live["cpu_percent"].is_number(), "{live}");
    assert_eq!(retained_saved, saved);
    assert_eq!(retained_live, live);
    assert_eq!(retained_points, points[1..]);
}

#[test]
fn the_first_retained_reading_stays_the_same_once_an_expired_minute_is_gone() {
    // Goal: removing a minute no retained reading needs changes nothing the period covers.
    // Method: read the first second of the third minute, place the cutoff on it, so the first
    // minute goes and the second stays hidden, and read that second in new sessions.
    let fixture = saved_across_three_minutes(&persistence::sealed_apps::SealedApps::default());
    let (saved, live, points) = reading_of_second(&fixture.root, 1004);

    let removed = expire_and_remove(&fixture.root, 120 * NS);
    let (retained_saved, retained_live, retained_points) = reading_of_second(&fixture.root, 1004);

    assert_eq!(removed, 1);
    assert_eq!(Session::open(&fixture.root).unwrap().windows.len(), 2);
    assert!(saved["cpu_percent"].is_number(), "{saved}");
    assert!(live["cpu_percent"].is_number(), "{live}");
    assert_eq!(retained_saved, saved);
    assert_eq!(retained_live, live);
    assert_eq!(retained_points, points[3..]);
}

#[test]
fn the_first_retained_reading_comes_from_raw_readings_alone() {
    // Goal: no table a minute can rebuild is what makes the first retained reading what it was. A
    // table a manifest lists and a reader cannot find is damage, so the state with none is a
    // recording whose minutes never derived any.
    // Method: as above, over minutes saved with no derived tables.
    let fixture = saved_across_three_minutes(&crate::live::identity_fixture::Underived);
    let (saved, live, points) = reading_of_second(&fixture.root, 1004);

    let removed = expire_and_remove(&fixture.root, 120 * NS);
    let (retained_saved, retained_live, retained_points) = reading_of_second(&fixture.root, 1004);

    assert_eq!(removed, 1);
    assert!(saved["cpu_percent"].is_number(), "{saved}");
    assert_eq!(retained_saved, saved);
    assert_eq!(retained_live, live);
    assert_eq!(retained_points, points[3..]);
}
