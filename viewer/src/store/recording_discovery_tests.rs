use std::io::ErrorKind;
use std::sync::Arc;

use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::store::{ByteBudget, Failure};

#[test]
fn a_capacity_stop_reports_incomplete_history_before_decoding() {
    // Goal: a bounded discovery cannot present a truncated source window as complete.
    // Method: seal 129 short windows inside the requested hour and invoke both loaders.
    let fixture = short_windows(129);
    let mut accepted = 0;

    let left_out = super::load_window(
        &fixture.root,
        None,
        crate::session::LOADED_SECONDS,
        &mut super::LoadBuffers::default(),
        |_, _, _| accepted += 1,
    );

    assert_eq!(left_out, Some(Failure::Commits(ErrorKind::InvalidData)));
    assert_eq!(accepted, 0);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

    let outcome =
        super::load_payload(&fixture.root, &budget, &operation, |_, _, _| accepted += 1).unwrap();

    assert_eq!(outcome.left_out, left_out);
    assert!(outcome.terminal.is_none());
    assert_eq!(accepted, 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn exactly_full_discovery_succeeds_when_the_source_is_exhausted() {
    // Goal: reaching the capacity is valid when discovery also proves source exhaustion.
    // Method: enumerate exactly 128 real sealed windows with no older source window.
    let fixture = short_windows(128);
    let mut found = Vec::new();

    let result = super::find(&fixture.root, &mut found, None);

    assert_eq!(result, Ok(()));
    assert_eq!(found.len(), 128);
}

#[test]
fn exactly_full_discovery_succeeds_when_the_next_window_is_outside_the_period() {
    // Goal: an older window can prove the requested boundary after the last accepted slot.
    // Method: request 67 seconds plus the 60-second lead-in from 129 short windows.
    let fixture = short_windows(129);
    let mut found = Vec::new();

    let result = super::find_window(
        &fixture.root,
        &mut found,
        &super::LoadWindow {
            since: None,
            seconds: 67,
            unread: false,
        },
    );

    assert_eq!(result, Ok(()));
    assert_eq!(found.len(), 128);
    assert_eq!(found.last().unwrap().publication_end, 3);
}

fn short_windows(count: u64) -> Fixture {
    let mut fixture = Fixture::new();
    for minute in 0..count {
        fixture.publish_with_events(10_000 + minute, &[]);
        fixture.skip_run_ticks(59);
        fixture.seal_minute(minute);
    }

    fixture
}
