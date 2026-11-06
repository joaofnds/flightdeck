//! What a session returns once the retention period has passed some of its readings.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use collector::recording::private::PrivateDirectory;
use collector::ring::{Identity, ObservationTimes};
use persistence::Error;
use persistence::canonical::ReadWork;
use persistence::canonical::catalog::Catalog;
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

    assert!(matches!(page, Err(Error::SecondExpired)));
    assert!(matches!(detail, Err(Error::SecondExpired)));
}

#[test]
fn an_expired_selection_says_so_in_the_words_a_page_shows() {
    // Goal: a page that asks again for a second it holds shows why it cannot, in the user's words
    // rather than as a limit of the store.
    // Method: select a second, expire it, and read the text of the refusal.
    let fixture = saved_seconds();
    let mut session = Session::open(&fixture.root).unwrap();
    session.select(1003, 0).unwrap();
    expire_before(&fixture.root, 5 * NS);

    let refused = session.page("ticks", 0).unwrap_err();

    assert_eq!(refused.to_string(), "the selected second has expired");
}

#[test]
fn a_period_raised_before_removal_shows_what_it_hid_again() {
    // Goal: a shortened period hides before it deletes, so raising it again in time loses nothing.
    // Method: place the cutoff past a second, select it, raise the period, and select it in a
    // session opened after that.
    const YEAR_SECONDS: u64 = 365 * 86_400;
    let fixture = saved_seconds();
    expire_before(&fixture.root, 5 * NS);
    let hidden = Session::open(&fixture.root)
        .unwrap()
        .select(1003, 0)
        .unwrap();

    Period::from_seconds(YEAR_SECONDS)
        .unwrap()
        .write(&PrivateDirectory::open(&fixture.root).unwrap())
        .unwrap();
    let shown = Session::open(&fixture.root)
        .unwrap()
        .select(1003, 0)
        .unwrap();

    assert!(hidden["selected"].is_null());
    assert_eq!(shown["selected"]["wall_ns"], 1003 * NS);
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

/// A session holding a ring's seconds 1001 to 1008, on whose clock the cutoff then moves to
/// `continuous_ns`, with no new reading after it.
fn live_session_with_no_new_reading(fixture: &mut Fixture, continuous_ns: u64) -> LiveSession {
    for second in 1001..=1008 {
        fixture.publish_with_system_rows(second);
    }
    let boot = fixture.writer.identity().unwrap().boot;
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();
    expire_boot_before(&fixture.root, boot, continuous_ns);

    session
}

#[test]
fn a_poll_with_no_new_reading_lets_go_of_what_expired() {
    // Goal: the readings of a ring whose collector stopped leave memory as they expire.
    // Method: hold a ring's seconds, place the cutoff among them, poll with nothing new, and ask
    // for what each read shows.
    let mut fixture = Fixture::new();
    let mut session = live_session_with_no_new_reading(&mut fixture, 5 * NS);

    let changed = session.poll().unwrap();

    let followed = session.follow_latest().unwrap();
    let history = session
        .app_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();
    assert!(changed);
    assert_eq!(trend_seconds(&followed), [1006, 1007, 1008]);
    assert_eq!(history["points"].as_array().unwrap().len(), 3, "{history}");
}

#[test]
fn a_poll_with_nothing_expired_changes_nothing() {
    // Goal: a poll that finds nothing new and nothing expired says so, so no page is redrawn.
    // Method: hold a ring's seconds with the cutoff before them all, and poll.
    let mut fixture = Fixture::new();
    let mut session = live_session_with_no_new_reading(&mut fixture, 0);

    assert!(!session.poll().unwrap());
}

#[test]
fn a_select_with_no_poll_shows_no_reading_that_expired() {
    // Goal: a page that holds a second polls nothing, and what it selects next still expires.
    // Method: hold a ring's seconds, place the cutoff among them, and select with no poll.
    let mut fixture = Fixture::new();
    let mut session = live_session_with_no_new_reading(&mut fixture, 5 * NS);

    let expired = session.select(1005, 0).unwrap();
    let retained = session.select(1007, 0).unwrap();

    assert!(expired["selected"].is_null());
    assert_eq!(trend_seconds(&retained), [1006, 1007, 1008]);
}

#[test]
fn a_ring_that_expires_whole_leaves_nothing_and_lends_the_next_reading_its_rates() {
    // Goal: a ring wholly past the period shows nothing once polled, and the first reading after it
    // still has the rates the last of them gives.
    // Method: hold a ring's seconds, place the cutoff past them all, poll, publish one more second
    // that the period covers, and read it.
    let mut fixture = Fixture::new();
    let mut session = live_session_with_no_new_reading(&mut fixture, 8 * NS);
    session.poll().unwrap();
    let nothing = session.select(1008, 0).unwrap();

    fixture.publish_with_system_rows(1009);
    session.poll().unwrap();
    let next = session.follow_latest().unwrap();

    assert!(nothing["selected"].is_null());
    assert_eq!(trend_seconds(&next), [1009]);
    assert!(next["summary"]["cpu_percent"].is_number(), "{next}");
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

    assert!(matches!(restored, Err(Error::SecondExpired)));
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

    assert!(matches!(apps, Err(Error::SecondExpired)));
}

/// Five readings of three saved minutes, with the tables `derive` derives, on a retention clock
/// that has expired none of them: 1001 at 59 seconds, 1002 at 60 and 1003 at 119, then 1004 at 120
/// and 1005 at 121.
pub(crate) fn saved_across_three_minutes(derive: &dyn persistence::worker::Derive) -> Fixture {
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
pub(crate) fn expire_and_remove(root: &Path, continuous_ns: u64) -> u64 {
    let now = ClockReading {
        boot: Identity([9, 9]),
        continuous_ns: 0,
        wall_ns: CLOCK_NS + continuous_ns,
    };
    Clock::started_at(&now)
        .write(&PrivateDirectory::open(root).unwrap())
        .unwrap();
    let store = persistence::Store::open(root).unwrap();
    let budget = Arc::new(persistence::resources::ByteBudget::new(256 * 1024 * 1024));

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
    // Method: read the last second of the third minute, place the cutoff on it, so the first
    // minute goes and the second stays hidden, and read that second in new sessions.
    let fixture = saved_across_three_minutes(&persistence::sealed_apps::SealedApps::default());
    let (saved, live, points) = reading_of_second(&fixture.root, 1005);

    let removed = expire_and_remove(&fixture.root, 121 * NS);
    let (retained_saved, retained_live, retained_points) = reading_of_second(&fixture.root, 1005);

    assert_eq!(removed, 1);
    assert_eq!(Session::open(&fixture.root).unwrap().windows.len(), 2);
    assert!(saved["cpu_percent"].is_number(), "{saved}");
    assert!(live["cpu_percent"].is_number(), "{live}");
    assert_eq!(retained_saved, saved);
    assert_eq!(retained_live, live);
    assert_eq!(retained_points, points[4..]);
}

#[test]
fn the_first_retained_reading_comes_from_raw_readings_alone() {
    // Goal: no table a minute can rebuild is what makes the first retained reading what it was. A
    // table a manifest lists and a reader cannot find is damage, so the state with none is a
    // recording whose minutes never derived any.
    // Method: as above, over minutes saved with no derived tables.
    let fixture = saved_across_three_minutes(&crate::live::identity_fixture::Underived);
    let (saved, live, points) = reading_of_second(&fixture.root, 1005);

    let removed = expire_and_remove(&fixture.root, 121 * NS);
    let (retained_saved, retained_live, retained_points) = reading_of_second(&fixture.root, 1005);

    assert_eq!(removed, 1);
    assert!(saved["cpu_percent"].is_number(), "{saved}");
    assert_eq!(retained_saved, saved);
    assert_eq!(retained_live, live);
    assert_eq!(retained_points, points[4..]);
}

#[test]
fn the_first_retained_reading_is_what_it_was_when_a_restart_saved_each_minute() {
    // Goal: a first retained reading whose rates a restart took from the minute before keeps them
    // once that minute expires, and shows no expired reading.
    // Method: as above, over minutes each saved through a derivation of its own.
    let fixture = saved_across_three_minutes(&persistence::canonical::ready_fixture::Restarted);
    let (saved, live, points) = reading_of_second(&fixture.root, 1005);

    let removed = expire_and_remove(&fixture.root, 121 * NS);
    let (retained_saved, retained_live, retained_points) = reading_of_second(&fixture.root, 1005);

    assert_eq!(removed, 1);
    assert!(saved["cpu_percent"].is_number(), "{saved}");
    assert_eq!(retained_saved, saved);
    assert_eq!(retained_live, live);
    assert_eq!(retained_points, points[4..]);
}

#[test]
fn an_app_history_reads_on_past_a_minute_removal_took_from_under_its_list() {
    // Goal: a session whose list still names a minute removal took reads an App's history without
    // calling a healthy recording damaged.
    // Method: open a session over minutes saved with no parts, so its history replays them, take a
    // removal pass that leaves the list naming a minute now gone, and ask for the history.
    let fixture = saved_across_three_minutes(&crate::live::identity_fixture::Underived);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1005, 0).unwrap();
    let removed = expire_and_remove(&fixture.root, 121 * NS);

    let history = session
        .app_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();

    assert_eq!(removed, 1);
    assert!(history["restoration_error"].is_null(), "{history}");
}

/// Nine saved minutes of one reading each, at the start of its minute on the continuous clock.
pub(crate) fn saved_across_nine_minutes() -> Fixture {
    let mut fixture = Fixture::new();
    let boot = fixture.writer.identity().unwrap().boot;
    expire_boot_before(&fixture.root, boot, 0);
    for minute in 0..NINE_MINUTES {
        fixture.publish_with_system_rows(FIRST_SECOND + minute);
        fixture.skip_run_ticks(59);
        fixture.seal_minute(minute);
    }

    fixture
}

const NINE_MINUTES: u64 = 9;
const FIRST_SECOND: u64 = 2_000;
const READER_WAIT_MAX: Duration = Duration::from_secs(120);
const NAMES_MAX: usize = 1 << 16;

/// As `expire_and_remove`, with `boot` the boot still running, so its record stays while its
/// readings lie past where another boot's first pass would say it ended. A clock on `boot` far
/// ahead of any real one stands still as `expire_and_remove`'s does.
fn expire_and_remove_while_running(root: &Path, boot: Identity, continuous_ns: u64) -> u64 {
    let now = ClockReading {
        boot,
        continuous_ns: CLOCK_NS,
        wall_ns: CLOCK_NS + continuous_ns,
    };
    Clock::started_at(&now)
        .write(&PrivateDirectory::open(root).unwrap())
        .unwrap();
    let store = persistence::Store::open(root).unwrap();
    let budget = Arc::new(persistence::resources::ByteBudget::new(256 << 20));

    store.retire_expired(&now, &[], &budget).unwrap()
}

/// Moves the cutoff a minute at a time up to the newest reading, with a pass at each, and lets
/// `read_count` go up twice after each, so the second of those reads started after the pass. A
/// read takes far less than the grace physical removal leaves a read under way, so no read sees the
/// clock stand two passes behind what has gone.
fn removal_beside_reads(root: &Path, boot: Identity, read_count: &AtomicU64) -> u64 {
    let mut removed = 0;
    for minute in 1..NINE_MINUTES - 1 {
        removed += expire_and_remove_while_running(root, boot, (minute + 1) * 60 * NS);
        let reads = read_count.load(Ordering::Acquire);
        let waited = Instant::now();
        while read_count.load(Ordering::Acquire) < reads + 2 {
            assert!(
                waited.elapsed() < READER_WAIT_MAX,
                "the reader stopped reading"
            );
            std::thread::yield_now();
        }
    }

    removed
}

/// Every name under `root`, which no recursion walks.
fn names_under(root: &Path) -> BTreeSet<PathBuf> {
    let mut names = BTreeSet::new();
    let mut directories = vec![root.to_path_buf()];
    for _name in 0..NAMES_MAX {
        let Some(directory) = directories.pop() else {
            return names;
        };
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            names.insert(entry.path());
            if entry.metadata().unwrap().is_dir() {
                directories.push(entry.path());
            }
        }
    }

    panic!("a recording holds more directories than a test can walk");
}

