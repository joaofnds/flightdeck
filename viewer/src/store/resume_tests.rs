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
    let mut held = super::Held::new();
    let mut ring = super::Ring::new(fixture.root.clone());
    let cancellation = std::sync::Arc::new(crate::cancellation::Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    super::startup(&fixture.root, &mut held, &mut ring, &operation).unwrap();
    let mut buffers = super::recording::LoadBuffers::default();
    ring.follow().unwrap();
    ring.read(|run, second, apps| held.take(run, second, apps))
        .unwrap();
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
    use std::sync::atomic::{AtomicBool, AtomicU64};
    super::Shared {
        mailbox: Mailbox::new(),
        stop: AtomicBool::new(false),
        active: AtomicBool::new(true),
        catch_up: AtomicBool::new(false),
        catching_up: AtomicBool::new(true),
        paused: AtomicBool::new(false),
        window_seconds: AtomicU64::new(300),
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
    let expected = **known.seconds.last().unwrap();
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
fn a_half_hour_resume_loads_the_selected_five_minutes_and_its_lead_in() {
    // Goal: resume replaces the old window with the selected period, not the missing half hour.
    // Method: pause a real five-minute store, seal thirty new minutes, and inspect its first View.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |_| true));
    assert!(store.set_window(300));
    drop(view_where(&store, |_| true));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    for minute in 1..=30 {
        fixture.skip_run_ticks(59);
        fixture.publish_with_events(100 + minute * 60, &[]);
        fixture.seal_minute(minute);
    }

    store.set_active(true);
    let resumed = view_where(&store, |_| true);

    let seconds: Vec<_> = resumed
        .seconds
        .iter()
        .map(|second| second.wall_ns / 1_000_000_000)
        .collect();
    assert_eq!(seconds, [1540, 1600, 1660, 1720, 1780, 1840, 1900]);
    assert_eq!(resumed.failure, None);
    drop(resumed);
    assert!(store.set_window(3600));
    let expanded = view_where(&store, |view| view.seconds.len() == 31);
    assert_eq!(expanded.seconds.first().unwrap().wall_ns, 100_000_000_000);
    assert_eq!(expanded.seconds.last().unwrap().wall_ns, 1_900_000_000_000);
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
