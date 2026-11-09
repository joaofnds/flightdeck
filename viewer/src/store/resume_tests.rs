use std::time::Instant;

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
    let operation = std::sync::Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
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
    assert!(super::refresh(&fixture.root, &mut held, &mut ring, &shared, &operation,).unwrap());
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

/// The store thread's held hour over `fixture` before its first step, following the ring past
/// the newest sealed minute.
fn reconciliation_held(fixture: &Fixture) -> (super::Held, super::Ring) {
    let mut held = super::Held::without_recording();
    let mut ring = super::make_ring(&fixture.root, &held.payload_budget);
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
        &std::sync::Arc::new(crate::cancellation::Cancellation::default())
            .operation(0)
            .unwrap(),
        Publication::Complete,
    )
    .unwrap();
}

fn reconciliation_shared() -> super::Shared {
    use std::sync::atomic::AtomicBool;
    super::Shared {
        queries: crate::store::query::Queries::new(),
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

    let session = crate::LiveSession::open_with(&fixture.root, cancellation, 0).unwrap();

    assert!(session.catalog()["last_second"].is_null());
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
    assert!(expected.follows());
    assert_eq!(*resumed.seconds[index], expected);
    assert_eq!(resumed.apps[index], expected_apps);
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
fn a_refused_minute_is_read_again_with_the_next_read_of_the_hour() {
    // Goal: a successful suffix cannot permanently skip an unreadable older minute, which the
    // Store's next read of the hour tries again.
    // Method: refuse the first sealed minute, hide the store, restore the minute, overrun the
    // ring while hidden, and show the same store again.
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
    std::fs::write(&ticks, original).unwrap();
    // The recording opens only files of the owner-only mode it writes, which the rewrite lost.
    std::fs::set_permissions(&ticks, std::fs::Permissions::from_mode(0o600)).unwrap();
    // More publications than the fixture's eight-slot ring holds, so the show reads the hour.
    for second in 230..240 {
        fixture.publish_with_events(second, &[]);
    }
    let newest = |view: &super::View| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 239_000_000_000)
    };

    store.set_active(true);
    let recovered = view_where(&store, newest);

    let fresh = Store::start(&fixture.root).unwrap();
    assert_eq!(recovered.left_out, None);
    assert_eq!(
        seconds_of(&recovered),
        seconds_of(&view_where(&fresh, newest))
    );
}