#[test]
fn a_read_opened_before_a_pass_reads_on_past_it() {
    // Goal: the grace removal leaves covers a read that opened a pass ago, which still lists the
    // minute that pass could take.
    // Method: open a session before each pass of a clock moving a minute at a time, and read the
    // newest second through it after the pass.
    let fixture = saved_across_nine_minutes();
    let boot = fixture.writer.identity().unwrap().boot;
    let newest = FIRST_SECOND + NINE_MINUTES - 1;
    let mut removed = 0;

    for minute in 1..NINE_MINUTES - 1 {
        let mut session = Session::open(&fixture.root).unwrap();
        removed += expire_and_remove_while_running(&fixture.root, boot, (minute + 1) * 60 * NS);
        session.select(newest, 0).unwrap();
    }

    assert!(removed > 0);
}

#[test]
fn a_reader_opening_beside_removal_meets_no_damage() {
    // Goal: removal goes on beside readers, who meet neither damage nor a missing newest reading.
    // Method: open a session and read the newest second over and over on this thread, while another
    // removes the fixture's minutes oldest first.
    let fixture = saved_across_nine_minutes();
    let root = fixture.root.clone();
    let boot = fixture.writer.identity().unwrap().boot;
    let newest = FIRST_SECOND + NINE_MINUTES - 1;
    let read_count = Arc::new(AtomicU64::new(0));
    let removal = {
        let root = root.clone();
        let read_count = Arc::clone(&read_count);
        std::thread::spawn(move || removal_beside_reads(&root, boot, &read_count))
    };

    let waited = Instant::now();
    while !removal.is_finished() {
        assert!(waited.elapsed() < READER_WAIT_MAX, "removal did not finish");
        let selected = Session::open(&root).unwrap().select(newest, 0).unwrap();
        assert_eq!(trend_seconds(&selected).last(), Some(&newest));
        read_count.fetch_add(1, Ordering::Release);
    }
    let windows_removed = removal.join().unwrap();

    let windows_left = Session::open(&root).unwrap().windows.len();
    assert!(windows_removed > 0);
    assert!(u64::try_from(windows_left).unwrap() < NINE_MINUTES);
}

