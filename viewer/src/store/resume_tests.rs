use super::tests::view_where;
use super::{MachineSecond, Mailbox, Publication, Store};
use crate::live::identity_fixture::Fixture;

#[test]
fn a_checkpoint_overwritten_after_admission_reloads_before_publishing() {
    // Goal: a writer cannot turn a covered resume into a silently incomplete replacement.
    // Method: overwrite the oldest unread slot after the real coverage check, then reconcile.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    let (mut held, mut ring) = reconciliation_held(&fixture);
    let mut buffers = super::recording::LoadBuffers::default();
    fixture.skip_run_ticks(59);
    for second in 101..109 {
        fixture.publish_with_events(second, &[]);
    }
    fixture.seal_minute(1);
    assert!(ring.covers(held.newest.as_ref().unwrap()).unwrap());
    fixture.publish_with_events(109, &[]);
    let shared = reconciliation_shared();

    reconciliation_step(&mut held, &mut ring, &shared);

    assert!(shared.mailbox.take().is_none());
    assert!(super::run_retry(&mut held, &ring, &shared));
    assert!(
        super::refresh(
            &fixture.root,
            &mut held,
            &mut ring,
            &shared,
            &mut buffers,
            None
        )
        .unwrap()
    );
    for _ in 0..2 {
        reconciliation_step(&mut held, &mut ring, &shared);
        assert!(!super::run_retry(&mut held, &ring, &shared));
    }
    let resumed = shared.mailbox.take().unwrap();
    let seconds: Vec<_> = resumed
        .seconds
        .iter()
        .map(|second| second.wall_ns)
        .collect();
    let expected: Vec<_> = (100..110).map(|second| second * 1_000_000_000).collect();
    assert_eq!(seconds, expected);
    assert_eq!(resumed.failure, None);
    assert_eq!(resumed.left_out, None);
}

#[test]
fn a_ring_overrun_during_a_resume_reads_only_the_minutes_sealed_since_its_newest_second() {
    // Goal: losing the place in the ring keeps the held hour, as a long return does.
    // Method: hold two sealed minutes, take the oldest offline, overrun the ring, reconcile.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(160, &[]);
    fixture.seal_minute(1);
    let (mut held, mut ring) = reconciliation_held(&fixture);
    let mut buffers = super::recording::LoadBuffers::default();
    take_minute_offline(&fixture, 0);
    fixture.skip_run_ticks(59);
    for second in 161..169 {
        fixture.publish_with_events(second, &[]);
    }
    fixture.seal_minute(2);
    assert!(ring.covers(held.newest.as_ref().unwrap()).unwrap());
    fixture.publish_with_events(169, &[]);
    let shared = reconciliation_shared();
    reconciliation_step(&mut held, &mut ring, &shared);
    assert!(super::run_retry(&mut held, &ring, &shared));

    let refreshed = super::refresh(
        &fixture.root,
        &mut held,
        &mut ring,
        &shared,
        &mut buffers,
        None,
    );
    for _ in 0..2 {
        reconciliation_step(&mut held, &mut ring, &shared);
    }

    assert!(refreshed.unwrap());
    let resumed = shared.mailbox.take().unwrap();
    let mut expected = vec![100, 160];
    expected.extend(161..170);
    assert_eq!(seconds_of(&resumed), expected);
    assert_eq!(resumed.failure, None);
    assert_eq!(resumed.left_out, None);
}

