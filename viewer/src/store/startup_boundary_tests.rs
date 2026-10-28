//! Real-source observations at the staged startup boundaries.
use super::*;
use crate::live::identity_fixture::Fixture;
use crate::store::startup_tests::{
    drive_primary, published_view, query_context, query_outcome, usage,
};
use crate::store::{QueryOutcome, QueryReadiness, QueryUnavailable, StartupState};

#[test]
fn rollback_selection_follows_source_order_through_complete() {
    // Goal: following chooses source acceptance order when the wall clock rolls back.
    // Method: seal two minutes with later publications at smaller walls, then query both stages.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1));
    fixture.seal_minute(0);
    fixture.skip_run_ticks(58);
    fixture.publish_with_usage(900, &usage(60));
    fixture.publish_with_usage(901, &usage(61));
    fixture.seal_minute(1);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut stage = GuardStage::new(&fixture, &budget);

    primary(&mut stage, &fixture, &budget);
    let selected = query_context(&mut stage.held, &mut stage.ring);
    stage.complete(&fixture, &budget);
    let complete = query_context(&mut stage.held, &mut stage.ring);

    assert_eq!(selected.readiness(), QueryReadiness::Primary);
    assert_eq!(complete.readiness(), QueryReadiness::Complete);
    assert_eq!(complete.source, selected.source);
    assert_eq!(selected.selected().observation.stamp.publication, 4);
    assert_eq!(selected.selected().observation.run_tick, 61);
    assert_eq!(
        selected.selected().observation.wall_ns,
        Some(901_000_000_000)
    );
    assert_eq!(selected.context_last_second(), 901);
    assert_eq!(
        complete.selected().observation,
        selected.selected().observation
    );
    let payload = complete.selected().apps().processes.as_ref().unwrap();
    let process = payload.iter().find(|process| process.pid == 7).unwrap();
    assert_eq!(process.disk_read_bytes(), crate::apps::Amount::Known(17));
    assert_eq!(process.disk_written_bytes(), crate::apps::Amount::Known(43));
    assert_cohorts(&mut stage.held, 2);
    drop((selected, complete, stage));
    assert_eq!(budget.used(), 0);
}

#[test]
fn all_clockless_saved_ticks_complete_with_no_displayable_readings() {
    // Goal: a real source certificate cannot invent displayable clocked readings.
    // Method: save two actual ticks with absent wall clocks and complete every staged phase.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1));
    fixture.seal_with_observations(&[
        persistence::Observation {
            publication: 1,
            wall_ns: None,
            continuous_ns: Some(0),
            awake_ns: Some(0),
        },
        persistence::Observation {
            publication: 2,
            wall_ns: None,
            continuous_ns: Some(1_000_000_000),
            awake_ns: Some(1_000_000_000),
        },
    ]);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut stage = GuardStage::new(&fixture, &budget);

    primary(&mut stage, &fixture, &budget);
    let generation = stage.held.source_generation;
    stage.complete(&fixture, &budget);

    assert_absent(&mut stage);
    let catalog = stage.held.checked_catalog.as_ref().unwrap();
    assert_eq!(catalog.windows, 1);
    assert_eq!(catalog.unknown_clock_ticks, Some(2));
    assert_eq!(catalog.first_known_second, None);
    assert_eq!(catalog.last_known_second, None);
    assert_eq!(stage.held.source_generation, generation);
    assert_eq!(generation, 1);
    drop(stage);
    assert_eq!(budget.used(), 0);
}

#[test]
fn an_empty_registered_source_completes_with_a_truthful_empty_certificate() {
    // Goal: an empty schedule finishes without a fabricated selection or unchecked source.
    // Method: drive a real registered empty ring through staged discovery and completion.
    let fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut stage = GuardStage::new(&fixture, &budget);

    primary(&mut stage, &fixture, &budget);
    stage.complete(&fixture, &budget);

    assert_absent(&mut stage);
    let catalog = stage.held.checked_catalog.as_ref().unwrap();
    assert_eq!(catalog.windows, 0);
    assert_eq!(catalog.unknown_clock_ticks, Some(0));
    assert_eq!(catalog.first_known_second, None);
    assert_eq!(catalog.last_known_second, None);
    assert_eq!(stage.held.source_generation, 1);
    drop(stage);
    assert_eq!(budget.used(), 0);
}