#[test]
fn a_cancelled_read_leaves_nothing_in_the_way_of_removal_or_the_next_read() {
    // Goal: a read given up holds nothing, so removal goes on and the session goes on reading
    // whatever the period still covers.
    // Method: cancel a page partway through its rows, remove what expired, and read through a new
    // epoch of the same session.
    let fixture = saved_across_three_minutes(&crate::live::identity_fixture::Underived);
    let cancellation = Arc::new(crate::Cancellation::default());
    let mut session = LiveSession::open_with(&fixture.root, Arc::clone(&cancellation), 0).unwrap();
    session.select(1005, 0).unwrap();
    cancellation.interrupt_after_page_row();
    let cancelled = session.page("ticks", 0);

    let removed = expire_and_remove(&fixture.root, 121 * NS);
    session.begin(cancellation.epoch()).unwrap();
    let selected = session.select(1005, 0).unwrap();

    assert!(matches!(
        cancelled,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted
    ));
    assert!(removed > 0);
    assert_eq!(trend_seconds(&selected).last(), Some(&1005));
}

#[test]
fn a_reader_leaves_nothing_on_disk_that_a_crash_could_strand() {
    // Goal: a reader that dies needs no cleanup and pins nothing, since no reader takes a lease or
    // writes a name.
    // Method: read through both kinds of session and compare every name in the recording.
    let fixture = saved_across_three_minutes(&crate::live::identity_fixture::Underived);
    let before = names_under(&fixture.root);

    let mut session = Session::open(&fixture.root).unwrap();
    session.select(1005, 0).unwrap();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.select(1005, 0).unwrap();

    assert_eq!(names_under(&fixture.root), before);
}