/// The store thread's held hour over `fixture` before its first step, following the ring past
/// the newest sealed minute.
fn reconciliation_held(fixture: &Fixture) -> (super::Held, super::Ring) {
    let mut held = super::Held::new();
    let mut ring = super::Ring::new(fixture.root.clone());
    let cancellation = std::sync::Arc::new(crate::cancellation::Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    super::startup(&fixture.root, &mut held, &mut ring, &operation).unwrap();
    ring.follow().unwrap();
    ring.read(|run, second, apps| held.take(run, second, apps))
        .unwrap();
    (held, ring)
}

fn reconciliation_step(held: &mut super::Held, ring: &mut super::Ring, shared: &super::Shared) {
    super::step(
        held,
        ring,
        Ok(()),
        &shared.mailbox,
        None,
        Publication::Complete,
    )
    .unwrap();
}

fn reconciliation_shared() -> super::Shared {
    use std::sync::atomic::AtomicBool;
    super::Shared {
        queries: None,
        mailbox: Mailbox::new(),
        stop: AtomicBool::new(false),
        active: AtomicBool::new(true),
        catch_up: AtomicBool::new(false),
        catching_up: AtomicBool::new(true),
        paused: AtomicBool::new(false),
        cancellation: std::sync::Arc::new(crate::cancellation::Cancellation::default()),
    }
}

#[test]
fn a_covered_ring_resumes_without_reopening_its_already_cached_sealed_data() {
    // Goal: a short hide needs only new ring data, even when the cached sealed files go offline.
    // Method: pause a real warm store, move its data directory, publish one tick, then show it.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal_minute(0);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |_| true));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    std::fs::rename(fixture.root.join("data"), fixture.root.join("offline_data")).unwrap();
    fixture.publish_with_system_rows(101);

    store.set_active(true);
    let resumed = view_where(&store, |_| true);

    assert_eq!(resumed.left_out, None);
    assert_eq!(resumed.failure, None);
    assert_eq!(resumed.seconds.last().unwrap().wall_ns, 101_000_000_000);
    assert_eq!(resumed.seconds.first().unwrap().wall_ns, 100_000_000_000);
}

#[test]
fn opening_pinned_context_does_not_ingest_the_live_ring() {
    // Goal: reopening a pin with a fresh reader cannot start live ingestion.
    // Method: open saved context over a populated ring and inspect its public catalog.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let cancellation = std::sync::Arc::new(crate::cancellation::Cancellation::default());

    let session = crate::LiveSession::open_history_with(&fixture.root, cancellation, 0).unwrap();

    assert!(session.catalog()["last_second"].is_null());
    assert_eq!(session.catalog()["tail_ticks"], 0);
    assert!(session.catalog()["collector_completed"].is_null());
}

#[test]
fn resuming_preserves_the_first_unsealed_readings_machine_and_app_rates() {
    // Goal: a fresh ring restores its predecessor instead of erasing already known rates.
    // Method: seal a predecessor, retain eight successors in the small ring, then hide and show.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(100);
    fixture.seal_minute(0);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |_| true));
    fixture.publish_with_system_rows(101);
    let known = view_where(&store, |view| view.seconds.last().unwrap().publication == 2);
    let expected = (**known.seconds.last().unwrap()).clone();
    let expected_apps = std::sync::Arc::clone(known.apps.last().unwrap());
    drop(known);
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    for second in 102..109 {
        fixture.publish_with_system_rows(second);
    }

    store.set_active(true);
    let resumed = view_where(&store, |_| true);

    let index = resumed
        .seconds
        .iter()
        .position(|second| second.publication == 2)
        .unwrap();
    assert!(expected.follows);
    assert_eq!(*resumed.seconds[index], expected);
    assert_eq!(resumed.apps[index], expected_apps);
}

#[test]
fn a_resume_beyond_ring_retention_reads_only_the_minutes_sealed_since_its_newest_second() {
    // Goal: a return keeps the held hour and reads only what the recording sealed while hidden.
    // Method: hold two sealed minutes, hide, seal nine more whose publications overrun the
    // fixture's eight-slot ring, take the oldest offline, show.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(160, &[]);
    fixture.seal_minute(1);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |_| true));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    for minute in 2..=10 {
        fixture.skip_run_ticks(59);
        fixture.publish_with_events(100 + minute * 60, &[]);
        fixture.seal_minute(minute);
    }
    take_minute_offline(&fixture, 0);

    store.set_active(true);
    let resumed = view_where(&store, |_| true);

    assert_eq!(
        seconds_of(&resumed),
        (0..=10).map(|minute| 100 + minute * 60).collect::<Vec<_>>()
    );
    assert_eq!(resumed.failure, None);
    assert_eq!(resumed.left_out, None);
}

#[test]
fn a_resume_past_the_newest_sealed_minute_reads_only_that_minute_again() {
    // Goal: a return whose newest held second follows the newest sealed minute reads that
    // minute alone, to seed the rates after it, and keeps the rest of the held hour.
    // Method: hold two sealed minutes and a later second, hide, publish nine more seconds
    // without sealing them, which overrun the fixture's eight-slot ring, take the oldest
    // minute offline, show.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(160, &[]);
    fixture.seal_minute(1);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(220, &[]);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 220_000_000_000)
    }));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    for second in 221..230 {
        fixture.publish_with_events(second, &[]);
    }
    take_minute_offline(&fixture, 0);

    store.set_active(true);
    let resumed = view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 229_000_000_000)
    });

    assert_eq!(seconds_of(&resumed)[..3], [100, 160, 220]);
    assert_eq!(resumed.failure, None);
    assert_eq!(resumed.left_out, None);
}

