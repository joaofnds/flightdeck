use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use persistence::{Error, Manifest};

use super::Session;
use crate::Cancellation;
use crate::live::identity_fixture::Fixture;

#[test]
fn reopening_adds_a_newly_sealed_minute_as_a_fresh_open_would() {
    // Goal: a reopen lists every committed window, the earlier ones and the new one.
    // Method: open after the first minute seals, seal the second, and compare with a fresh open.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish(1001);
    fixture.publish(1002);
    fixture.seal_minute(0);
    let first = Session::open(&fixture.root).unwrap();
    fixture.seal_minute(1);

    let reopened = reopen(&fixture.root, &first);

    assert_eq!(first.windows.len(), 1);
    assert_eq!(
        reopened.windows,
        Session::open(&fixture.root).unwrap().windows
    );
    assert_eq!(reopened.windows.len(), 2);
}

#[test]
fn reopening_rereads_only_the_marker_of_a_window_a_read_verified() {
    // Goal: a window a read verified costs a marker read after a reopen, not a verification of
    // all its files.
    // Method: read the window, put a stray file beside its tables, then reopen and read again.
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal();
    let mut first = Session::open(&fixture.root).unwrap();
    first.select(1001, 0).unwrap();
    add_a_stray_file(&fixture.root, &first.windows[0]);
    let mut reopened = reopen(&fixture.root, &first);

    let read = reopened.select(1001, 0);

    assert!(read.is_ok(), "{read:?}");
}

#[test]
fn reopening_verifies_a_window_no_read_verified_on_its_first_read() {
    // Goal: a reopen carries over only the windows a read verified, so a window the earlier
    // session listed but never read is still verified before a read uses it.
    // Method: seal two minutes whose seconds lie further apart than a selection's context
    // reaches, read only the first, damage the second, then reopen and read it.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish(1001);
    fixture.publish(2001);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    let mut first = Session::open(&fixture.root).unwrap();
    first.select(1001, 0).unwrap();
    add_a_stray_file(&fixture.root, &first.windows[1]);
    let mut reopened = reopen(&fixture.root, &first);

    let read = reopened.select(2001, 0);

    assert!(refused(&read), "{read:?}");
}

#[test]
fn opening_leaves_a_window_s_files_to_its_first_read() {
    // Goal: an open costs a marker read for each sealed window, and a window's files are
    // verified before its first read uses them.
    // Method: put a stray file beside a sealed window's tables, then open and read the window.
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal();
    let sealed = Session::open(&fixture.root).unwrap().windows.remove(0);
    add_a_stray_file(&fixture.root, &sealed);

    let mut session = Session::open(&fixture.root).unwrap();
    let read = session.select(1001, 0);

    assert!(refused(&read), "{read:?}");
}

#[test]
fn a_window_that_fails_verification_fails_every_read() {
    // Goal: a window refused on its first read is never served by a marker reread afterwards.
    // Method: put a stray file beside a sealed window's tables, then read the window twice.
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    add_a_stray_file(&fixture.root, &session.windows[0]);
    assert!(refused(&session.select(1001, 0)));

    let read = session.select(1001, 0);

    assert!(refused(&read), "{read:?}");
}

#[test]
fn a_verified_window_stops_being_readable_once_its_marker_is_gone() {
    // Goal: a read of a window a read verified still checks that the window stays committed.
    // Method: read the window, remove its marker, then read it again.
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let identity = &session.windows[0].identity;
    let marker = persistence::layout::WindowKey::from(identity).marker(&fixture.root);
    std::fs::remove_file(marker).unwrap();

    let read = session.select(1001, 0);

    assert!(
        matches!(&read, Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound),
        "{read:?}"
    );
}

#[test]
fn opening_refuses_a_window_sealed_for_another_recording() {
    // Goal: reading only markers at open still refuses a window from another recording.
    // Method: seal a window, give the recording another identity, then open it.
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal();
    let mut other = collector::recording::read_identity(&fixture.root).unwrap();
    other.0[0] ^= 1;
    let text = collector::recording::identity_text(other);
    std::fs::write(fixture.root.join("identity"), format!("{text}\n")).unwrap();

    let opened = Session::open(&fixture.root);

    assert!(
        matches!(&opened, Err(Error::Io(error)) if error.kind() == ErrorKind::InvalidData),
        "{:?}",
        opened.map(|session| session.windows)
    );
}

#[test]
fn opening_lists_every_sealed_minute_in_order() {
    // Goal: an open lists each sealed window once and in publication order, however the
    // markers are split between the threads that read them.
    // Method: seal more minutes than there are reader threads, open, and list their minutes.
    let count = u64::try_from(MINUTES).unwrap();
    let fixture = sealed_minutes(count);

    let session = Session::open(&fixture.root).unwrap();

    let minutes: Vec<u64> = session
        .windows
        .iter()
        .map(|window| window.identity.window)
        .collect();
    assert_eq!(minutes, (0..count).collect::<Vec<u64>>());
}

#[test]
fn opening_refuses_a_broken_marker_after_many_sound_ones() {
    // Goal: a marker that fails its read refuses the open wherever it sits in the listing.
    // Method: seal more minutes than there are reader threads, break the last marker, open.
    let fixture = sealed_minutes(u64::try_from(MINUTES).unwrap());
    let last = Session::open(&fixture.root).unwrap().windows.pop().unwrap();
    let marker = persistence::layout::WindowKey::from(&last.identity).marker(&fixture.root);
    std::fs::write(marker, b"broken").unwrap();

    let opened = Session::open(&fixture.root);

    assert!(
        refused(&opened),
        "{:?}",
        opened.map(|session| session.windows)
    );
}

/// More minutes than `discover` has reader threads, so a thread reads several markers. The
/// fixture's reader takes at most eight.
const MINUTES: usize = 8;
const _: () = assert!(MINUTES > super::DISCOVER_THREADS);

fn sealed_minutes(count: u64) -> Fixture {
    let mut fixture = Fixture::new();
    for minute in 0..count {
        fixture.publish(1001 + minute * 60);
        fixture.seal_minute(minute);
        fixture.skip_run_ticks(59);
    }
    fixture
}

fn reopen(root: &Path, earlier: &Session) -> Session {
    let cancellation = Arc::new(Cancellation::default());
    Session::reopen(root, cancellation, 0, &earlier.verified_windows()).unwrap()
}

/// A read refused because the window's files failed verification.
fn refused<T>(read: &persistence::Result<T>) -> bool {
    matches!(read, Err(Error::Io(error)) if error.kind() == ErrorKind::InvalidData)
}

fn add_a_stray_file(root: &Path, window: &Manifest) {
    let directory = root.join(&window.data_directory);
    std::fs::write(directory.join("stray.parquet"), b"stray").unwrap();
}
