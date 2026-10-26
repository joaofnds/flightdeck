use super::tests::view_where;
use super::{MachineSecond, Mailbox, Publication, Store};
use crate::live::identity_fixture::Fixture;

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