#[test]
fn a_resume_after_more_than_an_hour_holds_what_a_fresh_store_holds() {
    // Goal: a return whose newest second left the loaded hour starts the hour over.
    // Method: hold one sealed minute, hide, seal two minutes an hour later whose nine
    // publications overrun the fixture's eight-slot ring, show, then start a fresh store.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |_| true));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    fixture.skip_run_ticks(3_659);
    for second in 3_760..3_765 {
        fixture.publish_with_events(second, &[]);
    }
    fixture.seal_minute(61);
    fixture.skip_run_ticks(55);
    for second in 3_820..3_824 {
        fixture.publish_with_events(second, &[]);
    }
    fixture.seal_minute(62);

    store.set_active(true);
    let resumed = view_where(&store, |_| true);

    let fresh = Store::start(&fixture.root).unwrap();
    assert_eq!(
        seconds_of(&resumed),
        seconds_of(&view_where(&fresh, |_| true))
    );
    assert_eq!(seconds_of(&resumed)[0], 3_760);
    assert_eq!(resumed.failure, None);
    assert_eq!(resumed.left_out, None);
}

#[test]
fn a_resume_across_a_collector_restart_holds_what_a_fresh_store_holds() {
    // Goal: a return that cannot extend the new run's held minute keeps the older run's hour.
    // Method: hold two minutes of one run and a second of the next, hide, fill and seal that
    // second's minute and four more, nine publications that overrun the fixture's eight-slot
    // ring, show, then start a fresh store.
    let mut first = Fixture::new();
    first.publish_with_events(100, &[]);
    first.seal_minute(0);
    first.skip_run_ticks(59);
    first.publish_with_events(160, &[]);
    first.seal_minute(1);
    let store = Store::start(&first.root).unwrap();
    drop(view_where(&store, |_| true));
    let mut second = Fixture::new_mode(false, 2, Some(&first));
    second.publish_with_events(1000, &[]);
    drop(view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 1_000_000_000_000)
    }));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    for wall in 1001..1006 {
        second.publish_with_events(wall, &[]);
    }
    second.seal_minute(0);
    for minute in 1..=4 {
        second.skip_run_ticks(59);
        second.publish_with_events(1000 + minute * 60, &[]);
        second.seal_minute(minute);
    }

    store.set_active(true);
    let resumed = view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 1_240_000_000_000)
    });

    let fresh = Store::start(&first.root).unwrap();
    assert_eq!(
        seconds_of(&resumed),
        seconds_of(&view_where(&fresh, |_| true))
    );
    assert_eq!(seconds_of(&resumed)[..2], [100, 160]);
}

#[test]
fn a_resume_retries_refused_history_until_it_is_recovered() {
    // Goal: a successful suffix cannot hide or permanently skip an unreadable older minute.
    // Method: refuse the first sealed minute, resume, restore it, and resume the same store.
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new();
    for minute in 0..3 {
        if minute > 0 {
            fixture.skip_run_ticks(59);
        }
        fixture.publish_with_events(100 + minute * 60, &[]);
        fixture.seal_minute(minute);
    }
    for second in 221..230 {
        fixture.publish_with_events(second, &[]);
    }
    let ticks = minute_ticks(&fixture, 0);
    let original = std::fs::read(&ticks).unwrap();
    take_minute_offline(&fixture, 0);
    let store = Store::start(&fixture.root).unwrap();
    let initial = view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.publication == 12)
    });
    assert_eq!(initial.left_out, Some(super::Failure::Refused));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);

    store.set_active(true);
    let still_refused = view_where(&store, |_| true);

    assert_eq!(still_refused.left_out, Some(super::Failure::Refused));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    std::fs::write(&ticks, original).unwrap();
    // The recording opens only files of the owner-only mode it writes, which the rewrite lost.
    std::fs::set_permissions(&ticks, std::fs::Permissions::from_mode(0o600)).unwrap();

    store.set_active(true);
    let recovered = view_where(&store, |_| true);

    let fresh = Store::start(&fixture.root).unwrap();
    assert_eq!(recovered.left_out, None);
    assert_eq!(
        seconds_of(&recovered),
        seconds_of(&view_where(&fresh, |_| true))
    );
}

