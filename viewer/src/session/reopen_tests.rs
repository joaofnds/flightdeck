use std::sync::Arc;

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
fn reopening_rereads_only_the_marker_of_an_accepted_window() {
    // Goal: an accepted window costs a marker read, not a verification of all its files.
    // Method: remove a data file a fresh open verifies, then reopen with the window accepted.
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal();
    let first = Session::open(&fixture.root).unwrap();
    remove_a_data_file(&fixture.root.join("data"));

    let reopened = Session::reopen(
        &fixture.root,
        Arc::new(Cancellation::default()),
        0,
        &first.windows,
    );

    assert!(Session::open(&fixture.root).is_err());
    assert_eq!(reopened.unwrap().windows, first.windows);
}

fn remove_a_data_file(directory: &std::path::Path) {
    let mut pending = vec![directory.to_owned()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                std::fs::remove_file(entry.path()).unwrap();
                return;
            }
        }
    }
    panic!("the sealed window has a data file");
}