#[test]
fn selected_boundary_cancellation_accepts_no_apps_or_source_certificate() {
    // Goal: cancellation at selected acceptance cannot turn machine progress into readiness.
    // Method: stop after the real selected predecessor step, cancel, then try acceptance/View.
    let fixture = crate::store::startup_tests::primary_fixture();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let cancellation = Arc::new(crate::Cancellation::default());
    let mut stage = GuardStage::new(&fixture, &budget);
    stage.operation = Arc::clone(&cancellation).operation(0).unwrap();
    advance_to(&mut stage, &fixture, &budget, Phase::Selected);
    let accepted = stage.accepted();
    cancellation.cancel().unwrap();

    let result = stage.advance(&fixture, &budget);

    assert_cancelled(result);
    assert_eq!(stage.accepted(), accepted);
    assert_eq!(stage.held.span.len(), 6);
    assert_eq!(stage.held.span.newest().unwrap().publication, 6);
    assert!(
        stage
            .held
            .apps
            .held()
            .all(|apps| apps.missing == Some(Missing::Unread))
    );
    assert!(stage.held.startup == StartupState::Loading);
    assert_eq!(stage.held.source_generation, 0);
    assert!(stage.held.checked_catalog.is_none());
    assert!(matches!(
        query_outcome(&mut stage.held, &mut stage.ring),
        QueryOutcome::NeedsLoad(crate::store::QueryNeedsLoad::UncheckedCoverage)
    ));
    assert_no_cancelled_view(&mut stage);
    drop(stage);
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_boundary_cancellation_preserves_primary_without_publishing_complete() {
    // Goal: complete App progress cannot bypass the final cancellation/readiness boundary.
    // Method: finish real history, cancel before its final completion step, then query/publish.
    let fixture = crate::store::startup_tests::primary_fixture();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let cancellation = Arc::new(crate::Cancellation::default());
    let mut stage = GuardStage::new(&fixture, &budget);
    stage.operation = Arc::clone(&cancellation).operation(0).unwrap();
    primary(&mut stage, &fixture, &budget);
    for _ in 0..MINUTES_MAX {
        if stage.startup.phase == Phase::History {
            if stage.startup.history_index == stage.startup.selected.unwrap() {
                break;
            }
        }
        stage.advance(&fixture, &budget).unwrap();
    }
    assert!(stage.startup.phase == Phase::History);
    assert_eq!(stage.startup.history_index, stage.startup.selected.unwrap());
    assert!(stage.held.apps.held().all(|apps| apps.missing.is_none()));
    let accepted = stage.accepted();
    let generation = stage.held.source_generation;
    cancellation.cancel().unwrap();

    let result = stage.advance(&fixture, &budget);

    assert_cancelled(result);
    assert_eq!(stage.accepted(), accepted);
    assert_eq!(stage.held.source_generation, generation);
    assert_eq!(generation, 1);
    assert!(stage.held.startup == StartupState::Primary);
    let context = query_context(&mut stage.held, &mut stage.ring);
    assert_eq!(context.readiness(), QueryReadiness::Primary);
    assert_eq!(context.selected().observation.stamp.publication, 6);
    assert_no_cancelled_view(&mut stage);
    drop((context, stage));
    assert_eq!(budget.used(), 0);
}

fn primary(stage: &mut GuardStage, fixture: &Fixture, budget: &Arc<ByteBudget>) {
    drive_primary(
        &mut stage.startup,
        &Input {
            root: &fixture.root,
            budget,
            operation: &stage.operation,
        },
        &mut stage.held,
        &mut stage.ring,
    );
}

fn advance_to(stage: &mut GuardStage, fixture: &Fixture, budget: &Arc<ByteBudget>, phase: Phase) {
    for _ in 0..MINUTES_MAX * 2 {
        if stage.startup.phase == phase {
            return;
        }
        stage.advance(fixture, budget).unwrap();
    }
    panic!("bounded startup did not reach its phase");
}

fn assert_absent(stage: &mut GuardStage) {
    assert!(stage.held.startup == StartupState::Complete);
    assert!(stage.held.span.is_empty());
    assert!(stage.held.apps.is_empty());
    assert!(stage.held.newest.is_none());
    assert!(
        crate::store::coverage::checked(&stage.held, &mut stage.ring)
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        query_outcome(&mut stage.held, &mut stage.ring),
        QueryOutcome::Unavailable(QueryUnavailable::NoReadings)
    ));
}