/// Damages the named file of the oldest minute of `fixture`, removes that minute, and gives what a
/// new session then says of `newest_second`.
fn session_after_removing_damaged(fixture: &Fixture, file: &str, newest_second: u64) -> Value {
    let budget = Arc::new(persistence::resources::ByteBudget::new(256 * 1024 * 1024));
    let run = fixture.writer.identity().unwrap();
    let mut windows = Catalog::open(&fixture.root, &budget)
        .unwrap()
        .windows_of(&run, &mut ReadWork::default())
        .unwrap();
    windows.sort_by_key(|window| window.publication_first);
    let second_first_ns = windows[1].continuous_ns.unwrap().min;
    let raw = fixture.root.join(&windows[0].data_directory);
    std::fs::write(raw.join(file), b"damaged").unwrap();

    let removed = expire_and_remove(&fixture.root, second_first_ns + PERIOD_SECONDS * NS + 1);
    assert_eq!(removed, 1);

    Session::open(&fixture.root)
        .unwrap()
        .select(newest_second, 0)
        .unwrap()
}

#[test]
fn a_session_reads_on_once_a_minute_whose_ticks_did_not_read_has_gone() {
    // Goal: damage in an expired minute costs a reader nothing once that minute is gone.
    // Method: damage the oldest minute's ticks, remove it, and read the newest second, which lies
    // beside it.
    let fixture = saved_across_nine_minutes();
    let newest_second = FIRST_SECOND + NINE_MINUTES - 1;

    let selected = session_after_removing_damaged(&fixture, "ticks.parquet", newest_second);

    assert_eq!(selected["second"], newest_second);
}

