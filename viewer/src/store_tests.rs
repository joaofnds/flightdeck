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
fn a_second_ranks_its_apps_as_the_worker_ranks_them() {
    // Goal: the page draws a second's top Apps from the View, ranked by the App rule the
    // worker runs. Method: publish three ticks, then rank the newest through the worker's
    // session and through a store's View.
    let mut fixture = Fixture::new();
    for second in 100..103 {
        fixture.publish_with_system_rows(second);
    }
    let mut session = crate::LiveSession::open(&fixture.root).unwrap();
    session.select(102, 0).unwrap();
    let expected = session.selected_ranks().unwrap();

    let store = Store::start(&fixture.root).unwrap();

    let view = view_where(&store, |view| view.seconds.len() == 3);
    assert_eq!(view.apps[2].ranks(), Ok(expected));
}

#[test]
fn seconds_in_a_row_hold_one_name_for_each_app() {
    // Goal: an App's name is held once for every second that names it, which keeps each
    // second's Apps small. Method: publish two ticks and compare the names their seconds hold.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);

    let store = Store::start(&fixture.root).unwrap();

    let view = view_where(&store, |view| view.seconds.len() == 2);
    let (first, second) = (&view.apps[0].apps, &view.apps[1].apps);
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    assert!(Arc::ptr_eq(&first[0].name, &second[0].name));
    assert!(Arc::ptr_eq(&first[1].name, &second[1].name));
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
    use super::super::ring::{FOLLOW_STEPS, Ring};
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

    #[test]
    fn the_store_looks_for_the_new_run_once_in_ten_steps() {
        // Goal: a step reads the runs registry only once in ten, so the steps between the ring's
        // ticks read no file, and a new run is still followed within a second of steps. Method:
        // follow a run through the ring adapter, register a second run with a tick, and count
        // the steps until the adapter hands that tick over.
        let mut first = Fixture::new();
        first.publish_with_system_rows(100);
        let mut ring = Ring::new(first.root.clone());
        ring.follow().unwrap();
        ring.read(|_, _, _| true).unwrap();
        let mut second = Fixture::new_mode(false, 2, Some(&first));
        second.publish_with_system_rows(101);

        let steps = (1..=FOLLOW_STEPS + 1).find(|_| {
            ring.follow().unwrap();
            let mut taken = false;
            ring.read(|_, _, _| {
                taken = true;
                true
            })
            .unwrap();
            taken
        });

        assert_eq!(steps, Some(FOLLOW_STEPS));
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
    use super::{RUN, UNREAD, published, second_at, wall_seconds_of};

    #[test]
    fn the_store_keeps_the_first_second_of_each_ten_second_stretch() {
        // Goal: a chart of the loaded hour draws one reading in every ten seconds past the live
        // span, as the older tail draws them. Method: take in 25 seconds more than the live span
        // holds, from a wall second that starts no stretch, and read the older seconds.
        let mut held = Held::new();
        let first = 1_005;
        let last = first + u64::try_from(SECONDS_MAX).unwrap() + 25;

        for wall_second in first..last {
            assert!(held.take(&RUN, &second_at(wall_second), UNREAD));
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
            assert!(held.take(&RUN, &second_at(wall_second), UNREAD));
        }

        let view = published(&mut held);
        assert_eq!(view.earlier.len(), EARLIER_MAX);
        assert_eq!(wall_seconds_of(&view.earlier[..1]), [1_010]);
    }
}

mod when_a_second_reaches_the_store_twice {
    use super::super::Held;
    use super::{RUN, UNREAD, published, second_at, wall_seconds_of};
    use collector::ring::{Identity, RunIdentity};

    #[test]
    fn the_store_holds_it_once() {
        // Goal: a tick the recording sealed and the ring still holds shows once. Method: take in
        // one run's second, then that second and the one before it again.
        let mut held = Held::new();
        assert!(held.take(&RUN, &second_at(101), UNREAD));

        assert!(held.take(&RUN, &second_at(100), UNREAD));
        assert!(held.take(&RUN, &second_at(101), UNREAD));

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
        assert!(held.take(&RUN, &second_at(101), UNREAD));

        assert!(held.take(&next, &second_at(5), UNREAD));

        let view = published(&mut held);
        assert_eq!(wall_seconds_of(&view.seconds), [101, 5]);
    }
}

mod when_the_readings_break {
    use super::super::{Held, SECONDS_MAX};
    use super::{RUN, UNREAD, published, second_at};
    use collector::ring::{Identity, RunIdentity};

    #[test]
    fn a_second_that_does_not_follow_the_one_before_starts_a_segment() {
        // Goal: a chart breaks its line where the readings break. Method: take in two pairs of
        // seconds, each second of a pair following the one before it, and read their segments.
        let mut held = Held::new();

        for (wall_second, follows) in [(100, false), (101, true), (102, false), (103, true)] {
            assert!(held.take(&RUN, &following(wall_second, follows), UNREAD));
        }

        assert_eq!(segments_of(&published(&mut held).seconds), [0, 0, 1, 1]);
    }

    #[test]
    fn a_second_of_another_run_starts_a_segment() {
        // Goal: a line never joins the readings of two runs. Method: take in a second of the
        // next run that follows a second of its own run, after a second of this run.
        let mut held = Held::new();
        let next = RunIdentity {
            run: Identity([4, 4]),
            ..RUN
        };

        assert!(held.take(&RUN, &following(100, false), UNREAD));
        assert!(held.take(&next, &following(5, true), UNREAD));

        assert_eq!(segments_of(&published(&mut held).seconds), [0, 1]);
    }

    #[test]
    fn an_older_second_keeps_the_segment_its_readings_had() {
        // Goal: the older seconds, one in ten, break their line where the readings between them
        // broke. Method: break the readings at a second the older seconds leave out, let the
        // live span go of three stretches, and read the older seconds' segments.
        let mut held = Held::new();
        let last = 1_030 + u64::try_from(SECONDS_MAX).unwrap();

        for wall_second in 1_000..last {
            assert!(held.take(&RUN, &following(wall_second, wall_second != 1_015), UNREAD));
        }

        assert_eq!(segments_of(&published(&mut held).earlier), [0, 0, 1]);
    }

    fn following(wall_second: u64, follows: bool) -> super::super::MachineSecond {
        super::super::MachineSecond {
            follows,
            ..second_at(wall_second)
        }
    }

    fn segments_of(seconds: &[std::sync::Arc<super::super::MachineSecond>]) -> Vec<u64> {
        seconds.iter().map(|second| second.segment).collect()
    }
}

/// What a second the tests take in directly holds of its Apps.
const UNREAD: super::apps::TickApps<'static> =
    super::apps::TickApps::Missing(&super::Missing::Unread);

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

mod when_the_recording_holds_sealed_minutes {
    use std::sync::Arc;

    use super::{Failure, Fixture, Store, View, held_through, view_where, wall_seconds_of};
    use crate::store::{MachineSecond, Missing};

    #[test]
    fn the_store_holds_the_sealed_seconds_the_ring_no_longer_holds() {
        // Goal: a window that opens draws the hour before the ring's newest ticks. Method: seal
        // a minute of three ticks, publish the next minute's ticks until the ring holds none of
        // the sealed ones, and start a store.
        let mut fixture = Fixture::new();
        for second in 100..103 {
            fixture.publish_with_system_rows(second);
        }
        fixture.seal_minute(0);
        fixture.skip_run_ticks(57);
        for second in 160..168 {
            fixture.publish_with_system_rows(second);
        }

        let store = Store::start(&fixture.root).unwrap();

        let view = view_where(&store, |view| held_through(view, 167));
        let mut expected = vec![100, 101, 102];
        expected.extend(160..168);
        assert_eq!(wall_seconds_of(&view.seconds), expected);
    }

    #[test]
    fn a_store_built_from_the_recording_holds_what_one_built_from_the_ring_holds() {
        // Goal: the ring and the recording hand the store the same seconds, the rates of a
        // sealed minute's first second included. Method: follow two minutes' ticks with a store
        // over the ring, seal both, publish until the ring holds neither, start a second store,
        // and compare the seconds the two hold.
        let mut fixture = Fixture::new();
        fixture.skip_run_ticks(57);
        let from_ring = Store::start(&fixture.root).unwrap();
        for second in 157..162 {
            fixture.publish_with_system_rows(second);
            view_where(&from_ring, |view| held_through(view, second));
        }
        fixture.seal_minute(0);
        fixture.seal_minute(1);
        fixture.skip_run_ticks(58);
        for second in 240..248 {
            fixture.publish_with_system_rows(second);
        }

        let from_recording = Store::start(&fixture.root).unwrap();

        let expected = view_where(&from_ring, |view| held_through(view, 247));
        let view = view_where(&from_recording, |view| held_through(view, 247));
        assert_eq!(seconds_of(&view), seconds_of(&expected));
        assert_eq!(view.seconds[3].cpu_percent, Some(50.0));
    }

    #[test]
    fn a_store_built_from_the_recording_holds_the_apps_one_built_from_the_ring_holds() {
        // Goal: a sealed second holds the Apps its tick gave a store that read it from the ring,
        // so the page draws the same Apps whichever way the store took the second in. Method:
        // follow two minutes' ticks with a store over the ring, seal both with their Apps,
        // publish until the ring holds only the newest sealed tick, as the collector's two-minute
        // ring holds ticks it sealed, start a second store, and compare the Apps the two hold.
        let mut fixture = Fixture::new();
        fixture.skip_run_ticks(57);
        let from_ring = Store::start(&fixture.root).unwrap();
        for second in 157..162 {
            fixture.publish_with_system_rows(second);
            view_where(&from_ring, |view| held_through(view, second));
        }
        let derive = crate::SealedApps::default();
        fixture.seal_minute_derived(0, &derive);
        fixture.seal_minute_derived(1, &derive);
        fixture.skip_run_ticks(58);
        for second in 240..247 {
            fixture.publish_with_system_rows(second);
        }

        let from_recording = Store::start(&fixture.root).unwrap();

        let expected = view_where(&from_ring, |view| held_through(view, 246));
        let view = view_where(&from_recording, |view| held_through(view, 246));
        assert_eq!(view.apps, expected.apps);
        assert_eq!(view.apps[3].missing, None);
        assert!(Arc::ptr_eq(
            &view.apps[2].apps[0].name,
            &view.apps[3].apps[0].name
        ));
    }

    #[test]
    fn a_sealed_second_older_than_the_live_span_holds_no_apps() {
        // Goal: the store reads the Apps of the 18 minutes a live chart draws and none older,
        // which keeps its launch short and its memory small. Method: seal a minute, seal another
        // more than 18 minutes later, both with their Apps, publish until the ring holds neither,
        // and start a store.
        let mut fixture = Fixture::new();
        let derive = crate::SealedApps::default();
        fixture.publish_with_system_rows(100);
        fixture.seal_minute_derived(0, &derive);
        fixture.skip_run_ticks(59);
        fixture.publish_with_system_rows(1_300);
        fixture.seal_minute_derived(1, &derive);
        fixture.skip_run_ticks(59);
        for second in 1_400..1_408 {
            fixture.publish_with_system_rows(second);
        }

        let store = Store::start(&fixture.root).unwrap();

        let view = view_where(&store, |view| held_through(view, 1_407));
        assert_eq!(wall_seconds_of(&view.seconds)[..2], [100, 1_300]);
        assert_eq!(view.apps[0].missing, Some(Missing::Unread));
        assert_eq!(view.apps[1].missing, None);
    }

    #[test]
    fn a_minute_older_than_the_loaded_hour_is_left_out() {
        // Goal: the store reads the hour a chart reaches back and nothing before it. Method: seal
        // a minute, seal another more than the hour and its lead-in later, publish until the
        // ring holds neither, and start a store.
        let mut fixture = Fixture::new();
        fixture.publish_with_system_rows(100);
        fixture.seal_minute(0);
        fixture.skip_run_ticks(59);
        fixture.publish_with_system_rows(3_800);
        fixture.seal_minute(1);
        fixture.skip_run_ticks(59);
        for second in 3_900..3_908 {
            fixture.publish_with_system_rows(second);
        }

        let store = Store::start(&fixture.root).unwrap();

        let view = view_where(&store, |view| held_through(view, 3_907));
        let mut expected = vec![3_800];
        expected.extend(3_900..3_908);
        assert_eq!(wall_seconds_of(&view.seconds), expected);
    }

    #[test]
    fn a_minute_that_does_not_verify_leaves_a_gap_the_view_reports() {
        // Goal: a sealed minute whose files changed is held as missing, never as readings.
        // Method: seal two minutes, empty one file of the first, publish until the ring holds
        // neither, and start a store.
        let mut fixture = Fixture::new();
        fixture.publish_with_system_rows(100);
        fixture.seal_minute(0);
        fixture.skip_run_ticks(59);
        fixture.publish_with_system_rows(160);
        fixture.seal_minute(1);
        fixture.skip_run_ticks(59);
        for second in 220..228 {
            fixture.publish_with_system_rows(second);
        }
        empty_ticks_file(&fixture, 0);

        let store = Store::start(&fixture.root).unwrap();

        let view = view_where(&store, |view| held_through(view, 227));
        let mut expected = vec![160];
        expected.extend(220..228);
        assert_eq!(wall_seconds_of(&view.seconds), expected);
        assert_eq!(view.left_out, Some(Failure::Refused));
    }

    #[test]
    fn each_loader_decodes_about_an_even_share_of_the_hour() {
        // Goal: the minutes whose Apps the store reads, the newest, do not fall on one loader,
        // since the hour is held once the slowest loader is done. Method: split the loaded
        // hour's 44 older minutes, costing one each, and the live span's 18, costing three,
        // and compare each batch's cost with an even share of 17.
        let mut costs = vec![1; 44];
        costs.extend([3; 18]);

        let batches = crate::store::recording::load_batches_split(&costs);

        let minutes: Vec<usize> = batches.iter().flat_map(Clone::clone).collect();
        assert_eq!(minutes, (0..62).collect::<Vec<_>>());
        for batch in batches {
            assert!(costs[batch].iter().sum::<usize>() < 17 + 3);
        }
    }

    fn seconds_of(view: &View) -> Vec<MachineSecond> {
        view.seconds.iter().map(|second| **second).collect()
    }

    /// Empties the ticks table of the fixture's sealed minute `minute`.
    fn empty_ticks_file(fixture: &Fixture, minute: u64) {
        let store = persistence::Store::open(&fixture.root).unwrap();
        let marker = store.marker_path(&persistence::WindowIdentity {
            run: fixture.writer.identity().unwrap(),
            window: minute,
        });
        let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
        let ticks = fixture
            .root
            .join(&manifest.data_directory)
            .join("ticks.parquet");
        std::fs::write(ticks, []).unwrap();
    }
}

/// Whether the newest second `view` holds is at whole wall second `wall_second`.
fn held_through(view: &View, wall_second: u64) -> bool {
    view.seconds
        .last()
        .is_some_and(|second| second.wall_ns / 1_000_000_000 == wall_second)
}
