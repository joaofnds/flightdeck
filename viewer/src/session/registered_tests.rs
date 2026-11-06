//! A session over a registered recording, saved through the persister's own worker.

use std::path::PathBuf;
use std::sync::Arc;

use persistence::Error;
use persistence::canonical::ReadWork;
use persistence::canonical::catalog::Catalog;
use persistence::resources::ByteBudget;

use super::Session;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::KeyKind;
use crate::root_fixture::{older_root, unfinished_root};

/// The entries a boundary lookup of the canonical tree examines at most.
const ENTRIES_OF_A_BOUNDARY_MAX: u64 = 4_224;

const SECOND_FIRST: u64 = 1000;

/// Two minutes saved: the run's ticks 58 and 59, then its ticks 60 and 61.
fn registered_two_minutes() -> Fixture {
    let mut fixture = Fixture::registered();
    fixture.start_at_run_tick(58);
    for offset in 0..4 {
        fixture.publish_with_system_rows(SECOND_FIRST + offset);
    }
    fixture.seal();
    fixture
}

#[test]
fn a_session_refuses_a_recording_of_an_older_format() {
    // Goal: a recording this version cannot read is refused and never reads as an empty one.
    // Method: open a session over a root as the previous format left it.
    let root = older_root();

    let opened = Session::open(&root);

    assert!(
        matches!(opened, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_session_finds_no_recording_in_a_root_still_being_set_up() {
    // Goal: the last moment of a first launch reads as a recording not there yet.
    // Method: open a session over a root whose setup stopped after its identity.
    let root = unfinished_root();

    let opened = Session::open(&root);

    assert!(
        matches!(opened, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_session_opens_a_registered_recording_at_its_two_ends() {
    // Goal: a session reaches a registered recording through its catalog alone (P02.1).
    // Method: save two minutes through the worker and open a session over them.
    let fixture = registered_two_minutes();

    let session = Session::open(&fixture.root).unwrap();

    let catalog = session.catalog();
    assert_eq!(catalog["first_second"], SECOND_FIRST);
    assert_eq!(catalog["last_second"], SECOND_FIRST + 3);
    assert_eq!(catalog["unknown_clock_ticks"], 0);
    assert_eq!(session.windows.len(), 2);
}

#[test]
fn a_selection_reads_a_registered_window_with_its_rates() {
    // Goal: a selected second reads the same way from a registered window, and the first reading of
    // a minute takes its rates from the minute before it (P02.5).
    // Method: save two minutes and select the first second of the second minute.
    let fixture = registered_two_minutes();
    let mut session = Session::open(&fixture.root).unwrap();

    let selected = session.select(SECOND_FIRST + 2, 0).unwrap();

    assert_eq!(selected["selected"]["publication"], 3);
    assert_eq!(selected["trends"].as_array().unwrap().len(), 4);
    assert!(selected["summary"]["cpu_percent"].is_number());
}

#[test]
fn runs_sharing_a_second_both_stay_selectable() {
    // Goal: two runs on one wall second are two readings, neither hiding the other, as with a clock
    // set back between runs (P02.1, P02.5).
    // Method: save a run of the recording and then a second run over the same seconds, open a
    // session, and select the second of each.
    let first = registered_two_minutes();
    let mut second = Fixture::new_mode(true, 2, Some(&first));
    second.start_at_run_tick(58);
    for offset in 0..4 {
        second.publish_with_system_rows(SECOND_FIRST + offset);
    }
    second.seal();
    let mut session = Session::open(&first.root).unwrap();

    let one = session.select(SECOND_FIRST + 1, 0).unwrap();
    let other = session.select(SECOND_FIRST + 1, 1).unwrap();

    assert_eq!(session.windows.len(), 4);
    assert_eq!(one["alternatives"].as_array().unwrap().len(), 2);
    assert_ne!(one["selected"]["run"], other["selected"]["run"]);
    assert_eq!(
        one["selected"]["publication"],
        other["selected"]["publication"]
    );
}

#[test]
fn a_reading_with_no_clock_is_counted_and_its_window_still_reads() {
    // Goal: a window with partly missing clocks is found through the readings that have one, and
    // the rest are counted rather than lost (P02.5).
    // Method: save a minute of a reading with clocks and one without, and select the first.
    let mut fixture = Fixture::registered();
    fixture.publish_with_system_rows(SECOND_FIRST);
    fixture.publish_without_clock();
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();

    let selected = session.select(SECOND_FIRST, 0).unwrap();

    assert_eq!(selected["selected"]["publication"], 1);
    assert_eq!(session.catalog()["unknown_clock_ticks"], 1);
}

#[test]
fn readings_with_no_clock_at_all_are_counted_with_no_second_to_show() {
    // Goal: readings no clock can place still count, and a recording of nothing else has no ends to
    // offer rather than wrong ones (P02.5).
    // Method: save a minute of readings without clocks and open a session.
    let mut fixture = Fixture::registered();
    fixture.publish_without_clock();
    fixture.publish_without_clock();
    fixture.seal();

    let session = Session::open(&fixture.root).unwrap();

    let catalog = session.catalog();
    assert_eq!(catalog["first_second"], serde_json::Value::Null);
    assert_eq!(catalog["unknown_clock_ticks"], 2);
}

/// Three minutes saved, the first a long way before the others on the clock and out of reach of
/// their span, though the run's ticks follow on.
const SECOND_FAR: u64 = SECOND_FIRST + 100_000;

fn registered_far_apart() -> Fixture {
    let mut fixture = Fixture::registered();
    fixture.publish_with_system_rows(SECOND_FIRST);
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(SECOND_FAR);
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(SECOND_FAR + 60);
    fixture.seal();
    fixture
}

/// The numbers of the windows `session` lists.
fn registered_numbers(session: &Session) -> Vec<u64> {
    session
        .windows
        .iter()
        .map(|manifest| manifest.identity.window)
        .collect()
}

/// The proof of the listed window with number `window`.
fn registered_proof(session: &Session, window: u64) -> PathBuf {
    let manifest = session
        .windows
        .iter()
        .find(|manifest| manifest.identity.window == window)
        .unwrap();

    persistence::commitment_path(&session.root, manifest)
}

#[test]
fn a_session_lists_only_the_windows_of_the_span_it_reads() {
    // Goal: what a session holds follows the span it reads and never what the recording saved, and
    // the ends of the recording stay known (P02.1, P02.2).
    // Method: save three minutes, the first far before the others, open a session, then select a
    // second of each end.
    let fixture = registered_far_apart();
    let mut session = Session::open(&fixture.root).unwrap();
    let opened = registered_numbers(&session);
    let catalog = session.catalog();

    let old = session.select(SECOND_FIRST, 0).unwrap();
    let at_old = registered_numbers(&session);
    let new = session.select(SECOND_FAR + 60, 0).unwrap();

    assert_eq!(opened, [1, 2]);
    assert_eq!(catalog["first_second"], SECOND_FIRST);
    assert_eq!(catalog["last_second"], SECOND_FAR + 60);
    assert_eq!(old["selected"]["publication"], 1);
    assert_eq!(at_old, [0]);
    assert_eq!(new["selected"]["publication"], 3);
    assert_eq!(registered_numbers(&session), [1, 2]);
}

#[test]
fn a_read_past_the_listed_span_fails_rather_than_finding_nothing() {
    // Goal: a session lists one span, and a read outside it that found no readings would draw a gap
    // in the recording where there is none (P02.5).
    // Method: save minutes far apart, open a session at the newest, and read a range of the oldest.
    let fixture = registered_far_apart();
    let session = Session::open(&fixture.root).unwrap();
    let range = super::ContextRange::ending_at(SECOND_FIRST).unwrap();

    let read = session.context(range, &mut Vec::new());

    assert!(matches!(read, Err(Error::Bound(_))));
}

#[test]
fn a_read_of_kept_parts_past_the_listed_span_fails_rather_than_finding_nothing() {
    // Goal: the tables a minute keeps for the App histories answer for the same span (P02.5).
    // Method: save minutes far apart, open a session at the newest, and read parts of the oldest.
    let fixture = registered_far_apart();
    let session = Session::open(&fixture.root).unwrap();
    let range = super::ContextRange::ending_at(SECOND_FIRST).unwrap();

    let read = session.visit_sealed_parts(range, KeyKind::Executable, b"app", |_part| Ok(()));

    assert!(matches!(read, Err(Error::Bound(_))));
}

#[test]
fn an_unavailable_old_window_stops_no_reader_of_a_newer_one() {
    // Goal: damage outside what a reader asks for cannot stop it, and damage it asks for is
    // explicit (P02.3).
    // Method: save three minutes far apart, damage the oldest one's proof, then open a session and
    // select a second of the newest and of the oldest.
    let fixture = registered_far_apart();
    let mut session = Session::open(&fixture.root).unwrap();
    session.select(SECOND_FIRST, 0).unwrap();
    std::fs::write(registered_proof(&session, 0), b"damage").unwrap();

    let mut reopened = Session::open(&fixture.root).unwrap();
    let new = reopened.select(SECOND_FAR + 60, 0);
    let old = reopened.select(SECOND_FIRST, 0);

    assert_eq!(new.unwrap()["selected"]["publication"], 3);
    assert!(old.is_err());
}

#[test]
fn damage_in_the_margin_of_a_span_stops_no_reader_of_what_it_needs() {
    // Goal: a span's margin is only there to save a load later, so damage in it costs a reader
    // nothing it asked for, and it is explicit once a reader asks for it (P02.3).
    // Method: save minutes further apart than a read needs and closer than its margin, damage the
    // older one's proof, select a second of the newer one, and then one of the older.
    const NEEDED_SECONDS: u64 = 3660;
    let newer = SECOND_FIRST + NEEDED_SECONDS + 300;
    let mut fixture = Fixture::registered();
    fixture.publish_with_system_rows(SECOND_FIRST);
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(newer);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    std::fs::write(registered_proof(&session, 0), b"damage").unwrap();

    let mut reopened = Session::open(&fixture.root).unwrap();
    let listed = registered_numbers(&reopened);
    let new = reopened.select(newer, 0);
    let old = session.select(SECOND_FIRST, 0);

    assert_eq!(listed, [1]);
    assert_eq!(new.unwrap()["selected"]["publication"], 2);
    assert!(old.is_err());
}

#[test]
fn a_reopen_keeps_what_an_earlier_read_verified() {
    // Goal: a reopen need not verify a registered window's files again, as with any other window.
    // Method: read a window, put a stray file beside its tables, reopen, and read it again, which
    // a first read of the window would refuse.
    let fixture = registered_far_apart();
    let mut session = Session::open(&fixture.root).unwrap();
    session.select(SECOND_FAR + 60, 0).unwrap();
    let verified = session.verified_windows();
    let stray = registered_proof(&session, 2).with_file_name("stray.parquet");
    std::fs::write(stray, []).unwrap();
    let cancellation = Arc::new(crate::Cancellation::default());

    let mut reopened = Session::reopen(&fixture.root, cancellation, 0, &verified).unwrap();
    let again = reopened.select(SECOND_FAR + 60, 0);
    let fresh = Session::open(&fixture.root)
        .unwrap()
        .select(SECOND_FAR + 60, 0);

    assert_eq!(again.unwrap()["selected"]["publication"], 3);
    assert!(fresh.is_err());
}

#[test]
fn a_session_sees_when_a_new_minute_lands_in_its_span() {
    // Goal: a session can tell its list has gone stale without listing the recording (P02.2).
    // Method: open a session, save one more minute in its span, and ask again.
    let mut fixture = Fixture::registered();
    fixture.publish_with_system_rows(SECOND_FIRST);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let before = session.lists_every_sealed_window().unwrap();
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(SECOND_FIRST + 60);
    fixture.seal();

    let after = session.lists_every_sealed_window().unwrap();

    assert!(before);
    assert!(!after);
}

#[test]
fn a_live_session_lists_a_minute_once_it_is_saved() {
    // Goal: a live session follows what the persister saves through the catalog (P02.1).
    // Method: open a live session over a registered recording with nothing saved, save a minute,
    // and poll until the session lists it.
    let mut fixture = Fixture::registered();
    fixture.publish_with_system_rows(SECOND_FIRST);
    fixture.skip_run_ticks(59);
    let mut session = crate::LiveSession::open(&fixture.root).unwrap();
    session.poll().unwrap();
    let before = session.catalog();
    fixture.publish_with_system_rows(SECOND_FIRST + 60);
    fixture.seal();

    session.poll().unwrap();

    assert_eq!(before["windows"], 0);
    assert_eq!(session.catalog()["windows"], 2);
    assert_eq!(session.catalog()["history_error"], serde_json::Value::Null);
}

/// Saves a reading at `second` through the worker as the first of the next minute.
fn registered_save_minute(fixture: &mut Fixture, second: u64) {
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(second);
    fixture.seal();
}

/// Reads the recording's two ends through a session of its own, and how many windows it listed.
fn registered_read_both_ends(fixture: &Fixture, oldest: u64, newest: u64) -> usize {
    let mut session = Session::open(&fixture.root).unwrap();
    let catalog = session.catalog();

    assert_eq!(catalog["first_second"], oldest);
    assert_eq!(catalog["last_second"], newest);
    assert_eq!(session.select(newest, 0).unwrap()["second"], newest);
    assert_eq!(session.select(oldest, 0).unwrap()["second"], oldest);
    assert!(session.select(oldest, 0).unwrap()["selected"].is_object());

    session.windows.len()
}

/// The windows the long recording's test reads at: either side of the old cap of a session's list
/// and twice past it. A link takes about a millisecond, so the checks take a shorter recording,
/// whose tree has the same shape one level up.
#[cfg(feature = "long-recording")]
const WINDOW_COUNTS: [u64; 4] = [65_535, 65_536, 65_537, 131_072];
#[cfg(not(feature = "long-recording"))]
const WINDOW_COUNTS: [u64; 4] = [1_023, 1_024, 1_025, 2_048];

/// The windows of the recordings a lookup's cost is measured over, a minute each for 1, 7 and 30
/// days, of which the checks take the first.
#[cfg(feature = "long-recording")]
const WINDOW_COUNTS_OF_DAYS: [u64; 3] = [1_440, 10_080, 43_200];
#[cfg(not(feature = "long-recording"))]
const WINDOW_COUNTS_OF_DAYS: [u64; 1] = [1_440];

#[test]
fn a_recording_past_the_old_cap_of_windows_reads_at_both_ends() {
    // Goal: a recording keeps opening and answering at its oldest and newest seconds at 65,535,
    // 65,536, 65,537 and 131,072 windows, a live reader goes on across the old cap, neither lists
    // or reads what lies between, and two runs on one second both stay selectable (P02.1, P02.2).
    // Method: save real minutes at both ends through the worker and link windows between them whose
    // proofs do not read, so that admitting any of them fails the read. Then cross the cap with
    // real saves while a live session polls, and save another run over the oldest second.
    const SECOND_BETWEEN: u64 = SECOND_FIRST + 20_000;
    let [below, at, above, twice] = WINDOW_COUNTS;
    let newest = SECOND_BETWEEN + twice + 20_000;
    let mut fixture = Fixture::registered();
    fixture.publish_with_system_rows(SECOND_FIRST);
    fixture.seal();
    registered_link_between(&fixture, SECOND_BETWEEN, 1..below - 1);
    registered_save_minute(&mut fixture, newest);

    let listed_below = registered_read_both_ends(&fixture, SECOND_FIRST, newest);
    let mut live = crate::LiveSession::open(&fixture.root).unwrap();
    live.poll().unwrap();
    assert_eq!(at, below + 1);
    registered_save_minute(&mut fixture, newest + 60);
    live.poll().unwrap();
    let live_at = live.catalog()["last_second"].clone();
    let listed_at = registered_read_both_ends(&fixture, SECOND_FIRST, newest + 60);
    assert_eq!(above, at + 1);
    registered_save_minute(&mut fixture, newest + 120);
    live.poll().unwrap();
    let live_above = live.catalog()["last_second"].clone();
    let listed_above = registered_read_both_ends(&fixture, SECOND_FIRST, newest + 120);
    registered_link_between(&fixture, SECOND_BETWEEN, below - 1..twice - 4);
    registered_save_minute(&mut fixture, newest + 180);
    live.poll().unwrap();
    let listed_twice = registered_read_both_ends(&fixture, SECOND_FIRST, newest + 180);
    let mut sharing = Fixture::new_mode(true, 2, Some(&fixture));
    sharing.start_at_run_tick(58);
    sharing.publish_with_system_rows(SECOND_FIRST);
    sharing.seal();
    live.poll().unwrap();
    let mut session = Session::open(&fixture.root).unwrap();
    let one = session.select(SECOND_FIRST, 0).unwrap();
    let other = session.select(SECOND_FIRST, 1).unwrap();

    assert_eq!(one["alternatives"].as_array().unwrap().len(), 2);
    assert_ne!(one["selected"]["run"], other["selected"]["run"]);
    assert_eq!(live_at, newest + 60);
    assert_eq!(live_above, newest + 120);
    assert_eq!(live.catalog()["last_second"], newest + 180);
    assert_eq!(live.catalog()["history_error"], serde_json::Value::Null);
    assert!(listed_below <= 4);
    assert!(listed_at <= 4);
    assert!(listed_above <= 4);
    assert!(listed_twice <= 4);
}

#[test]
fn what_a_lookup_reads_stays_within_the_tree_however_long_the_recording() {
    // Goal: a lookup's work is bounded by the tree's shape and not by what the recording saved, at
    // the sizes of 1, 7 and 30 days, and saving one more window costs the same (P02.2).
    // Method: link recordings of those sizes and count what the catalog's lookups read: the two
    // ends, a window by its publication, a span of the clock and the run's frontier a save reads.
    for windows in WINDOW_COUNTS_OF_DAYS {
        let mut fixture = Fixture::registered();
        fixture.publish_with_system_rows(SECOND_FIRST);
        fixture.seal();
        registered_link_between(&fixture, SECOND_FIRST + 20_000, 1..windows);
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let catalog = Catalog::open(&fixture.root, &budget).unwrap();
        let run = fixture.writer.identity().unwrap();

        let mut ends = ReadWork::default();
        catalog.newest_known(&mut ends).unwrap();
        catalog.oldest_known(&mut ends).unwrap();
        let mut exact = ReadWork::default();
        catalog.window_holding(&run, 1, &mut exact).unwrap();
        let mut span = ReadWork::default();
        catalog
            .windows_in(0, (SECOND_FIRST + 1) * super::NS, &mut span)
            .unwrap();
        let store = persistence::Store::open(&fixture.root).unwrap();
        let mut frontier = ReadWork::default();
        store.source_frontier(&run, &budget, &mut frontier).unwrap();

        println!("{windows} windows: {ends:?} {exact:?} {span:?} {frontier:?}");
        assert!(ends.entries <= 2 * ENTRIES_OF_A_BOUNDARY_MAX);
        assert!(exact.entries <= ENTRIES_OF_A_BOUNDARY_MAX);
        assert!(span.entries <= 2 * ENTRIES_OF_A_BOUNDARY_MAX);
        assert!(frontier.entries <= 2 * ENTRIES_OF_A_BOUNDARY_MAX);
    }
}

/// Links a window of one reading for each publication of `publications`, a second apart from
/// `second`, under a run of their own.
fn registered_link_between(fixture: &Fixture, second: u64, publications: std::ops::Range<u64>) {
    let mut run = fixture.writer.identity().unwrap();
    run.run.0[1] ^= 1;
    let readings: Vec<_> = publications
        .map(
            |publication| persistence::canonical::catalog::KnownReading {
                wall_ns: (second + publication) * super::NS,
                run,
                publication,
            },
        )
        .collect();
    persistence::canonical::catalog::fixture::link_unread_windows(&fixture.root, &readings)
        .unwrap();
}
