use std::io::ErrorKind;
use std::sync::Arc;

use persistence::Error;

use super::Session;
use crate::Cancellation;
use crate::live::identity_fixture::Fixture;

#[test]
fn reopening_adds_a_newly_sealed_minute_as_a_fresh_open_would() {
    // Goal: a reopen lists every committed window, the accepted ones and the new one.
    // Method: open after the first minute seals, seal the second, and compare with a fresh open.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish(1001);
    fixture.publish(1002);
    fixture.seal_minute(0);
    let first = Session::open(&fixture.root).unwrap();
    fixture.seal_minute(1);

    let reopened = Session::reopen(
        &fixture.root,
        Arc::new(Cancellation::default()),
        0,
        &first.windows,
    )
    .unwrap();

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
    add_a_stray_file(&fixture.root.join("data"));

    let mut reopened = Session::reopen(
        &fixture.root,
        Arc::new(Cancellation::default()),
        0,
        &first.verified(),
    )
    .unwrap();

    assert!(reopened.select(1001, 0).is_ok());
    assert!(
        Session::open(&fixture.root)
            .unwrap()
            .select(1001, 0)
            .is_err()
    );
}

#[test]
fn opening_leaves_a_window_s_files_to_its_first_read() {
    // Goal: an open costs a marker read for each sealed window, and a window's files are
    // verified before its first read uses them.
    // Method: put a stray file beside a sealed window's tables, then open and read the window.
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal();
    add_a_stray_file(&fixture.root.join("data"));

    let mut session = Session::open(&fixture.root).unwrap();
    let read = session.select(1001, 0);

    assert!(
        matches!(&read, Err(Error::Io(error)) if error.kind() == ErrorKind::InvalidData),
        "{read:?}"
    );
}

#[test]
fn opening_refuses_a_window_sealed_for_another_recording() {
    // Goal: reading only markers at open still refuses a window from another recording.
    // Method: seal a window, give the recording a new identity, then open it.
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal();
    let other = collector::recording::random_identity().unwrap();
    let text = collector::recording::identity_text(other);
    std::fs::write(fixture.root.join("identity"), format!("{text}\n")).unwrap();

    let opened = Session::open(&fixture.root);

    assert!(
        matches!(&opened, Err(Error::Io(error)) if error.kind() == ErrorKind::InvalidData),
        "{:?}",
        opened.map(|session| session.windows)
    );
}

fn add_a_stray_file(directory: &std::path::Path) {
    let mut pending = vec![directory.to_owned()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                std::fs::write(path.join("stray.parquet"), b"stray").unwrap();
                return;
            }
        }
    }
    panic!("the sealed window has a data file");
}
