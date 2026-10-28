//! Sealed candidates age while the actual staged caller admits native successors.
use super::*;
use crate::live::identity_fixture::Fixture;
use crate::store::startup_tests::{primary_fixture, usage};
use crate::store::{Buffers, MachineSecond, Span, StartupState};
use persistence::work::ingestion::{Event, Key, Kind, Purpose, Trace};

#[test]
fn pending_recent_candidates_age_to_older_and_outside_without_rederivation() {
    // Goal: native progress reclassifies admitted history without losing its source predecessor.
    // Method: pin a retired recent spare, then append two real ticks between denied retries.
    let mut run = PendingStage::new(recent_fixture(), 6, RECENT_TICKS);
    let budget = Arc::clone(&run.budget);
    let trace = Trace::new(run.fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    run.reach_phase(Phase::SelectedBoundary);
    let retired = Arc::clone(run.stage.held.apps.newest_held().unwrap());
    assert_eq!(retired.observation, Some(run.observation(6)));
    assert_eq!(retired.missing, Some(Missing::Unread));
    run.reach_primary();
    assert_eq!(Arc::strong_count(&retired), 2);
    run.assert_recent_push_room();
    run.reach_phase(Phase::History);
    assert!(!run.turn());
    run.assert_pending_recent(1);
    run.assert_progress(&ReplayPosition {
        minute: 0,
        next_endpoint: 0,
        previous: None,
    });
    assert_stable_denial(&mut run, &trace, 1);
    let opened = table_work(&trace, &run.key(0));

    run.fixture.publish_with_usage(1064, &usage(122));
    assert!(!run.turn());
    assert_recent_aged_to_older(&run, &trace);
    assert_reopened(&trace, &run.key(0), &opened);
    let reopened = table_work(&trace, &run.key(0));
    assert_stable_denial(&mut run, &trace, 2);

    run.fixture.publish_with_usage(1065, &usage(123));
    assert!(!run.turn());
    run.assert_native(Interval {
        selected: 8,
        previous: 7,
    });
    assert!(matches!(
        target(&run.stage.held, &run.observation(2)),
        Target::Outside
    ));
    assert!(run.stage.startup.pending.is_none());
    run.assert_progress(&ReplayPosition {
        minute: 0,
        next_endpoint: 2,
        previous: Some(2),
    });
    run.assert_unread(&[3, 4]);
    assert_eq!(table_work(&trace, &run.key(0)), reopened);
    assert_history_work(&trace, &run, &[1, 2]);
    drop(retired);
    run.complete();

    run.assert_retained(&Retained {
        recent: &[3, 4, 5, 6, 7, 8],
        older: &[1],
    });
    run.assert_recent_rates(Interval {
        selected: 3,
        previous: 2,
    });
    assert_sampled_resident(run.sampled(1), &run.observation(1));
    assert_history_work(&trace, &run, &[1, 2, 3, 4]);
    drop((run, scope));
    assert_eq!(budget.used(), 0);
}

#[test]
fn pending_older_candidate_resolves_outside_without_overwriting_the_native_sample() {
    // Goal: a retired sampled reader can deny replacement without denying native eviction.
    // Method: pin sampled pub1, stall admitted pub3, then let native pub7 sample resolved pub5.
    let mut run = PendingStage::new(primary_fixture(), 2, OLDER_TICKS);
    run.stage.held.earlier = Span::new(1, Buffers::new(2, MachineSecond::default));
    run.stage.held.sampled = Some(Span::new(1, Buffers::new(2, SampledSecond::empty)));
    let budget = Arc::clone(&run.budget);
    let trace = Trace::new(run.fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    run.reach_loaded(2);
    let retired = Arc::clone(run.sampled_held());
    assert_eq!(retired.observation, run.observation(1));
    assert_eq!(retired.ranks(), Err(&Missing::Unread));
    run.reach_primary();
    assert_eq!(Arc::strong_count(&retired), 2);
    run.assert_retained(&Retained {
        recent: &[5, 6],
        older: &[3],
    });
    run.assert_sampled_push_room();
    assert_eq!(run.stage.held.apps.free(), 1);
    run.reach_phase(Phase::History);
    assert!(!run.turn());
    run.assert_pending_older(3);
    run.assert_progress(&ReplayPosition {
        minute: 1,
        next_endpoint: 0,
        previous: Some(2),
    });
    assert_eq!(run.sampled(3).ranks(), Err(&Missing::Unread));
    assert_stable_denial(&mut run, &trace, 3);
    let opened = table_work(&trace, &run.key(1));

    run.fixture.publish_with_usage(1122, &usage(122));
    assert!(!run.turn());
    run.assert_native(Interval {
        selected: 7,
        previous: 6,
    });
    assert_older_resolved(&run, &trace);
    assert_reopened(&trace, &run.key(1), &opened);
    drop(retired);
    run.complete();

    run.assert_retained(&Retained {
        recent: &[6, 7],
        older: &[5],
    });
    run.assert_recent_rates(Interval {
        selected: 7,
        previous: 6,
    });
    assert_sampled_resident(run.sampled(5), &run.observation(5));
    assert_sampled_disk(run.sampled(5));
    assert_eq!(
        publication_work(&trace, &run.observation(3), Kind::AppDerivation),
        1
    );
    assert_eq!(
        publication_work(&trace, &run.observation(4), Kind::AppDerivation),
        0
    );
    drop((run, scope));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_queued_primary_query_rejects_a_changed_witness_before_serving_a_reply() {
    // Goal: a queued query cannot bypass witness validation while history is capacity-blocked.
    // Method: receive a real Primary control, change its pending seal, then query the same caller.
    let mut run = PendingStage::new(recent_fixture(), 6, RECENT_TICKS);
    let budget = Arc::clone(&run.budget);
    let trace = Trace::new(run.fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    run.reach_phase(Phase::SelectedBoundary);
    let pinned = Arc::clone(run.stage.held.apps.newest_held().unwrap());
    run.reach_primary();
    run.reach_phase(Phase::History);
    assert!(!run.turn());
    run.assert_pending_recent(1);
    let handle = run
        .shared
        .queries
        .as_ref()
        .unwrap()
        .handle(std::thread::current());
    let selected = run.observation(6);

    let retained = run.primary_reply(&handle);

    assert_primary_query(&retained, &selected);
    let accepted = run.stage.accepted();
    let root = run.fixture.root.clone();
    change_loss_word(&root, &run.stage.startup.minutes[0].found.marker);
    let token = run.submit_primary_query(&handle);
    assert_eq!(
        token,
        crate::store::QueryToken {
            epoch: 0,
            sequence: 2
        }
    );

    let result = run.try_turn();

    assert!(
        matches!(result, Err(Failure::Read(Refused::Commitment(error)))
        if error.kind() == std::io::ErrorKind::InvalidData)
    );
    assert!(handle.take().is_none());
    assert!(handle.pending());
    assert!(run.shared.queries.as_ref().unwrap().requested());
    assert!(run.shared.mailbox.take().is_none());
    assert_eq!(run.stage.accepted(), accepted);
    run.assert_pending_recent(1);
    run.assert_progress(&ReplayPosition {
        minute: 0,
        next_endpoint: 0,
        previous: None,
    });
    assert_eq!(run.stage.held.newest.unwrap().publication, 6);
    assert_eq!(
        publication_work(&trace, &run.observation(1), Kind::AppDerivation),
        1
    );
    assert_primary_query(&retained, &selected);
    handle.cancel().unwrap();
    assert!(!handle.pending());
    assert!(!run.shared.queries.as_ref().unwrap().requested());
    drop((handle, pinned, run, scope, trace));
    assert!(!root.try_exists().unwrap());
    assert!(budget.used() > 0);
    assert_primary_query(&retained, &selected);
    drop(retained);
    assert_eq!(budget.used(), 0);
}

struct PendingStage {
    stage: GuardStage,
    shared: crate::store::Shared,
    blocked: Option<crate::store::StartupCapacity>,
    budget: Arc<ByteBudget>,
    fixture: Fixture,
    ticks: &'static [[u64; 3]],
}

#[derive(Clone, Copy)]
struct Interval {
    selected: u64,
    previous: u64,
}

struct ReplayPosition {
    minute: usize,
    next_endpoint: usize,
    previous: Option<u64>,
}

struct Retained<'readings> {
    recent: &'readings [u64],
    older: &'readings [u64],
}

const RECENT_TICKS: &[[u64; 3]] = &[
    [1, 58, 1000],
    [2, 59, 1001],
    [3, 60, 1002],
    [4, 61, 1003],
    [5, 120, 1062],
    [6, 121, 1063],
    [7, 122, 1064],
    [8, 123, 1065],
];
const OLDER_TICKS: &[[u64; 3]] = &[
    [1, 0, 1000],
    [2, 1, 1001],
    [3, 118, 1118],
    [4, 119, 1119],
    [5, 120, 1120],
    [6, 121, 1121],
    [7, 122, 1122],
];

impl PendingStage {
    fn new(fixture: Fixture, recent_capacity: usize, ticks: &'static [[u64; 3]]) -> Self {
        let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
        let mut stage = GuardStage::new(&fixture, &budget);
        stage.held.span = Span::new(
            recent_capacity,
            Buffers::new(recent_capacity + 1, MachineSecond::default),
        );
        stage.held.apps = Span::new(
            recent_capacity,
            Buffers::new(recent_capacity + 1, AppsSecond::default),
        );
        Self {
            stage,
            shared: startup_shared(),
            blocked: None,
            budget,
            fixture,
            ticks,
        }
    }

    fn turn(&mut self) -> bool {
        self.try_turn().unwrap()
    }

    fn try_turn(&mut self) -> Result<bool, Failure> {
        // The existing caller's park consumes this token; no timing threshold or gate is added.
        std::thread::current().unpark();
        crate::store::startup_staged_turn(
            &Input {
                root: &self.fixture.root,
                budget: &self.budget,
                operation: &self.stage.operation,
            },
            &mut self.stage.held,
            &mut self.stage.ring,
            &self.shared,
            &mut self.stage.startup,
            &mut self.blocked,
        )
    }

    fn submit_primary_query(&self, handle: &crate::store::QueryHandle) -> crate::store::QueryToken {
        let token = handle
            .submit(crate::store::QueryRequest {
                epoch: 0,
                source: Some(crate::store::SourceGeneration {
                    recording: self.fixture.writer.identity().unwrap().recording,
                    generation: 1,
                }),
                selection: crate::store::QuerySelection::FollowLatest,
            })
            .unwrap();
        assert!(self.shared.queries.as_ref().unwrap().requested());
        assert!(handle.pending());
        assert!(handle.take().is_none());
        token
    }

    fn primary_reply(
        &mut self,
        handle: &crate::store::QueryHandle,
    ) -> Arc<crate::store::QueryContext> {
        let token = self.submit_primary_query(handle);
        assert!(!self.turn());
        let reply = handle.take().unwrap();
        assert_eq!(reply.token, token);
        assert!(!handle.pending());
        assert!(!self.shared.queries.as_ref().unwrap().requested());
        let crate::store::QueryOutcome::Ready(context) = reply.outcome else {
            panic!("unchanged witness did not serve a real Ready control");
        };
        context
    }

    fn reach_phase(&mut self, phase: Phase) {
        for _ in 0..MINUTES_MAX * 2 {
            if self.stage.startup.phase == phase {
                return;
            }
            assert!(!self.turn());
        }
        panic!("staged caller did not reach the requested phase");
    }

    fn reach_loaded(&mut self, minute_count: usize) {
        for _ in 0..MINUTES_MAX * 2 {
            if self.stage.startup.cursor.next_minute_index == minute_count {
                assert!(self.stage.startup.phase == Phase::Machine);
                return;
            }
            assert!(!self.turn());
        }
        panic!("staged caller did not load the requested minutes");
    }

    fn reach_primary(&mut self) {
        for _ in 0..MINUTES_MAX * 2 {
            if self.stage.held.startup == StartupState::Primary {
                assert_eq!(self.stage.held.source_generation, 1);
                self.assert_pairing();
                return;
            }
            assert!(!self.turn());
        }
        panic!("staged caller did not become Primary");
    }

    fn complete(&mut self) {
        for _ in 0..MINUTES_MAX * 2 {
            if self.turn() {
                assert!(self.stage.held.startup == StartupState::Complete);
                assert!(self.stage.startup.phase == Phase::Complete);
                assert_eq!(self.stage.held.source_generation, 1);
                assert!(self.stage.held.checked_catalog.is_some());
                assert!(self.stage.held.failure.is_none());
                assert!(self.stage.startup.pending.is_none());
                self.assert_pairing();
                return;
            }
        }
        panic!("staged caller did not complete");
    }

    fn observation(&self, publication: u64) -> Observation {
        let [_, run_tick, wall_second] = *self
            .ticks
            .iter()
            .find(|tick| tick[0] == publication)
            .unwrap();
        let identity = self.fixture.writer.identity().unwrap();
        Observation {
            recording: identity.recording.0,
            stamp: crate::apps::Stamp {
                boot: identity.boot.0,
                run: identity.run.0,
                publication,
                continuous_ns: Some(run_tick * 1_000_000_000),
                awake_ns: Some(run_tick * 1_000_000_000),
            },
            run_tick,
            wall_ns: Some(wall_second * 1_000_000_000),
        }
    }

    fn key(&self, minute_index: usize) -> Key {
        let manifest = &self.stage.startup.minutes[minute_index]
            .witness
            .as_ref()
            .unwrap()
            .manifest;
        Key::Minute {
            run: self.fixture.writer.identity().unwrap(),
            first: manifest.publication_first,
            end: manifest.publication_end,
        }
    }

    fn recent(&self, publication: u64) -> &AppsSecond {
        self.stage
            .held
            .apps
            .held()
            .find(|apps| apps.observation == Some(self.observation(publication)))
            .unwrap()
    }

    fn sampled(&self, publication: u64) -> &SampledSecond {
        self.stage
            .held
            .sampled
            .as_ref()
            .unwrap()
            .held()
            .find(|sample| sample.observation == self.observation(publication))
            .unwrap()
    }

    fn sampled_held(&self) -> &Arc<SampledSecond> {
        self.stage
            .held
            .sampled
            .as_ref()
            .unwrap()
            .newest_held()
            .unwrap()
    }

    fn assert_recent_push_room(&self) {
        assert_eq!(self.stage.held.apps.free(), 0);
        assert_eq!(self.stage.held.apps.len(), 6);
        assert_eq!(
            Arc::strong_count(self.stage.held.apps.held().next().unwrap()),
            2
        );
        assert!(self.stage.held.apps.has_room());
        assert!(self.stage.held.span.has_room());
        self.assert_pairing();
    }

    fn assert_sampled_push_room(&self) {
        let sampled = self.stage.held.sampled.as_ref().unwrap();
        assert_eq!(sampled.free(), 0);
        assert_eq!(sampled.len(), 1);
        assert_eq!(Arc::strong_count(sampled.held().next().unwrap()), 2);
        assert!(sampled.has_room());
        assert!(self.stage.held.earlier.has_room());
        self.assert_pairing();
    }

    fn assert_pending_recent(&self, publication: u64) {
        let candidate = self.stage.startup.pending.as_ref().unwrap();
        let observation = self.observation(publication);
        assert_eq!(candidate.observation, observation);
        assert!(matches!(
            target(&self.stage.held, &observation),
            Target::Recent(_)
        ));
        assert!(candidate.sampled.is_none());
        assert_recent_resident(&candidate.apps, &observation);
        assert!(candidate.before.as_ref().unwrap().is_admitted());
        assert_eq!(
            candidate
                .before
                .as_ref()
                .unwrap()
                .previous()
                .0
                .observation(),
            observation
        );
        assert!(self.blocked.is_some());
        self.assert_recent_push_room();
    }

    fn assert_pending_older(&self, publication: u64) {
        let candidate = self.stage.startup.pending.as_ref().unwrap();
        let observation = self.observation(publication);
        assert_eq!(candidate.observation, observation);
        assert!(matches!(
            target(&self.stage.held, &observation),
            Target::Older(_)
        ));
        assert_sampled_resident(candidate.sampled.as_ref().unwrap(), &observation);
        assert!(candidate.before.as_ref().unwrap().is_admitted());
        assert_eq!(
            candidate
                .before
                .as_ref()
                .unwrap()
                .previous()
                .0
                .observation(),
            observation
        );
        assert!(self.blocked.is_some());
        self.assert_sampled_push_room();
    }

    fn assert_progress(&self, position: &ReplayPosition) {
        assert_eq!(
            self.stage.startup.minutes[position.minute].next_endpoint,
            position.next_endpoint
        );
        let accepted = self
            .stage
            .startup
            .cursor
            .decoder
            .predecessor
            .as_ref()
            .map(|before| before.previous().0.observation());
        assert_eq!(
            accepted,
            position
                .previous
                .map(|publication| self.observation(publication))
        );
        assert!(self.stage.held.startup == StartupState::Primary);
        assert_eq!(self.stage.held.source_generation, 1);
        assert!(self.stage.held.failure.is_none());
    }

    fn assert_unread(&self, publications: &[u64]) {
        for publication in publications {
            let apps = self.recent(*publication);
            assert_eq!(apps.missing, Some(Missing::Unread));
            assert_eq!(apps.processes.as_ref().unwrap_err(), &Missing::Unread);
        }
    }

    fn assert_native(&self, interval: Interval) {
        assert_eq!(
            self.stage.held.apps.newest().unwrap().observation,
            Some(self.observation(interval.selected))
        );
        self.assert_recent_rates(interval);
        assert_eq!(
            self.stage.held.newest.unwrap().publication,
            interval.selected
        );
        assert!(self.stage.held.failure.is_none());
        self.assert_pairing();
    }

    fn assert_retained(&self, expected: &Retained<'_>) {
        let actual: Vec<_> = self
            .stage
            .held
            .apps
            .held()
            .map(|apps| apps.observation.unwrap().stamp.publication)
            .collect();
        assert_eq!(actual, expected.recent);
        let actual: Vec<_> = self
            .stage
            .held
            .sampled
            .as_ref()
            .unwrap()
            .held()
            .map(|sample| sample.observation.stamp.publication)
            .collect();
        assert_eq!(actual, expected.older);
        self.assert_pairing();
    }

    fn assert_recent_rates(&self, interval: Interval) {
        let apps = self.recent(interval.selected);
        let observation = self.observation(interval.selected);
        let previous = self.observation(interval.previous);
        assert_recent_resident(apps, &observation);
        let payload = apps.processes.as_ref().unwrap();
        assert_eq!(payload.previous, Some(previous.stamp));
        assert_eq!(apps.elapsed_ns, Some(1_000_000_000));
        let process = payload.iter().find(|process| process.pid == 7).unwrap();
        assert_eq!(process.disk_read_bytes(), crate::apps::Amount::Known(17));
        assert_eq!(process.disk_written_bytes(), crate::apps::Amount::Known(43));
        let ranks = apps.ranks().unwrap();
        let disk = ranks
            .metrics
            .iter()
            .find(|ranking| ranking.metric == crate::live_app_response::Metric::Disk)
            .unwrap();
        let entry = disk
            .entries
            .iter()
            .find(|rank| rank.app.bytes.as_slice() == b"/Apps/Current.app")
            .unwrap();
        assert_eq!(entry.amount, 60);
        assert!((entry.rate.unwrap() - 60.0).abs() <= f64::EPSILON * 60.0);
    }

    fn assert_pairing(&self) {
        assert_eq!(self.stage.held.span.len(), self.stage.held.apps.len());
        for (machine, apps) in self.stage.held.span.held().zip(self.stage.held.apps.held()) {
            let observation = self.observation(machine.publication);
            assert_eq!(apps.observation, Some(observation));
            assert_eq!(Some(machine.wall_ns), observation.wall_ns);
        }
        let sampled = self.stage.held.sampled.as_ref().unwrap();
        assert_eq!(self.stage.held.earlier.len(), sampled.len());
        for (machine, sample) in self.stage.held.earlier.held().zip(sampled.held()) {
            assert_eq!(sample.observation, self.observation(machine.publication));
            assert_eq!(sample.machine, **machine);
        }
    }
}

fn assert_primary_query(context: &crate::store::QueryContext, selected: &Observation) {
    assert_eq!(context.readiness(), crate::store::QueryReadiness::Primary);
    assert_eq!(
        context.source,
        crate::store::SourceGeneration {
            recording: collector::ring::Identity(selected.recording),
            generation: 1,
        }
    );
    assert_eq!(context.selected().observation, *selected);
    assert_eq!(context.context_last_second(), 1063);
    assert_eq!(context.seconds().len(), 6);
    assert!(
        context
            .seconds()
            .iter()
            .all(crate::store::QuerySecond::detached)
    );
    assert_eq!(context.selected().machine().cpu_percent, Some(50.0));
    let selection = context
        .selection(&Arc::new(crate::Cancellation::default()), 0)
        .unwrap();
    assert_eq!(selection["selected"]["publication"], 6);
    assert_eq!(selection["selected"]["run_tick"], 121);
    assert_eq!(selection["selected"]["wall_ns"], 1_063_000_000_000_u64);
    assert_eq!(selection["summary"]["cpu_percent"], 50.0);
}

fn recent_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(58);
    fixture.publish_with_usage(1000, &usage(58));
    fixture.publish_with_usage(1001, &usage(59));
    fixture.seal_minute(0);
    fixture.publish_with_usage(1002, &usage(60));
    fixture.publish_with_usage(1003, &usage(61));
    fixture.seal_minute(1);
    fixture.skip_run_ticks(58);
    fixture.publish_with_usage(1062, &usage(120));
    fixture.publish_with_usage(1063, &usage(121));
    fixture.seal_minute(2);
    fixture
}

fn startup_shared() -> crate::store::Shared {
    use std::sync::atomic::AtomicBool;
    crate::store::Shared {
        mailbox: crate::store::Mailbox::new(),
        queries: Some(crate::store::query::Queries::new()),
        stop: AtomicBool::new(false),
        active: AtomicBool::new(true),
        catch_up: AtomicBool::new(false),
        catching_up: AtomicBool::new(true),
        paused: AtomicBool::new(false),
        cancellation: Arc::new(crate::Cancellation::default()),
    }
}

fn assert_stable_denial(run: &mut PendingStage, trace: &Trace, publication: u64) {
    let accepted = run.stage.accepted();
    let work = source_work(trace);
    let next_endpoint = run.stage.startup.minutes[run.stage.startup.history_index].next_endpoint;
    let before = run
        .stage
        .startup
        .cursor
        .decoder
        .predecessor
        .as_ref()
        .map(|before| before.previous().0.observation());
    assert!(!run.turn());
    assert_eq!(run.stage.accepted(), accepted);
    assert_eq!(source_work(trace), work);
    assert_eq!(
        run.stage.startup.pending.as_ref().unwrap().observation,
        run.observation(publication)
    );
    assert_eq!(
        run.stage.startup.minutes[run.stage.startup.history_index].next_endpoint,
        next_endpoint
    );
    assert_eq!(
        run.stage
            .startup
            .cursor
            .decoder
            .predecessor
            .as_ref()
            .map(|before| before.previous().0.observation()),
        before
    );
    assert!(run.blocked.is_some());
    assert!(run.stage.held.failure.is_none());
    assert_eq!(run.stage.held.source_generation, 1);
}

fn assert_recent_resident(apps: &AppsSecond, observation: &Observation) {
    assert_eq!(apps.observation, Some(*observation));
    assert_eq!(apps.missing, None);
    let payload = apps.processes.as_ref().unwrap();
    assert_eq!(payload.selected, *observation);
    let process = payload.iter().find(|process| process.pid == 7).unwrap();
    assert_eq!(process.resident_bytes(), crate::apps::Amount::Known(11));
    assert_eq!(process.footprint_bytes(), crate::apps::Amount::Known(12));
    let ranks = apps.ranks().unwrap();
    let resident = ranks
        .metrics
        .iter()
        .find(|ranking| ranking.metric == crate::live_app_response::Metric::Resident)
        .unwrap();
    let entry = resident
        .entries
        .iter()
        .find(|rank| rank.app.bytes.as_slice() == b"/Apps/Current.app")
        .unwrap();
    assert_eq!(entry.amount, 11);
}

fn assert_sampled_resident(sample: &SampledSecond, observation: &Observation) {
    assert_eq!(sample.observation, *observation);
    let ranks = sample.ranks().unwrap();
    assert_eq!(ranks.selected, *observation);
    let resident = ranks
        .metrics
        .iter()
        .find(|ranking| ranking.metric == crate::live_app_response::Metric::Resident)
        .unwrap();
    let entry = resident
        .entries()
        .find(|rank| rank.app.bytes.as_slice() == b"/Apps/Current.app")
        .unwrap();
    assert_eq!(entry.amount, 11);
}

fn assert_sampled_disk(sample: &SampledSecond) {
    let ranks = sample.ranks().unwrap();
    let disk = ranks
        .metrics
        .iter()
        .find(|ranking| ranking.metric == crate::live_app_response::Metric::Disk)
        .unwrap();
    let entry = disk
        .entries()
        .find(|rank| rank.app.bytes.as_slice() == b"/Apps/Current.app")
        .unwrap();
    assert_eq!(entry.amount, 60);
    assert!((entry.rate.unwrap() - 60.0).abs() <= f64::EPSILON * 60.0);
}

fn source_work(trace: &Trace) -> Vec<Event> {
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    events
        .into_iter()
        .filter(|event| {
            if event.purpose == Purpose::Ordinary {
                matches!(
                    event.kind,
                    Kind::AppDerivation
                        | Kind::CommittedFrameCapture
                        | Kind::ContentCapture
                        | Kind::ProjectionDecode
                        | Kind::AdmissionRead
                        | Kind::NativeCollect
                )
            } else {
                false
            }
        })
        .collect()
}

fn publication_work(trace: &Trace, observation: &Observation, kind: Kind) -> usize {
    let key = Key::Publication {
        run: collector::ring::RunIdentity {
            recording: collector::ring::Identity(observation.recording),
            boot: collector::ring::Identity(observation.stamp.boot),
            run: collector::ring::Identity(observation.stamp.run),
        },
        publication: observation.stamp.publication,
    };
    source_work(trace)
        .iter()
        .filter(|event| event.key == key)
        .filter(|event| event.kind == kind)
        .count()
}

fn minute_work(trace: &Trace, key: &Key, kind: Kind) -> usize {
    source_work(trace)
        .iter()
        .filter(|event| event.key == *key)
        .filter(|event| event.kind == kind)
        .count()
}

#[derive(Debug, PartialEq, Eq)]
struct TableWork {
    captures: usize,
    collections: usize,
    projections: usize,
}

fn table_work(trace: &Trace, key: &Key) -> TableWork {
    TableWork {
        captures: minute_work(trace, key, Kind::ContentCapture),
        collections: minute_work(trace, key, Kind::NativeCollect),
        projections: minute_work(trace, key, Kind::ProjectionDecode),
    }
}

fn assert_reopened(trace: &Trace, key: &Key, before: &TableWork) {
    let after = table_work(trace, key);
    // Only process/events tables reopen; cached endpoint headers need no new tick projection.
    assert_eq!(after.captures, before.captures + 2);
    assert_eq!(after.collections, before.collections + 2);
    assert_eq!(after.projections, before.projections);
}

fn assert_older_resolved(run: &PendingStage, trace: &Trace) {
    run.assert_retained(&Retained {
        recent: &[6, 7],
        older: &[5],
    });
    assert_sampled_resident(run.sampled(5), &run.observation(5));
    assert_sampled_disk(run.sampled(5));
    assert!(matches!(
        target(&run.stage.held, &run.observation(3)),
        Target::Outside
    ));
    assert!(matches!(
        target(&run.stage.held, &run.observation(4)),
        Target::Outside
    ));
    assert!(run.stage.startup.pending.is_none());
    run.assert_progress(&ReplayPosition {
        minute: 1,
        next_endpoint: 2,
        previous: Some(4),
    });
    assert_eq!(
        publication_work(trace, &run.observation(3), Kind::AppDerivation),
        1
    );
    assert_eq!(
        publication_work(trace, &run.observation(4), Kind::AppDerivation),
        0
    );
    assert_eq!(
        publication_work(trace, &run.observation(4), Kind::CommittedFrameCapture),
        1
    );
}

fn assert_recent_aged_to_older(run: &PendingStage, trace: &Trace) {
    run.assert_native(Interval {
        selected: 7,
        previous: 6,
    });
    assert_sampled_resident(run.sampled(1), &run.observation(1));
    run.assert_pending_recent(2);
    run.assert_progress(&ReplayPosition {
        minute: 0,
        next_endpoint: 1,
        previous: Some(1),
    });
    run.assert_unread(&[2, 3, 4]);
    assert_eq!(
        publication_work(trace, &run.observation(1), Kind::AppDerivation),
        1
    );
}

fn assert_history_work(trace: &Trace, run: &PendingStage, publications: &[u64]) {
    for publication in publications {
        let observation = run.observation(*publication);
        assert_eq!(
            publication_work(trace, &observation, Kind::AppDerivation),
            1
        );
        assert_eq!(
            publication_work(trace, &observation, Kind::CommittedFrameCapture),
            1
        );
    }
}