#[test]
fn a_resume_beyond_ring_retention_reads_only_the_minutes_sealed_since_its_newest_second() {
    // Goal: a return keeps the held hour and reads only what the recording sealed while hidden.
    // Method: hold two sealed minutes, hide, seal nine more whose publications overrun the
    // fixture's eight-slot ring, take the oldest offline, show.
    let (fixture, store) = hidden_past_the_ring();
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
fn a_query_asked_as_a_return_past_the_ring_begins_hears_the_hour_the_store_caught_up_to() {
    // Goal: a page that asks as the window returns past the ring hears the readings the Store
    // caught up to, rather than that it has not yet checked what it holds, which leaves the page
    // to ask again only at its next poll.
    // Method: hold two sealed minutes, hide, seal nine more whose publications overrun the
    // fixture's eight-slot ring and publish three after them, which the Store takes in a step
    // after it reads the sealed minutes, show, ask for the newest reading at once and read the
    // answer.
    let (mut fixture, store) = hidden_past_the_ring();
    for second in 701..=703 {
        fixture.publish_with_events(second, &[]);
    }
    let handle = store.query_handle();

    store.set_active(true);
    handle
        .submit(super::QueryRequest {
            epoch: 0,
            source: None,
            selection: super::QuerySelection::FollowLatest,
        })
        .unwrap();
    let outcome = super::tests::next_reply(&handle);

    let super::QueryOutcome::Ready(context) = outcome else {
        panic!("the Store answered {outcome:?}");
    };
    assert_eq!(context.selected_second(), 703);
}

#[test]
fn a_query_asked_as_a_return_over_a_lapped_ring_hears_the_step_that_catches_up() {
    // Goal: a page that asks as the window returns hears the Store from the step that ends its
    // catch-up, even where its ring lost readings so that it can say only that it has not checked
    // what it holds, rather than after the Store's wait on a ring that publishes nothing.
    // Method: hold two sealed minutes, hide, seal nine more, then five times publish twelve
    // readings, which lap the fixture's eight-slot ring, show, ask at once, time the answer and
    // hide, and take the quickest. A wait costs every answer most of `WAIT`, however fast the
    // machine, and one quick answer shows it ended, however busy the machine is for the others.
    let (mut fixture, store) = hidden_past_the_ring();
    let handle = store.query_handle();

    let quickest = (0_u64..5)
        .map(|attempt| {
            for second in 701 + 12 * attempt..713 + 12 * attempt {
                fixture.publish_with_events(second, &[]);
            }
            store.set_active(true);
            let started = Instant::now();
            handle
                .submit(super::QueryRequest {
                    epoch: 0,
                    source: None,
                    selection: super::QuerySelection::FollowLatest,
                })
                .unwrap();
            handle.wake_store();
            let outcome = super::tests::next_reply(&handle);
            let elapsed = started.elapsed();
            store.set_active(false);
            super::activity_tests::wait_until_paused(&store);
            assert!(
                matches!(
                    outcome,
                    super::QueryOutcome::NeedsLoad(super::QueryNeedsLoad::UncheckedCoverage)
                ),
                "{outcome:?}"
            );
            elapsed
        })
        .min()
        .unwrap();

    assert!(quickest < super::WAIT / 2, "{quickest:?}");
}

/// A Store that held two sealed minutes and was hidden while nine more were sealed, whose
/// publications overrun the fixture's eight-slot ring. The newest sealed reading is second 700.
fn hidden_past_the_ring() -> (Fixture, Store) {
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(160, &[]);
    fixture.seal_minute(1);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |view| seconds_of(view) == [100, 160]));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    for minute in 2..=10 {
        fixture.skip_run_ticks(59);
        fixture.publish_with_events(100 + minute * 60, &[]);
        fixture.seal_minute(minute);
    }

    (fixture, store)
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
    drop(view_where(&store, |view| seconds_of(view) == [100, 160]));
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

#[test]
fn a_resume_beyond_ring_retention_reduces_only_the_publications_it_takes_in() {
    // Goal: the minute a return continues from, whose seconds the store holds, costs no second
    // machine reduction, so each publication keeps one.
    // Method: trace a return past the ring's retention and count each publication's ordinary
    // reductions. The return decodes its minutes at once, so the trace holds them in no fixed
    // order.
    use persistence::work::ingestion::{Key, Kind, Purpose};
    let traced = traced_return_past_the_ring();

    let reduced: Vec<_> = traced
        .events
        .iter()
        .filter(|event| event.kind == Kind::MachineReduction)
        .map(|event| (event.key, event.purpose))
        .collect();

    let ordinary_reductions: Vec<_> = (3..=11)
        .map(|publication| {
            let run = traced.run;
            let expected = (Key::Publication { run, publication }, Purpose::Ordinary);
            reduced.iter().filter(|event| **event == expected).count()
        })
        .collect();
    assert_eq!(reduced.len(), 9);
    assert_eq!(ordinary_reductions, [1; 9]);
    assert_eq!(
        traced.seconds,
        (0..=10).map(|minute| 100 + minute * 60).collect::<Vec<_>>()
    );
}

#[test]
fn a_resume_beyond_ring_retention_reads_the_minute_it_continues_from_as_predecessor_work() {
    // Goal: what a return reads of the minute it continues from is classed apart from the
    // ingestion of new readings.
    // Method: trace a return past the ring's retention and list the purpose of each event of
    // the minute holding publication 2, which reading it records, and of publication 2 itself,
    // which replaying its frames records.
    use persistence::work::ingestion::{Key, Purpose};
    let traced = traced_return_past_the_ring();
    let minute = Key::Minute {
        run: traced.run,
        first: 2,
        end: 3,
    };
    let publication = Key::Publication {
        run: traced.run,
        publication: 2,
    };

    let [read, replayed] = [minute, publication].map(|key| {
        traced
            .events
            .iter()
            .filter(|event| event.key == key)
            .map(|event| event.purpose)
            .collect::<Vec<_>>()
    });

    assert!(!read.is_empty());
    assert_eq!(read, vec![Purpose::Predecessor; read.len()]);
    assert!(!replayed.is_empty());
    assert_eq!(replayed, vec![Purpose::Predecessor; replayed.len()]);
}

