use std::sync::Arc;

use super::*;
use crate::Cancellation;
use crate::apps::Stamp;
use crate::live::identity_fixture::Fixture;
use persistence::Error;

struct Pressure {
    history: History,
    saved: Session,
    selected: Observation,
    operation: Operation,
    cancellation: Arc<Cancellation>,
    expected: [Value; 2],
    available: usize,
    cold_reads: u64,
    fixture: Fixture,
}

#[test]
fn attribution_saturated_cache_retains_pinned_facts_and_reuses_them_across_metrics() {
    // A real sealed context receives a smaller available cache budget, as a busy live tail leaves.
    let pressure = Pressure::new();

    let actual = pressure.query(AppAttributionMetric::Cpu).unwrap();

    assert_eq!(actual["points"], pressure.expected[0]["points"]);
    assert_eq!(actual["cohort"], pressure.expected[0]["cohort"]);
    let retained = pressure
        .history
        .saved_residents
        .borrow_mut()
        .take_attribution();
    assert!(
        retained.count() > 0,
        "cache pressure discarded every saved fact"
    );
    assert!(retained.count() < 6);
    assert!(retained.bytes() <= pressure.available);
    pressure
        .saved
        .validate_attribution_cache(&retained)
        .unwrap();
    assert!(
        actual["coverage"]["saved_observations_cached"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(actual["coverage"]["saved_cache_limited"], true);
}

#[test]
fn attribution_saturated_cache_replays_only_uncached_suffix_on_warm_metric_switches() {
    // Cache pressure qualifies the suffix work; retained windows avoid full-context restoration.
    let pressure = Pressure::new();
    let first = pressure.query(AppAttributionMetric::Cpu).unwrap();
    let before = pressure.saved.source_reads.get();

    let power = pressure.query(AppAttributionMetric::Power).unwrap();
    let reads = pressure.saved.source_reads.get() - before;
    let repeated = pressure.query(AppAttributionMetric::Cpu).unwrap();

    assert_eq!(power["points"], pressure.expected[1]["points"]);
    assert_eq!(power["cohort"], pressure.expected[1]["cohort"]);
    assert_eq!(repeated, first);
    assert_eq!(
        power["coverage"]["saved_observations_cached"],
        first["coverage"]["saved_observations_cached"]
    );
    assert!(
        reads > 4,
        "the explicitly uncached suffix still requires saved work"
    );
    assert!(
        reads < pressure.cold_reads * 3,
        "the full context was restored three times"
    );
}

#[test]
fn attribution_saturated_cache_rejects_replaced_pinned_files_on_a_warm_query() {
    // A partial cache retains the same file identity obligations as a complete cache.
    let pressure = Pressure::new();
    pressure.query(AppAttributionMetric::Cpu).unwrap();
    let cache = pressure
        .history
        .saved_residents
        .borrow_mut()
        .take_attribution();
    let path = pressure
        .fixture
        .root
        .join(&cache.pins()[0].manifest.data_directory)
        .join("events.parquet");
    pressure
        .history
        .saved_residents
        .borrow_mut()
        .install_attribution(cache);
    let original = pressure.fixture.root.join("events-original.parquet");
    std::fs::rename(&path, &original).unwrap();
    std::fs::copy(original, &path).unwrap();

    let result = pressure.query(AppAttributionMetric::Power);

    assert!(matches!(
        result,
        Err(Error::Bound("attribution cache file changed"))
    ));
}

#[test]
fn attribution_saturated_cache_refuses_a_canceled_warm_query() {
    // Superseding a partially cached request cannot authorize retained or streamed output.
    let pressure = Pressure::new();
    pressure.query(AppAttributionMetric::Cpu).unwrap();
    pressure.cancellation.cancel().unwrap();

    let result = pressure.query(AppAttributionMetric::Power);

    assert!(matches!(result, Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
}

#[test]
fn attribution_reports_zero_remaining_cache_space_without_underflow_or_losing_context() {
    // A fully occupied live-tail budget leaves no saved-cache allocation and keeps output exact.
    let mut pressure = Pressure::new();
    pressure.history.bytes = BYTES_MAX;

    let actual = pressure
        .history
        .attribution_response(
            &Selected::ending_at(pressure.selected).unwrap(),
            AppAttributionMetric::Cpu,
            Some(&pressure.saved),
            &pressure.operation,
        )
        .unwrap();

    assert_eq!(actual["points"], pressure.expected[0]["points"]);
    assert_eq!(actual["cohort"], pressure.expected[0]["cohort"]);
    assert_eq!(actual["coverage"]["saved_observations_cached"], 0);
    assert_eq!(actual["coverage"]["saved_observations_uncached"], 6);
    assert_eq!(actual["coverage"]["saved_cache_limited"], true);
}

impl Pressure {
    fn new() -> Self {
        let fixture = context();
        let saved = Session::open(&fixture.root).unwrap();
        let selected = observation(&saved);
        let cancellation = Arc::new(Cancellation::default());
        let operation = Arc::clone(&cancellation).operation(0).unwrap();
        let full = History::new();
        let before = saved.source_reads.get();
        let cpu = full
            .attribution_response(
                &Selected::ending_at(selected).unwrap(),
                AppAttributionMetric::Cpu,
                Some(&saved),
                &operation,
            )
            .unwrap();
        let cold_reads = saved.source_reads.get() - before;
        let power = full
            .attribution_response(
                &Selected::ending_at(selected).unwrap(),
                AppAttributionMetric::Power,
                Some(&saved),
                &operation,
            )
            .unwrap();
        let cache = full.saved_residents.borrow_mut().take_attribution();
        let available = cache.bytes() - cache.pins().last().unwrap().bytes();
        Self {
            history: History::new(),
            saved,
            selected,
            operation,
            cancellation,
            expected: [cpu, power],
            available,
            cold_reads,
            fixture,
        }
    }

    fn query(&self, metric: AppAttributionMetric) -> Result<Value> {
        self.query_budget(metric, self.available)
    }

    fn query_budget(&self, metric: AppAttributionMetric, available: usize) -> Result<Value> {
        self.history.attribution_saved(
            &SavedQuery {
                selected: &Selected::ending_at(self.selected).unwrap(),
                metric,
                saved: &self.saved,
                operation: &self.operation,
            },
            Reduction::new(
                &Selected::ending_at(self.selected).unwrap(),
                metric,
                &self.operation,
            ),
            available,
        )
    }
}

fn context() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1002);
    fixture.publish_with_system_rows(1003);
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1004);
    fixture.publish_with_system_rows(1005);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    fixture.seal_minute(2);
    fixture.seal_minute(3);
    fixture
}

fn observation(saved: &Session) -> Observation {
    let run = saved.windows[0].identity.run;
    Observation {
        recording: run.recording.0,
        stamp: Stamp {
            boot: run.boot.0,
            run: run.run.0,
            publication: 6,
            continuous_ns: Some(180_000_000_000),
            awake_ns: Some(180_000_000_000),
        },
        run_tick: 180,
        wall_ns: Some(1_005_000_000_000),
    }
}
