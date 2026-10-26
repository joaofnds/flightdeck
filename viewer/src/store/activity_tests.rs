use std::thread;
use std::time::{Duration, Instant};

use super::Store;
use super::tests::view_where;
use crate::live::identity_fixture::Fixture;

#[test]
fn a_paused_store_catches_up_from_sealed_readings_missing_from_the_ring() {
    // Goal: hiding stops ingestion without losing readings overwritten before the next show.
    // Method: park a real store, seal readings, overwrite its small ring, then resume it.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let store = Store::start(&fixture.root).unwrap();
    let retained = view_where(&store, |view| view.seconds.len() == 1);
    let original = *retained.seconds[0];
    store.set_active(false);
    wait_until_paused(&store);

    fixture.publish_with_system_rows(101);
    fixture.publish_with_system_rows(102);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(57);
    for second in 160..170 {
        fixture.publish_with_system_rows(second);
    }
    assert!(store.take().is_none());
    store.set_active(true);
    let caught_up = view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 169_000_000_000)
    });

    let mut expected = vec![100, 101, 102];
    expected.extend(162..170);
    let actual: Vec<_> = caught_up
        .seconds
        .iter()
        .map(|second| second.wall_ns / 1_000_000_000)
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(caught_up.failure, None);
    assert_eq!(retained.seconds.len(), 1);
    assert_eq!(*retained.seconds[0], original);
    assert_eq!(retained.seconds[0].publication, 1);
}

#[test]
fn resumed_payload_ingestion_matches_a_fresh_store_without_changing_a_retained_view() {
    // Goal: catch-up preserves complete processes and exact observations in the payload mode.
    // Method: retain an old View, seal an overwritten prefix, and compare with fresh ingestion.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let store = Store::start_with_payload(&fixture.root, 64 * 1024 * 1024).unwrap();
    let retained = view_where(&store, |view| view.seconds.len() == 1);
    store.set_active(false);
    wait_until_paused(&store);
    fixture.publish_with_system_rows(101);
    fixture.publish_with_system_rows(102);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(57);
    for second in 160..170 {
        fixture.publish_with_system_rows(second);
    }
    let fresh = Store::start_with_payload(&fixture.root, 64 * 1024 * 1024).unwrap();
    let expected = view_where(&fresh, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 169_000_000_000)
    });

    assert_eq!(expected.left_out, None);
    assert_eq!(expected.seconds.len(), 11);

    store.set_active(true);
    let actual = view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 169_000_000_000)
    });

    assert_eq!(actual.seconds, expected.seconds);
    assert_eq!(actual.apps, expected.apps);
    assert_eq!(actual.left_out, expected.left_out);
    assert_eq!(actual.failure, None);
    assert_eq!(retained.apps.len(), 1);
    assert_eq!(retained.apps[0], expected.apps[0]);
    assert!(retained.apps[0].processes.is_ok());
}

#[test]
fn resuming_does_not_reingest_sealed_observations_from_a_previous_run() {
    // Goal: catch-up never appends old runs again after a collector restart.
    // Method: hold readings across two runs, seal both, pause and resume without new data.
    let mut first = Fixture::new();
    first.publish_with_system_rows(100);
    first.seal();
    let store = Store::start(&first.root).unwrap();
    drop(view_where(&store, |view| view.seconds.len() == 1));
    let mut second = Fixture::new_mode(false, 2, Some(&first));
    second.publish_with_system_rows(101);
    second.seal();
    let retained = view_where(&store, |view| view.seconds.len() == 2);
    store.set_active(false);
    wait_until_paused(&store);

    second.publish_with_system_rows(102);
    store.set_active(true);
    let actual = view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 102_000_000_000)
    });

    let actual: Vec<_> = actual.seconds.iter().map(|second| second.wall_ns).collect();
    assert_eq!(actual, [100_000_000_000, 101_000_000_000, 102_000_000_000]);
    assert_eq!(retained.seconds.len(), 2);
}

#[test]
fn dropping_a_paused_store_wakes_and_reaps_its_thread() {
    // Goal: quitting while hidden never leaves the store waiting for a show that cannot arrive.
    // Method: park a real store and require its destructor to complete through a bounded channel.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |view| view.seconds.len() == 1));
    store.set_active(false);
    wait_until_paused(&store);
    let (sent, received) = std::sync::mpsc::sync_channel(1);

    let destructor = thread::spawn(move || {
        drop(store);
        sent.send(()).unwrap();
    });

    received.recv_timeout(Duration::from_secs(2)).unwrap();
    destructor.join().unwrap();
}

fn wait_until_paused(store: &Store) {
    let deadline = Instant::now() + Duration::from_secs(2);
    for _ in 0..2_000 {
        if store.is_paused() {
            return;
        }
        assert!(Instant::now() < deadline, "store did not pause");
        thread::sleep(Duration::from_millis(1));
    }
    panic!("store did not pause");
}

#[test]
fn resuming_reuses_the_live_reader_instead_of_reprocessing_consumed_ticks() {
    // Goal: catch-up processes only ring publications received since the reader rested.
    // Method: use the real ring before and after the actual resume path and inspect its output.
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let mut fixture = Fixture::new();
    for second in 100..103 {
        fixture.publish_with_system_rows(second);
    }
    let mut held = super::Held::new();
    let mut ring = super::Ring::new(fixture.root.clone());
    ring.follow().unwrap();
    ring.read(|run, second, apps| held.take(run, second, apps))
        .unwrap();
    for second in 103..106 {
        fixture.publish_with_system_rows(second);
    }
    let shared = super::Shared {
        mailbox: super::Mailbox::new(),
        stop: AtomicBool::new(false),
        active: AtomicBool::new(true),
        catch_up: AtomicBool::new(false),
        catching_up: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        cancellation: Arc::new(crate::cancellation::Cancellation::default()),
    };

    assert!(super::refresh(&fixture.root, &mut held, &mut ring, &shared, None).unwrap());
    ring.follow().unwrap();
    let mut publications = Vec::new();
    let mut consecutive = Vec::new();
    ring.read(|_, second, _| {
        publications.push(second.publication);
        consecutive.push(second.follows);
        true
    })
    .unwrap();

    assert_eq!(publications, [4, 5, 6]);
    assert_eq!(consecutive, [true, true, true]);
}

#[test]
fn pending_handoff_clears_only_after_catch_up_and_view_delivery() {
    // Goal: a shown window can poll until both ingestion and its View handoff complete.
    // Method: park a real store, publish another tick, resume and drain its mailbox.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |view| view.seconds.len() == 1));
    store.set_active(false);
    wait_until_paused(&store);
    assert!(store.pending());
    fixture.publish_with_system_rows(101);

    store.set_active(true);
    let view = view_where(&store, |view| view.seconds.len() == 2);
    let deadline = Instant::now() + Duration::from_secs(2);
    for _ in 0..2_000 {
        if !store.pending() {
            break;
        }
        drop(store.take());
        assert!(
            Instant::now() < deadline,
            "store never completed its handoff"
        );
        thread::sleep(Duration::from_millis(1));
    }

    assert!(!store.pending());
    assert_eq!(view.seconds.last().unwrap().publication, 2);
}