/// What a store holding two sealed minutes records while it returns past the ring's retention,
/// nine minutes later, and the seconds it then holds.
struct TracedReturn {
    run: collector::ring::RunIdentity,
    events: Vec<persistence::work::ingestion::Event>,
    seconds: Vec<u64>,
}

/// Holds two sealed minutes, seals nine more whose publications overrun the fixture's eight-slot
/// ring, then traces the return's refresh and the steps after it.
fn traced_return_past_the_ring() -> TracedReturn {
    let mut fixture = Fixture::new();
    fixture.publish_with_events(100, &[]);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_events(160, &[]);
    fixture.seal_minute(1);
    let (mut held, mut ring) = reconciliation_held(&fixture);
    for minute in 2..=10 {
        fixture.skip_run_ticks(59);
        fixture.publish_with_events(100 + minute * 60, &[]);
        fixture.seal_minute(minute);
    }
    let shared = reconciliation_shared();
    let operation = std::sync::Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let run = fixture.writer.identity().unwrap();
    let trace = persistence::work::ingestion::Trace::new(run.recording);

    let scope = trace.enter();
    assert!(super::refresh(&fixture.root, &mut held, &mut ring, &shared, &operation).unwrap());
    for _ in 0..3 {
        reconciliation_step(&mut held, &mut ring, &shared);
    }
    drop(scope);

    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    TracedReturn {
        run,
        events,
        seconds: seconds_of(&shared.mailbox.take().unwrap()),
    }
}

#[test]
fn a_resume_beyond_ring_retention_gives_the_reading_after_its_predecessor_a_fresh_stores_rates() {
    // Goal: the minute a return continues from still gives the first reading after it its
    // machine and App rates.
    // Method: hold one sealed reading, hide, seal the eight readings after it in the next
    // minute and eight more in the minute after, which overrun the fixture's eight-slot ring,
    // show, and compare the first reading after the held one with what a fresh store holds.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(100);
    fixture.seal_minute(0);
    let store = Store::start(&fixture.root).unwrap();
    drop(view_where(&store, |view| seconds_of(view) == [100]));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);
    for second in 101..=108 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal_minute(1);
    fixture.skip_run_ticks(52);
    for second in 109..=116 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal_minute(2);

    store.set_active(true);
    let resumed = view_where(&store, |_| true);

    let fresh = Store::start(&fixture.root).unwrap();
    let expected = view_where(&fresh, |view| seconds_of(view) == seconds_of(&resumed));
    assert_eq!(seconds_of(&resumed), (100..=116).collect::<Vec<_>>());
    assert!(expected.seconds[1].follows());
    assert_eq!(resumed.seconds[1], expected.seconds[1]);
    assert_eq!(resumed.apps[1], expected.apps[1]);
}

/// Spoils a minute's saved readings without changing discovery of its committed marker.
fn take_minute_offline(fixture: &Fixture, minute: u64) {
    let ticks = minute_ticks(fixture, minute);
    std::fs::remove_file(&ticks).unwrap();
    std::fs::write(&ticks, b"offline").unwrap();
}

fn minute_ticks(fixture: &Fixture, minute: u64) -> std::path::PathBuf {
    let marker = fixture.marker(minute);
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
fn ordinary_ingestion_delivers_every_reading_while_a_retained_view_holds_the_buffers() {
    // Goal: live ingestion hands over a replacement even when its current View holds old buffers.
    // Method: retain a full span, queue 65 real ring ticks, take the next View, then finish.
    let mut fixture = pressure_recording();
    let run = fixture.writer.identity().unwrap();
    let mut held = super::Held::without_recording();
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
    let mut ring = super::make_ring(&fixture.root, &held.payload_budget);
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
        &std::sync::Arc::new(crate::cancellation::Cancellation::default())
            .operation(0)
            .unwrap(),
        Publication::Progress,
    )
    .unwrap();
    let replacement = mailbox.take().expect("a View of the new readings");

    assert_eq!(replacement.seconds.last().unwrap().publication, 1145);
    assert_eq!(retained.seconds.last().unwrap().publication, 1080);
}

