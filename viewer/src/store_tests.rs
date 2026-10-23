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
    assert_eq!(wall_seconds_of(&view.seconds), [100, 101, 102]);
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
    assert_eq!(wall_seconds_of(&view.seconds), [100, 101]);
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
    use super::{Fixture, Store, view_where, wall_seconds_of};

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
        assert_eq!(wall_seconds_of(&view.seconds), [100, 101]);
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

mod when_the_live_span_lets_go_of_a_second {
    use super::super::{EARLIER_MAX, Held, SECONDS_MAX};
    use super::{RUN, published, second_at, wall_seconds_of};

    #[test]
    fn the_store_keeps_the_first_second_of_each_ten_second_stretch() {
        // Goal: a chart of the loaded hour draws one reading in every ten seconds past the live
        // span, as the older tail draws them. Method: take in 25 seconds more than the live span
        // holds, from a wall second that starts no stretch, and read the older seconds.
        let mut held = Held::new();
        let first = 1_005;
        let last = first + u64::try_from(SECONDS_MAX).unwrap() + 25;

        for wall_second in first..last {
            assert!(held.take(&RUN, &second_at(wall_second)));
        }

        let view = published(&mut held);
        assert_eq!(wall_seconds_of(&view.earlier), [1_005, 1_010, 1_020]);
    }

    #[test]
    fn the_store_lets_go_of_the_oldest_stretch_past_the_loaded_hour() {
        // Goal: the older seconds stay within their bound. Method: let the live span go of one
        // stretch more than the older seconds hold, and read the oldest of them.
        let mut held = Held::new();
        let let_go = (u64::try_from(EARLIER_MAX).unwrap() + 1) * 10;
        let last = 1_000 + u64::try_from(SECONDS_MAX).unwrap() + let_go;

        for wall_second in 1_000..last {
            assert!(held.take(&RUN, &second_at(wall_second)));
        }

        let view = published(&mut held);
        assert_eq!(view.earlier.len(), EARLIER_MAX);
        assert_eq!(wall_seconds_of(&view.earlier[..1]), [1_010]);
    }
}

mod when_a_second_reaches_the_store_twice {
    use super::super::Held;
    use super::{RUN, published, second_at, wall_seconds_of};
    use collector::ring::{Identity, RunIdentity};

    #[test]
    fn the_store_holds_it_once() {
        // Goal: a tick the recording sealed and the ring still holds shows once. Method: take in
        // one run's second, then that second and the one before it again.
        let mut held = Held::new();
        assert!(held.take(&RUN, &second_at(101)));

        assert!(held.take(&RUN, &second_at(100)));
        assert!(held.take(&RUN, &second_at(101)));

        let view = published(&mut held);
        assert_eq!(wall_seconds_of(&view.seconds), [101]);
    }

    #[test]
    fn a_second_of_the_next_run_follows_it() {
        // Goal: a collector that starts again numbers its publications afresh, so the store
        // tells its seconds from the run's before by the run. Method: take in a second of one
        // run, then a second of another run with a lower publication.
        let mut held = Held::new();
        let next = RunIdentity {
            run: Identity([4, 4]),
            ..RUN
        };
        assert!(held.take(&RUN, &second_at(101)));

        assert!(held.take(&next, &second_at(5)));

        let view = published(&mut held);
        assert_eq!(wall_seconds_of(&view.seconds), [101, 5]);
    }
}

/// A run of a recording, as the tests that take seconds in directly name it.
const RUN: collector::ring::RunIdentity = collector::ring::RunIdentity {
    recording: collector::ring::Identity([1, 1]),
    run: collector::ring::Identity([2, 2]),
    boot: collector::ring::Identity([3, 3]),
};

/// The second at whole wall second `wall_second`, which is also its publication.
fn second_at(wall_second: u64) -> super::MachineSecond {
    super::MachineSecond {
        publication: wall_second,
        run_tick: wall_second,
        wall_ns: wall_second * 1_000_000_000,
        ..super::MachineSecond::default()
    }
}

/// The View `held` hands over next.
fn published(held: &mut super::Held) -> Arc<View> {
    let mailbox = super::Mailbox::new();
    held.publish(&mailbox);
    mailbox.take().unwrap()
}

/// The whole wall-clock second of each of `seconds`.
fn wall_seconds_of(seconds: &[Arc<super::MachineSecond>]) -> Vec<u64> {
    seconds
        .iter()
        .map(|second| second.wall_ns / 1_000_000_000)
        .collect()
}
