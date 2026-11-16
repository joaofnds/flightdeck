//! The launch as the store's own turn steps it, with native ticks arriving between its steps.
use super::*;
use crate::live::identity_fixture::Fixture;
use crate::store::startup_tests::usage;
use crate::store::{Buffers, MachineSecond, SampledSecond, Span, StartupState};
use persistence::work::ingestion::{Event, Key, Kind, Purpose, Trace};

#[test]
fn recent_candidates_install_past_a_retired_reader_without_rederivation() {
    // Goal: a reader holding a recent second's buffer stops no install, even with every App
    // buffer held, since a recent install takes a held buffer's slot, and native progress after
    // the installs derives no history twice.
    // Method: pin a retired recent second, hold every other App buffer, install the history,
    // append two real ticks, then finish, compare the span's seconds with the retired one and
    // count each publication's derivations.
    let mut run = TurnStage::new(recent_fixture(), 6, RECENT_TICKS);
    let budget = Arc::clone(&run.budget);
    let trace = Trace::new(run.fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    run.reach_phase(Phase::SelectedBoundary);
    let retired = Arc::clone(run.stage.held.apps.newest_held().unwrap());
    assert_eq!(retired.observation, Some(run.observation(6)));
    assert_eq!(retired.missing, Some(Missing::Unread));
    run.reach_primary();
    let others = run.stage.held.apps.hold_every_free_buffer();
    assert_eq!(run.stage.held.apps.free_count(), 0);
    let count = run.stage.held.apps.buffer_count();
    run.reach_phase(Phase::History);
    assert!(!run.turn());
    assert!(run.blocked.is_none());

    run.fixture.publish_with_usage(1064, &usage(122));
    assert!(!run.turn());
    run.fixture.publish_with_usage(1065, &usage(123));
    assert!(!run.turn());
    run.complete();

    assert_eq!(run.stage.held.apps.buffer_count(), count);
    let installed = run.recent(6);
    assert!(!std::ptr::eq(installed, Arc::as_ptr(&retired)));
    assert_eq!(installed.missing, None);
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
    drop((retired, others, run, scope));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_queued_primary_query_rejects_a_changed_witness_before_serving_a_reply() {
    // Goal: a queued query cannot bypass witness validation while history is capacity-blocked.
    // Method: over seconds more than fifteen minutes older than the recent ones so the query
    // reaches none that left the span, hold every byte the budget has left once history begins,
    // receive a real Primary control, change the seal history reads, then query the same caller.
    let mut run = TurnStage::new(far_older_fixture(), 4, FAR_OLDER_TICKS);
    let budget = Arc::clone(&run.budget);
    let trace = Trace::new(run.fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    run.reach_primary();
    let handle = run.shared.queries.handle(std::thread::current());
    let selected = run.observation(7);

    let retained = run.primary_reply(&handle);

    assert_primary_query(&retained, &selected);
    run.reach_phase(Phase::History);
    let lease = budget.reserve(budget.maximum() - budget.used()).unwrap();
    assert!(!run.turn());
    assert!(run.blocked.is_some());
    let accepted = run.stage.accepted();
    let history_index = run.stage.startup.history_index;
    let root = run.fixture.root.clone();
    let marker = &run.stage.startup.minutes[history_index].found.marker;
    crate::store::tests::change_saved_manifest(&root, marker);
    let token = run.submit_primary_query(&handle);
    assert_eq!(
        token,
        crate::store::QueryToken {
            epoch: 0,
            sequence: 2
        }
    );

    let result = run.try_turn();
    drop(lease);

    assert!(
        matches!(result, Err(Failure::Read(Refused::Commitment(error)))
        if error.kind() == std::io::ErrorKind::InvalidData)
    );
    assert!(handle.take().is_none());
    assert!(handle.pending());
    assert!(run.shared.queries.requested());
    assert!(run.shared.mailbox.take().is_none());
    assert_eq!(run.stage.accepted(), accepted);
    assert_eq!(run.stage.startup.history_index, history_index);
    assert_eq!(run.stage.startup.minutes[history_index].next_endpoint, 0);
    assert_eq!(run.stage.held.newest.unwrap().publication, 7);
    assert_eq!(
        publication_work(&trace, &run.observation(4), Kind::AppDerivation),
        0
    );
    assert_primary_query(&retained, &selected);
    handle.cancel().unwrap();
    assert!(!handle.pending());
    assert!(!run.shared.queries.requested());
    drop((handle, run, scope, trace));
    assert!(!root.try_exists().unwrap());
    assert!(budget.used() > 0);
    assert_primary_query(&retained, &selected);
    drop(retained);
    assert_eq!(budget.used(), 0);
}

struct TurnStage {
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
/// The publication, run tick and wall second of each tick of `far_older_fixture`.
const FAR_OLDER_TICKS: &[[u64; 3]] = &[
    [1, 0, 1000],
    [2, 1, 1010],
    [3, 60, 1020],
    [4, 61, 2000],
    [5, 62, 2001],
    [6, 120, 2002],
    [7, 121, 2003],
];
impl TurnStage {
    fn new(fixture: Fixture, recent_capacity: usize, ticks: &'static [[u64; 3]]) -> Self {
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let mut stage = GuardStage::new(&fixture, &budget);
        stage.held.span = Span::new(
            recent_capacity,
            Buffers::new(recent_capacity + 1, MachineSecond::default),
        );
        stage.held.apps = crate::store::block::BlockSpan::with_fewest_buffers(recent_capacity);
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
        assert!(self.shared.queries.requested());
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
        assert!(!self.shared.queries.requested());
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
            .held()
            .find(|sample| sample.observation == self.observation(publication))
            .unwrap()
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
        let process = payload
            .processes()
            .find(|process| process.pid() == 7)
            .unwrap();
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
        let sampled = &self.stage.held.sampled;
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
    assert_eq!(context.context_last_second(), 2003);
    assert_eq!(context.seconds().len(), 4);
    assert!(
        context
            .seconds()
            .iter()
            .all(crate::store::QuerySecond::fixed)
    );
    assert_eq!(context.selected().machine().cpu_percent, Some(50.0));
    let selection = context
        .selection(&Arc::new(crate::Cancellation::default()), 0)
        .unwrap();
    assert_eq!(selection["selected"]["publication"], 7);
    assert_eq!(selection["selected"]["run_tick"], 121);
    assert_eq!(selection["selected"]["wall_ns"], 2_003_000_000_000_u64);
    assert_eq!(selection["summary"]["cpu_percent"], 50.0);
}

/// Three seconds in three stretches of ten wall seconds, then four more than fifteen minutes
/// later, sealed in three minutes so the live span of four reaches into the second minute.
fn far_older_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1010, &usage(1));
    fixture.seal_minute(0);
    fixture.skip_run_ticks(58);
    fixture.publish_with_usage(1020, &usage(60));
    fixture.publish_with_usage(2000, &usage(61));
    fixture.publish_with_usage(2001, &usage(62));
    fixture.seal_minute(1);
    fixture.skip_run_ticks(57);
    fixture.publish_with_usage(2002, &usage(120));
    fixture.publish_with_usage(2003, &usage(121));
    fixture.seal_minute(2);
    fixture
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
        queries: crate::store::query::Queries::new(),
        stop: AtomicBool::new(false),
        active: AtomicBool::new(true),
        catch_up: AtomicBool::new(false),
        catching_up: AtomicBool::new(true),
        paused: AtomicBool::new(false),
        cancellation: Arc::new(crate::Cancellation::default()),
    }
}

fn assert_recent_resident(apps: &AppsSecond, observation: &Observation) {
    assert_eq!(apps.observation, Some(*observation));
    assert_eq!(apps.missing, None);
    let payload = apps.processes.as_ref().unwrap();
    assert_eq!(payload.selected, *observation);
    let process = payload
        .processes()
        .find(|process| process.pid() == 7)
        .unwrap();
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

fn assert_history_work(trace: &Trace, run: &TurnStage, publications: &[u64]) {
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