fn pressure_recording() -> Fixture {
    use collector::recording::{RegisteredRun, Registry};
    use collector::ring::{Place, RingReader, Shape, Writer};
    let mut fixture = Fixture::new_mode(false, 2, None);
    let reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let shape = Shape {
        slot_count: 120,
        // The one event of each tick the ring holds and of the one it publishes.
        event_capacity: 121,
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

/// A recording with one damaged saved minute, whose Store holds what a load left behind when the
/// recording's newest run changed under it.
fn run_changed_during_load() -> (Fixture, super::Held, super::Ring, super::Shared) {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal_minute(0);
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(160);
    fixture.seal_minute(1);
    super::tests::empty_ticks_file(&fixture, 0);
    let budget = std::sync::Arc::new(super::ByteBudget::new(256 * 1024 * 1024));
    let mut held = super::Held::new(fixture.root.clone(), budget.clone());
    let ring = super::make_ring(&fixture.root, &budget);
    held.left_out = Some(super::Failure::ChangedIdentity);

    (fixture, held, ring, reconciliation_shared())
}

/// Reads the hour as the Store's next pass does.
fn run_changed_refresh(fixture: &Fixture, held: &mut super::Held, ring: &mut super::Ring) -> bool {
    let shared = reconciliation_shared();
    let cancellation = std::sync::Arc::new(crate::cancellation::Cancellation::default());
    let operation = cancellation.operation(0).unwrap();

    super::refresh(&fixture.root, held, ring, &shared, &operation).unwrap()
}

#[test]
fn a_run_registered_during_a_load_reads_the_hour_again() {
    // Goal: a recorder that registers its run while the Store reads the hour costs one more read,
    // since a load with no catalog would leave every page without an answer.
    // Method: leave the Store as that load leaves it, take its retry and read the hour.
    let (fixture, mut held, mut ring, shared) = run_changed_during_load();

    let retried = super::run_retry(&mut held, &ring, &shared);
    let loaded = run_changed_refresh(&fixture, &mut held, &mut ring);

    assert!(retried);
    assert!(shared.catch_up.load(std::sync::atomic::Ordering::Acquire));
    assert!(loaded);
    assert_eq!(held.left_out, Some(super::Failure::Refused));
    assert!(held.checked_catalog.is_some());
}

#[test]
fn a_run_gone_from_the_recording_starts_the_load_over() {
    // Goal: the retry reaches a recording that no longer lists the run the Store held, where a
    // load continuing from that run would refuse again at every pass.
    // Method: hold a place in a run the recording does not list, take the retry and read the hour.
    let (fixture, mut held, mut ring, shared) = run_changed_during_load();
    let mut run = fixture.writer.identity().unwrap();
    run.run.0[0] ^= 1;
    held.newest = Some(super::Newest {
        run,
        publication: 1,
        segment: 0,
    });

    super::run_retry(&mut held, &ring, &shared);
    run_changed_refresh(&fixture, &mut held, &mut ring);

    assert_eq!(held.left_out, Some(super::Failure::Refused));
    assert!(held.checked_catalog.is_some());
}

#[test]
fn a_minute_left_out_for_its_damage_reads_no_hour_again_at_once() {
    // Goal: a damaged minute stays left out without the Store rereading the hour at every pass.
    // Method: leave the Store with a refused minute and ask for its retry.
    let (_fixture, mut held, ring, shared) = run_changed_during_load();
    held.left_out = Some(super::Failure::Refused);

    let retried = super::run_retry(&mut held, &ring, &shared);

    assert!(!retried);
    assert!(!shared.catch_up.load(std::sync::atomic::Ordering::Acquire));
}