#[test]
fn a_refused_predecessor_does_not_discard_accepted_history() {
    // Goal: a refused checkpoint minute is distinct from one outside the loaded hour.
    // Method: retain two minutes, refuse the newer one, then catch up nine more minutes whose
    // publications overrun the fixture's eight-slot ring.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(160, &[]);
    fixture.seal_minute(1);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |_| true));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    take_minute_offline(&fixture, 1);
    for minute in 2..=10 {
        fixture.skip_run_ticks(59);
        fixture.publish_with_events(100 + minute * 60, &[]);
        fixture.seal_minute(minute);
    }

    store.set_active(true);
    let resumed = view_where(&store, |_| true);

    assert_eq!(resumed.left_out, Some(super::Failure::Refused));
    assert_eq!(
        seconds_of(&resumed),
        (0..=10).map(|minute| 100 + minute * 60).collect::<Vec<_>>()
    );
}

/// Spoils a minute's saved readings without changing discovery of its committed marker.
fn take_minute_offline(fixture: &Fixture, minute: u64) {
    let ticks = minute_ticks(fixture, minute);
    std::fs::remove_file(&ticks).unwrap();
    std::fs::write(&ticks, b"offline").unwrap();
}

fn minute_ticks(fixture: &Fixture, minute: u64) -> std::path::PathBuf {
    let marker = persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: minute,
    }
    .marker(&fixture.root);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet")
}

/// The wall second of each second `view` holds at one-second detail, oldest first.
fn seconds_of(view: &super::View) -> Vec<u64> {
    view.seconds
        .iter()
        .map(|second| second.wall_ns / 1_000_000_000)
        .collect()
}

#[test]
fn ordinary_ingestion_delivers_a_replacement_before_its_retained_view_exhausts_buffers() {
    // Goal: live ingestion hands over a replacement even when its current View holds old buffers.
    // Method: retain a full span, queue 65 real ring ticks, take the next View, then finish.
    let mut fixture = pressure_recording();
    let run = fixture.writer.identity().unwrap();
    let mut held = super::Held::new();
    for publication in 1..=1080 {
        fixture.publish_with_system_rows(publication);
        let second = MachineSecond {
            publication,
            run_tick: publication,
            wall_ns: publication * 1_000_000_000,
            ..MachineSecond::default()
        };
        assert!(held.take(
            &run,
            &second,
            super::apps::TickApps::Missing(&super::Missing::Unread)
        ));
    }
    let mut ring = super::Ring::new(fixture.root.clone());
    ring.follow().unwrap();
    ring.read(|run, second, apps| held.take(run, second, apps))
        .unwrap();
    let mailbox = Mailbox::new();
    held.publish(&mailbox);
    let retained = mailbox.take().unwrap();
    for second in 1081..=1145 {
        fixture.publish_with_system_rows(second);
    }

    super::step(
        &mut held,
        &mut ring,
        Ok(()),
        &mailbox,
        None,
        Publication::Progress,
    )
    .unwrap();
    let replacement = mailbox
        .take()
        .expect("a replacement must break buffer backpressure");

    assert_eq!(replacement.seconds.last().unwrap().publication, 1144);
    drop(retained);
    drop(replacement);
    super::step(
        &mut held,
        &mut ring,
        Ok(()),
        &mailbox,
        None,
        Publication::Progress,
    )
    .unwrap();
    assert_eq!(
        mailbox.take().unwrap().seconds.last().unwrap().publication,
        1145
    );
}

fn pressure_recording() -> Fixture {
    use collector::recording::{RegisteredRun, Registry};
    use collector::ring::{Place, RingReader, Shape, Writer};
    let mut fixture = Fixture::new_mode(false, 2, None);
    let reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let shape = Shape {
        slot_count: 120,
        ..reader.shape()
    };
    let identity = fixture.writer.identity().unwrap();
    let writer = Writer::create_recording(
        &Place {
            lock_path: fixture.root.join("pressure.lock"),
            name_prefix: format!("/fdpressure{}", std::process::id()),
        },
        shape,
        &identity,
        0,
    )
    .unwrap();
    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = writer;
    Registry {
        runs: vec![RegisteredRun {
            identity,
            ring_name: fixture.writer.name().to_owned(),
            collector_pid: 0,
        }],
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    fixture
}