fn assert_cancelled(result: Result<Advance, Failure>) {
    assert!(
        matches!(result, Err(Failure::Read(Refused::Decode(Error::Io(error))))
        if error.kind() == std::io::ErrorKind::Interrupted)
    );
}

fn assert_no_cancelled_view(stage: &mut GuardStage) {
    let mailbox = crate::store::Mailbox::new();
    let result = crate::store::step(
        &mut stage.held,
        &mut stage.ring,
        Ok(()),
        &mailbox,
        Some(&stage.operation),
        crate::store::Publication::Complete,
    );
    assert!(matches!(result, Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert!(mailbox.take().is_none());
}

fn assert_cohorts(held: &mut Held, count: usize) {
    let view = published_view(held);
    assert!(view.apps_loaded);
    let cohorts = crate::store::tests::charted_cohorts(&view);
    assert_eq!(cohorts.seconds, count);
    assert!(!cohorts.refused);
    assert_eq!(cohorts.resident.len(), 1);
    assert_eq!(cohorts.resident[0].bytes.as_slice(), b"/Apps/Current.app");
    assert_eq!(cohorts.cpu.len(), 1);
    assert_eq!(cohorts.cpu[0].bytes.as_slice(), b"/Apps/Current.app");
    assert_eq!(cohorts.disk.len(), 1);
    assert_eq!(cohorts.disk[0].bytes.as_slice(), b"/Apps/Current.app");
}

#[test]
fn full_staged_retention_reuses_sample_spares_and_keeps_a_detached_primary_immutable() {
    // Goal: full real retention progresses past both spare bounds while a Primary stays immutable.
    // Method: seal 2375 ticks, replace 12 older targets in one minute, then append 65 native ticks.
    let mut fixture = large_fixture();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let trace =
        persistence::work::ingestion::Trace::new(fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    let mut stage = GuardStage::new(&fixture, &budget);
    primary(&mut stage, &fixture, &budget);
    let primary = query_context(&mut stage.held, &mut stage.ring);
    let headers = primary_headers(&primary);
    let generation = stage.held.source_generation;
    assert_large_primary(&stage.held, &primary);

    stage.advance(&fixture, &budget).unwrap();
    stage.advance(&fixture, &budget).unwrap();
    assert_eq!(historical_apps(&trace), 12);
    assert_eq!(stage.held.sampled.as_ref().unwrap().free(), 8);
    assert_sampled_prefix(&stage.held);
    append_native(&mut fixture, &mut stage);
    stage.complete(&fixture, &budget);

    assert!(stage.held.startup == StartupState::Complete);
    assert_eq!(stage.held.source_generation, generation);
    assert_eq!(historical_apps(&trace), 1246);
    assert_large_final(&mut stage.held);
    assert_eq!(primary_headers(&primary), headers);
    assert_eq!(primary.readiness(), QueryReadiness::Primary);
    assert_primary_selected(&primary);
    drop((primary, stage, scope));
    assert_eq!(budget.used(), 0);
}

fn assert_large_primary(held: &Held, primary: &crate::store::QueryContext) {
    assert_eq!(held.span.len(), 1080);
    assert_eq!(held.span.get(0).unwrap().publication, 1296);
    assert_eq!(held.span.newest().unwrap().publication, 2375);
    assert_eq!(held.earlier.len(), 259);
    let sampled = held.sampled.as_ref().unwrap();
    assert_eq!(sampled.len(), 259);
    assert_eq!(sampled.free(), 8);
    for index in 0..259 {
        let sample = sampled.get(index).unwrap();
        let publication = 1 + 5 * u64::try_from(index).unwrap();
        assert_eq!(sample.observation.stamp.publication, publication);
        assert_eq!(sample.observation.run_tick, publication - 1);
        assert_eq!(
            sample.observation.wall_ns,
            Some((10000 + 2 * (publication - 1)) * 1_000_000_000)
        );
        assert_eq!(sample.machine.publication, publication);
        assert!(matches!(sample.ranks(), Err(Missing::Unread)));
    }
    assert_eq!(primary.seconds().len(), 960);
    assert_eq!(primary.seconds()[0].observation.stamp.publication, 1416);
    assert!(
        primary
            .seconds()
            .iter()
            .all(crate::store::QuerySecond::detached)
    );
    assert_primary_selected(primary);
}

fn assert_primary_selected(primary: &crate::store::QueryContext) {
    assert_eq!(primary.selected().observation.stamp.publication, 2375);
    assert_eq!(primary.selected().observation.run_tick, 2374);
    assert_eq!(
        primary.selected().observation.wall_ns,
        Some(13_669_000_000_000)
    );
    assert_eq!(primary.selected().machine().cpu_percent, Some(50.0));
    assert_eq!(primary.context_last_second(), 13669);
    let ranks = primary.selected().apps().ranks().unwrap();
    let resident = ranks
        .metrics
        .iter()
        .find(|rank| rank.metric == crate::live_app_response::Metric::Resident)
        .unwrap();
    let app = resident
        .entries
        .iter()
        .find(|entry| entry.app.bytes.as_slice() == b"/Apps/Current.app")
        .unwrap();
    assert_eq!(app.amount, 11);
}

fn primary_headers(context: &crate::store::QueryContext) -> Vec<(Observation, u64, Option<f64>)> {
    context
        .seconds()
        .iter()
        .map(|second| {
            (
                second.observation,
                second.machine().wall_ns,
                second.machine().cpu_percent,
            )
        })
        .collect()
}

fn assert_sampled_prefix(held: &Held) {
    let sampled = held.sampled.as_ref().unwrap();
    for index in 0..12 {
        let sample = sampled.get(index).unwrap();
        assert_eq!(
            sample.observation.stamp.publication,
            1 + 5 * u64::try_from(index).unwrap()
        );
        assert_sampled_resident(sample);
    }
    assert!(matches!(
        sampled.get(12).unwrap().ranks(),
        Err(Missing::Unread)
    ));
    assert_sampled_disk(sampled.get(1).unwrap());
}

fn assert_sampled_resident(sample: &SampledSecond) {
    let ranks = sample.ranks().unwrap();
    assert_eq!(ranks.selected, sample.observation);
    let resident = ranks
        .metrics
        .iter()
        .find(|rank| rank.metric == crate::live_app_response::Metric::Resident)
        .unwrap();
    let app = resident
        .entries()
        .find(|entry| entry.app.bytes.as_slice() == b"/Apps/Current.app")
        .unwrap();
    assert_eq!(app.amount, 11);
}

fn assert_sampled_disk(sample: &SampledSecond) {
    let ranks = sample.ranks().unwrap();
    let disk = ranks
        .metrics
        .iter()
        .find(|rank| rank.metric == crate::live_app_response::Metric::Disk)
        .unwrap();
    let app = disk
        .entries()
        .find(|entry| entry.app.bytes.as_slice() == b"/Apps/Current.app")
        .unwrap();
    assert_eq!(app.amount, 60);
    assert!((app.rate.unwrap() - 60.0).abs() <= f64::EPSILON * 60.0);
}

fn append_native(fixture: &mut Fixture, stage: &mut GuardStage) {
    for run_tick in 2375..2440 {
        fixture.publish_with_usage(11295 + run_tick, &usage(run_tick));
        crate::store::native_acceptance::read_native(
            &mut stage.held,
            &mut stage.ring,
            &stage.operation,
        )
        .unwrap();
        assert_eq!(stage.held.span.newest().unwrap().publication, run_tick + 1);
        if run_tick == 2375 {
            let payload = stage
                .held
                .apps
                .newest()
                .unwrap()
                .processes
                .as_ref()
                .unwrap();
            assert_eq!(payload.previous.unwrap().publication, 2375);
            let process = payload.iter().find(|process| process.pid == 7).unwrap();
            assert_eq!(process.disk_read_bytes(), crate::apps::Amount::Known(17));
            assert_eq!(process.disk_written_bytes(), crate::apps::Amount::Known(43));
        }
    }
}

fn assert_large_final(held: &mut Held) {
    assert_eq!(held.span.len(), 1080);
    assert_eq!(held.span.get(0).unwrap().publication, 1361);
    assert_eq!(held.span.newest().unwrap().publication, 2440);
    assert_eq!(held.span.newest().unwrap().run_tick, 2439);
    assert_eq!(held.span.newest().unwrap().wall_ns, 13_734_000_000_000);
    assert!(held.apps.held().all(|apps| apps.missing.is_none()));
    let sampled = held.sampled.as_ref().unwrap();
    assert_eq!(sampled.len(), 259);
    assert_eq!(sampled.get(0).unwrap().observation.stamp.publication, 36);
    assert_eq!(
        sampled.newest().unwrap().observation.stamp.publication,
        1356
    );
    for sample in sampled.held() {
        assert_sampled_resident(sample);
        assert_eq!(
            sample.machine.publication,
            sample.observation.stamp.publication
        );
    }
    assert_sampled_disk(sampled.newest().unwrap());
    assert_cohorts(held, 960);
}

fn historical_apps(trace: &persistence::work::ingestion::Trace) -> usize {
    use persistence::work::ingestion::{Key, Kind, Purpose};
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    events
        .iter()
        .filter(|event| event.kind == Kind::AppDerivation)
        .filter(|event| event.purpose == Purpose::Ordinary)
        .filter(|event| {
            matches!(
                event.key,
                Key::Publication {
                    publication: 1..=2375,
                    ..
                }
            )
        })
        .count()
}

fn large_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    let mut reader = collector::ring::RingReader::open_generation(fixture.writer.name()).unwrap();
    let shape = reader.shape();
    for minute in 0..40 {
        let mut window = persistence::Window::new(
            persistence::WindowIdentity {
                run: fixture.writer.identity().unwrap(),
                window: minute,
            },
            persistence::Limits::from_shape(&shape).unwrap(),
        );
        for run_tick in minute * 60..((minute + 1) * 60).min(2375) {
            let wall = if run_tick < 1295 {
                10000 + 2 * run_tick
            } else {
                11295 + run_tick
            };
            fixture.publish_with_usage(wall, &usage(run_tick));
            large_fixture_capture(&mut reader, &mut window);
        }
        large_fixture_commit(&fixture.root, &window);
    }
    drop(reader);
    fixture
}

fn large_fixture_capture(
    reader: &mut collector::ring::RingReader,
    window: &mut persistence::Window,
) {
    use collector::ring::{Found, FoundEvent, IdentityObservation};
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    let mut identities = Vec::with_capacity(2);
    let Found::Publication { publication, .. } = reader
        .read_identity_into(&mut tick, &mut identities)
        .unwrap()
    else {
        panic!("fixture tick missing");
    };
    assert_eq!(tick.run_tick / 60, window.identity.window);
    assert!(tick.events.end - tick.events.start <= 32);
    let mut events = Vec::new();
    let mut event_identities = Vec::new();
    for expected in tick.events.clone() {
        let mut identity = IdentityObservation::EMPTY;
        let FoundEvent::Event {
            number,
            tick: run_tick,
            event,
        } = reader.read_event_identity(&mut identity).unwrap()
        else {
            panic!("fixture event missing");
        };
        assert_eq!(number, expected);
        assert_eq!(run_tick, tick.run_tick);
        events.push((number, event));
        event_identities.push(identity);
    }
    let numbered: Vec<_> = events
        .iter()
        .map(|(number, event)| persistence::NumberedEvent {
            number: *number,
            tick: tick.run_tick,
            event,
        })
        .collect();
    let times = tick.times.unwrap();
    window
        .push_tick_with_identity(
            &persistence::Observation {
                publication,
                wall_ns: Some(times.wall_ns),
                continuous_ns: Some(times.continuous_ns),
                awake_ns: Some(times.awake_ns),
            },
            &tick,
            &numbered,
            &persistence::IdentitySource {
                reader,
                processes: &identities,
                events: &event_identities,
            },
        )
        .unwrap();
}

fn large_fixture_commit(root: &Path, window: &persistence::Window) {
    let store = persistence::Store::open(root).unwrap();
    let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
        panic!("fixture window already committed");
    };
    let tables = persistence::Encoder.encode(window, pending.path()).unwrap();
    let publications = window.publication_range();
    let events = window.event_range();
    let min = window
        .observations()
        .map(|observation| observation.wall_ns.unwrap())
        .min()
        .unwrap();
    let max = window
        .observations()
        .map(|observation| observation.wall_ns.unwrap())
        .max()
        .unwrap();
    store
        .commit(
            pending,
            Manifest {
                identity: window.identity,
                publication_first: publications.start,
                publication_end: publications.end,
                events_first: events.start,
                events_end: events.end,
                wall_ns: Some(persistence::TimeBounds { min, max }),
                continuous_ns: None,
                awake_ns: None,
                losses: persistence::Losses::default(),
                data_directory: String::new(),
                tables,
            },
        )
        .unwrap();
}
