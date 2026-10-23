use std::io::ErrorKind;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{Failure, Store, View};
use crate::live::identity_fixture::Fixture;

/// The first View the store hands over that `holds` accepts, failing after ten seconds.
fn view_where(store: &Store, holds: impl Fn(&View) -> bool) -> Arc<View> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(view) = store.take() {
            if holds(&view) {
                return view;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the store handed over no such View"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// The whole wall-clock second of each second `view` holds.
fn wall_seconds(view: &View) -> Vec<u64> {
    view.seconds
        .iter()
        .map(|second| second.wall_ns / 1_000_000_000)
        .collect()
}

#[test]
fn a_store_holds_each_ticks_second_oldest_first() {
    // Goal: the store takes in every tick the ring holds when it starts. Method: publish three
    // ticks, start a store over the recording, and read its View that holds three seconds.
    let mut fixture = Fixture::new();
    for second in 100..103 {
        fixture.publish_with_system_rows(second);
    }

    let store = Store::start(&fixture.root).unwrap();

    let view = view_where(&store, |view| view.seconds.len() == 3);
    assert_eq!(wall_seconds(&view), [100, 101, 102]);
}

#[test]
fn a_tick_published_later_reaches_a_view() {
    // Goal: the store follows the ring as the collector publishes. Method: start a store over
    // one tick, wait for its View, publish a second tick, and wait for a View of both.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let store = Store::start(&fixture.root).unwrap();
    view_where(&store, |view| view.seconds.len() == 1);

    fixture.publish_with_system_rows(101);

    let view = view_where(&store, |view| view.seconds.len() == 2);
    assert_eq!(wall_seconds(&view), [100, 101]);
}

#[test]
fn a_second_carries_the_rates_since_the_tick_before_it() {
    // Goal: the store derives each second's rates from the run's previous tick. Method:
    // publish two consecutive ticks whose CPU was busy half the time between them.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);

    let store = Store::start(&fixture.root).unwrap();

    let view = view_where(&store, |view| view.seconds.len() == 2);
    assert_eq!(view.seconds[1].cpu_percent, Some(50.0));
}

#[test]
fn each_view_has_a_higher_version_than_the_one_before() {
    // Goal: the main thread tells a new View from one it drew by its version, since the store
    // writes each View's buffer again. Method: take the View of one tick, publish another, and
    // compare the versions.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let store = Store::start(&fixture.root).unwrap();
    let first = view_where(&store, |view| view.seconds.len() == 1);

    fixture.publish_with_system_rows(101);

    let second = view_where(&store, |view| view.seconds.len() == 2);
    assert!(second.version > first.version);
}

mod when_the_collector_starts_again {
    use super::{Fixture, Store, view_where, wall_seconds};

    #[test]
    fn the_store_follows_the_new_run() {
        // Goal: a collector that starts again is followed into its new run, after the seconds
        // of the run before. Method: publish a tick of one run, register a second run after
        // it, publish a tick there, and read the View that holds both.
        let mut first = Fixture::new();
        first.publish_with_system_rows(100);
        let store = Store::start(&first.root).unwrap();
        view_where(&store, |view| view.seconds.len() == 1);
        let mut second = Fixture::new_mode(false, 2, Some(&first));

        second.publish_with_system_rows(101);

        let view = view_where(&store, |view| view.seconds.len() == 2);
        assert_eq!(wall_seconds(&view), [100, 101]);
    }
}

mod when_the_folder_holds_no_recording {
    use super::{ErrorKind, Failure, Store, view_where};

    #[test]
    fn the_view_reports_the_missing_recording() {
        // Goal: a window over a folder the collector never recorded into says why it shows no
        // readings. Method: start a store over a folder that does not exist.
        let root =
            std::env::temp_dir().join(format!("flightdeck-store-missing-{}", std::process::id()));

        let store = Store::start(&root).unwrap();

        let view = view_where(&store, |view| view.failure.is_some());
        assert_eq!(view.failure, Some(Failure::Recording(ErrorKind::NotFound)));
    }
}