#[test]
fn a_session_reads_on_far_from_a_removed_minute_whose_proof_did_not_read() {
    // Goal: a proof that does not read cannot name its readings, so they stay and read as damage
    // where a read reaches them, and a read of the rest of the recording never does.
    // Method: damage the proof of a minute further back than a session lists, remove it, and read
    // the newest second.
    let mut fixture = Fixture::new();
    let boot = fixture.writer.identity().unwrap().boot;
    expire_boot_before(&fixture.root, boot, 0);
    let newest_second = FIRST_SECOND + 4_000;
    for (minute, second) in [FIRST_SECOND, newest_second - 1, newest_second]
        .into_iter()
        .enumerate()
    {
        fixture.publish_with_system_rows(second);
        fixture.skip_run_ticks(59);
        fixture.seal_minute(u64::try_from(minute).unwrap());
    }

    let selected = session_after_removing_damaged(&fixture, "metadata", newest_second);

    assert_eq!(selected["second"], newest_second);
}

#[test]
fn a_damaged_minute_goes_without_the_minute_a_stepped_wall_clock_put_inside_it() {
    // Goal: readings of the same run that a wall clock stepped back put among a damaged minute's
    // stay when that minute goes.
    // Method: save a minute across walls 100 and 300 seconds and the next at 200, damage the first
    // minute's ticks, remove it, and list the whole clock.
    let mut fixture = Fixture::new();
    let boot = fixture.writer.identity().unwrap().boot;
    expire_boot_before(&fixture.root, boot, 0);
    let clocked = |wall_seconds: u64, continuous_seconds: u64| ObservationTimes {
        wall_ns: wall_seconds * NS,
        continuous_ns: continuous_seconds * NS,
        awake_ns: continuous_seconds * NS,
    };
    fixture.set_next_clocks(Some(clocked(100, 1)));
    fixture.publish_with_system_rows(1);
    fixture.set_next_clocks(Some(clocked(300, 2)));
    fixture.publish_with_system_rows(2);
    fixture.skip_run_ticks(58);
    fixture.seal_minute(0);
    fixture.set_next_clocks(Some(clocked(200, 61)));
    fixture.publish_with_system_rows(3);
    fixture.skip_run_ticks(59);
    fixture.seal_minute(1);
    fixture.set_next_clocks(Some(clocked(400, 121)));
    fixture.publish_with_system_rows(4);
    fixture.skip_run_ticks(59);
    fixture.seal_minute(2);
    let budget = Arc::new(persistence::resources::ByteBudget::new(256 * 1024 * 1024));
    let catalog = Catalog::open(&fixture.root, &budget).unwrap();
    let run = fixture.writer.identity().unwrap();
    let mut windows = catalog.windows_of(&run, &mut ReadWork::default()).unwrap();
    windows.sort_by_key(|window| window.publication_first);
    let raw = fixture.root.join(&windows[0].data_directory);
    std::fs::write(raw.join("ticks.parquet"), b"damaged").unwrap();

    let removed = expire_and_remove(&fixture.root, 61 * NS + PERIOD_SECONDS * NS + 1);
    let listed = catalog
        .known_page(0, u64::MAX, None, &mut ReadWork::default())
        .unwrap();

    assert_eq!(removed, 1);
    let walls: Vec<_> = listed
        .readings
        .iter()
        .map(|reading| reading.wall_ns)
        .collect();
    assert_eq!(walls, [200 * NS, 400 * NS]);
}

#[test]
fn a_minute_with_no_clock_whose_ticks_did_not_read_goes_whole() {
    // Goal: a reading with no clock has its name found too when its window cannot name it.
    // Method: save two minutes with no clocks, damage the first one's ticks, and remove what the
    // retention clock has passed.
    let mut fixture = Fixture::new();
    let boot = fixture.writer.identity().unwrap().boot;
    expire_boot_before(&fixture.root, boot, 0);
    fixture.publish_without_clock();
    fixture.publish_without_clock();
    fixture.skip_run_ticks(58);
    fixture.seal_minute(0);
    fixture.publish_without_clock();
    fixture.skip_run_ticks(59);
    fixture.seal_minute(1);
    let budget = Arc::new(persistence::resources::ByteBudget::new(256 * 1024 * 1024));
    let catalog = Catalog::open(&fixture.root, &budget).unwrap();
    let run = fixture.writer.identity().unwrap();
    let windows = catalog.windows_of(&run, &mut ReadWork::default()).unwrap();
    let oldest = windows.iter().min_by_key(|window| window.publication_first);
    let raw = fixture.root.join(&oldest.unwrap().data_directory);
    std::fs::write(raw.join("ticks.parquet"), b"damaged").unwrap();

    let removed = expire_and_remove(&fixture.root, 200 * NS);
    let listed = catalog
        .unknown_page(None, &mut ReadWork::default())
        .unwrap();

    assert_eq!(removed, 2);
    assert_eq!(listed.readings.len(), 0);
}
