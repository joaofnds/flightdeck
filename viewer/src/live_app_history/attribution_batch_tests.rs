use std::sync::Arc;

use super::*;
use crate::Cancellation;
use crate::apps::Stamp;
use crate::live::identity_fixture::Fixture;
use crate::live_app_history::{App, CompactMeasure, compact};
use crate::live_app_response::{Key, KeyKind, Measure};

struct SavedFixture {
    fixture: Fixture,
    saved: Session,
    selected: Observation,
    cancellation: Arc<Cancellation>,
    operation: Operation,
    expected: [Value; 3],
    limited_bytes: usize,
}

#[test]
fn batch_keeps_distinct_eight_app_cohorts_other_and_missing_observations() {
    // Rotated leaders and a missing middle publication forbid sharing CPU's cohort or zeros.
    let history = distinct_history();
    let selected = observation(3);
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let expected = METRICS.map(|metric| {
        history
            .attribution_response(&selected, metric, None, &operation)
            .unwrap()
    });

    let batch = history
        .attribution_batch_response(&selected, None, &operation)
        .unwrap();

    assert_batch(&batch, &expected);
    let cpu = batch.cpu.as_ref().unwrap();
    let power = batch.power.as_ref().unwrap();
    let disk = batch.disk.as_ref().unwrap();
    assert_ne!(cpu["cohort"], power["cohort"]);
    assert_ne!(cpu["cohort"], disk["cohort"]);
    assert_ne!(power["cohort"], disk["cohort"]);
    assert_eq!(cpu["cohort"].as_array().unwrap().len(), 8);
    assert_eq!(cpu["points"][0]["other"]["app_count"], 4);
    assert_eq!(cpu["points"][1]["source_available"], false);
    assert_eq!(cpu["points"][1]["other"], Value::Null);
}

#[test]
fn batch_pressure_replays_the_uncached_suffix_twice_for_all_three_metrics() {
    // The same explicitly limited budget exercises the two physical streamed data passes.
    let fixture = SavedFixture::new();
    let history = History::new();
    PROBE.with(|probe| *probe.borrow_mut() = Probe::default());

    let batch = fixture.query(&history, fixture.limited_bytes).unwrap();

    assert_batch(&batch, &fixture.expected);
    let cpu = batch.cpu.as_ref().unwrap();
    let uncached = cpu["coverage"]["saved_observations_uncached"]
        .as_u64()
        .unwrap();
    assert!(uncached > 0);
    assert_eq!(cpu["coverage"]["saved_cache_limited"], true);
    PROBE.with(|probe| {
        assert_eq!(u64::from(probe.borrow().observed), uncached);
        assert_eq!(u64::from(probe.borrow().projected), uncached);
    });
    assert!(history.saved_residents.borrow().bytes() <= fixture.limited_bytes);
    let warm = fixture.query(&history, fixture.limited_bytes).unwrap();
    assert_batch(&warm, &fixture.expected);
}

#[test]
fn batch_zero_cache_budget_keeps_context_and_aggregate_response_bound() {
    // No saved allocation remains available; every uncached endpoint must still be represented.
    let fixture = SavedFixture::new();
    let history = History::new();

    let batch = fixture.query(&history, 0).unwrap();

    assert_batch(&batch, &fixture.expected);
    let cpu = batch.cpu.as_ref().unwrap();
    assert_eq!(cpu["coverage"]["saved_observations_cached"], 0);
    assert_eq!(cpu["coverage"]["saved_observations_uncached"], 6);
    let encoded: usize = [&batch.cpu, &batch.power, &batch.disk]
        .into_iter()
        .map(|value| serde_json::to_vec(value.as_ref().unwrap()).unwrap().len())
        .sum();
    assert!(encoded <= crate::APP_ATTRIBUTION_HISTORIES_RESPONSE_BYTES_MAX);
    assert!(history.total_bytes() <= BYTES_MAX);
}

#[test]
fn batch_cancellation_restores_warm_facts_and_refuses_every_metric() {
    // Cancellation after choosing leaders cannot lose the taken cache or return a partial batch.
    let fixture = SavedFixture::new();
    let history = History::new();
    fixture.query(&history, BYTES_MAX).unwrap();
    let before = history.total_bytes();
    let cancellation = Arc::clone(&fixture.cancellation);
    PROBE.with(|probe| {
        probe.borrow_mut().before_project = Some(Box::new(move || cancellation.cancel().unwrap()));
    });

    let result = fixture.query(&history, BYTES_MAX);

    assert!(matches!(result, Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(history.total_bytes(), before);
    let operation = Arc::clone(&fixture.cancellation)
        .operation(fixture.cancellation.epoch())
        .unwrap();
    let retry = history
        .attribution_batch_saved(
            &SavedQuery {
                selected: &fixture.selected,
                saved: &fixture.saved,
                operation: &operation,
                available: BYTES_MAX.saturating_sub(history.bytes),
            },
            &mut Batch::new(&history, &fixture.selected, &operation).unwrap(),
        )
        .unwrap();
    assert_batch(&retry, &fixture.expected);
}

#[test]
fn batch_final_validation_rejects_replaced_cached_files_and_restores_ownership() {
    // Replacing an admitted file between phases must fail even when no raw replay is needed.
    let fixture = SavedFixture::new();
    let history = History::new();
    fixture.query(&history, BYTES_MAX).unwrap();
    let mut resident = history.saved_residents.borrow_mut();
    let rates = resident.take_attribution();
    let path = fixture
        .fixture
        .root
        .join(&rates.pins()[0].manifest.data_directory)
        .join("events.parquet");
    let retained = rates.count();
    resident.install_attribution(rates);
    drop(resident);
    let original = fixture.fixture.root.join("batch-events-original.parquet");
    PROBE.with(|probe| {
        probe.borrow_mut().before_project = Some(Box::new(move || {
            std::fs::rename(&path, &original).unwrap();
            std::fs::copy(&original, &path).unwrap();
        }));
    });

    let result = fixture.query(&history, BYTES_MAX);

    assert!(matches!(
        result,
        Err(Error::Bound("attribution cache file changed"))
    ));
    let mut resident = history.saved_residents.borrow_mut();
    let rates = resident.take_attribution();
    assert_eq!(rates.count(), retained);
    resident.install_attribution(rates);
    assert!(history.bytes + resident.bytes() <= BYTES_MAX);
}

#[test]
fn batch_cpu_failure_preserves_power_and_disk() {
    // A local reduction refusal must leave both sibling payloads byte-identical.
    assert_metric_failure(0);
}

#[test]
fn batch_power_failure_preserves_cpu_and_disk() {
    // Each position in the fixed coordinator needs its own independently reporting case.
    assert_metric_failure(1);
}

#[test]
fn batch_disk_failure_preserves_cpu_and_power() {
    // The final result slot must retain its own failure rather than borrowing a sibling.
    assert_metric_failure(2);
}

fn assert_metric_failure(failed: usize) {
    let history = distinct_history();
    let selected = observation(3);
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let expected = METRICS.map(|metric| {
        history
            .attribution_response(&selected, metric, None, &operation)
            .unwrap()
    });
    let mut batch = Batch::new(&history, &selected, &operation).unwrap();
    batch.states[failed].apply(|_reduction| Err(Error::Bound("batch metric test failure")));
    batch.choose(&operation).unwrap();
    batch.project_live(&history, &operation).unwrap();

    let result = batch.finish(&history, None, &operation).unwrap();

    for (index, value) in [&result.cpu, &result.power, &result.disk]
        .into_iter()
        .enumerate()
    {
        if index == failed {
            assert!(matches!(
                value,
                Err(Error::Bound("batch metric test failure"))
            ));
        } else {
            assert_payload(value.as_ref().unwrap(), &expected[index]);
        }
    }
}

#[test]
fn batch_keeps_a_saved_predecessor_outside_the_context_across_minutes() {
    // The visible endpoint needs its real predecessor in the preceding committed minute.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1900);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    retire(&fixture);
    let mut session = crate::LiveSession::open(&fixture.root).unwrap();
    session.select(1900, 0).unwrap();
    let expected = METRICS.map(|metric| session.app_attribution_history(metric).unwrap());

    let batch = session.app_attribution_histories().unwrap();

    assert_batch(&batch, &expected);
    assert_eq!(
        batch.cpu.as_ref().unwrap()["points"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        batch.cpu.as_ref().unwrap()["points"][0]["elapsed_ns"],
        "1000000000"
    );
}

#[test]
fn batch_rejects_more_than_the_observation_bound_in_each_metric() {
    // A full ledger is accepted; the next observation fails instead of growing a result.
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let mut selected = observation(4097);
    selected.wall_ns = Some(1_000_000_000_000);
    let mut states = METRICS.map(|metric| {
        MetricState::Ready(Box::new(
            Reduction::new(&selected, metric, &operation).unwrap(),
        ))
    });
    for publication in 1..=4097 {
        let mut selected = observation(publication);
        selected.wall_ns = Some(1_000_000_000_000);
        let capture = crate::app_attribution_history::Capture {
            coverage: crate::live::resident_history::Capture {
                observation: selected,
                membership_complete: true,
                ungrouped_members: 0,
                usage_refused: 0,
                usage_gone: 0,
            },
            elapsed_ns: Some(1_000_000_000),
        };
        for state in &mut states {
            state.apply(|reduction| reduction.observe(&capture, std::iter::empty(), Source::Saved));
        }
    }
    for state in states {
        assert!(matches!(
            state,
            MetricState::Failed(Error::Bound("attribution observations"))
        ));
    }
}

#[test]
fn batch_accepts_the_app_population_bound_and_rejects_one_more_key() {
    // Increasing exact byte keys fill the shared reductions without repeated-key shortcuts.
    let keys: Vec<_> = (0_u64..24_000)
        .map(|index| index.to_be_bytes().to_vec())
        .collect();

    assert_context_boundary(&keys, &24_000_u64.to_be_bytes(), "attribution context Apps");
}

#[test]
fn batch_accepts_the_key_arena_bound_and_rejects_one_more_byte() {
    // Legal individual keys fill exactly eight MiB before a distinct one-byte key is refused.
    let mut keys: Vec<_> = (0_u64..2048)
        .map(|index| {
            let mut key = vec![0; collector::ring::PROGRAM_PATH_BYTES_MAX];
            key[..8].copy_from_slice(&index.to_be_bytes());
            key
        })
        .collect();
    keys.push(vec![254; 2048]);
    assert_eq!(keys.iter().map(Vec::len).sum::<usize>(), 8 * 1024 * 1024);

    assert_context_boundary(&keys, &[255], "attribution context key bytes");
}

fn assert_context_boundary(keys: &[Vec<u8>], extra: &[u8], error: &'static str) {
    let history = History::new();
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let mut batch = Batch::new(&history, &observation(3), &operation).unwrap();
    let mut capture = crate::app_attribution_history::Capture {
        coverage: crate::live::resident_history::Capture {
            observation: observation(1),
            membership_complete: true,
            ungrouped_members: 0,
            usage_refused: 0,
            usage_gone: 0,
        },
        elapsed_ns: Some(1_000_000_000),
    };
    for state in &mut batch.states {
        state.apply(|reduction| {
            reduction.observe(
                &capture,
                keys.iter().map(|key| boundary_reading(key)),
                Source::Saved,
            )
        });
        assert!(matches!(state, MetricState::Ready(_)));
    }
    capture.coverage.observation = observation(2);

    for state in &mut batch.states {
        state.apply(|reduction| {
            reduction.observe(
                &capture,
                std::iter::once(boundary_reading(extra)),
                Source::Saved,
            )
        });
    }
    let result = batch.finish(&history, None, &operation).unwrap();

    for value in [&result.cpu, &result.power, &result.disk] {
        assert!(matches!(value, Err(Error::Bound(actual)) if *actual == error));
    }
}

fn boundary_reading(key: &[u8]) -> crate::app_attribution_history::Reading<'_> {
    crate::app_attribution_history::Reading {
        key: crate::apps::AppKey::Bundle(key),
        measure: amount(1).measure(),
        members: 1,
        membership_complete: true,
    }
}

impl SavedFixture {
    fn new() -> Box<Self> {
        let mut fixture = Fixture::new();
        fixture.start_at_run_tick(59);
        for minute in 0..3 {
            fixture.publish_with_system_rows(1000 + minute * 2);
            fixture.publish_with_system_rows(1001 + minute * 2);
            if minute < 2 {
                fixture.skip_run_ticks(58);
            }
        }
        for minute in 0..4 {
            fixture.seal_minute(minute);
        }
        let saved = Session::open(&fixture.root).unwrap();
        let run = saved.windows[0].identity.run;
        let selected = Observation {
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
        };
        let cancellation = Arc::new(Cancellation::default());
        let operation = Arc::clone(&cancellation).operation(0).unwrap();
        let control = History::new();
        let expected = METRICS.map(|metric| {
            control
                .attribution_response(&selected, metric, Some(&saved), &operation)
                .unwrap()
        });
        let rates = control.saved_residents.borrow_mut().take_attribution();
        let limited_bytes = rates.bytes() - rates.pins().last().unwrap().bytes();

        Box::new(Self {
            fixture,
            saved,
            selected,
            cancellation,
            operation,
            expected,
            limited_bytes,
        })
    }

    fn query(&self, history: &History, available: usize) -> Result<Box<AppAttributionHistories>> {
        let mut batch = Batch::new(history, &self.selected, &self.operation)?;
        let available = available.min(BYTES_MAX.saturating_sub(history.bytes));
        history.attribution_batch_saved(
            &SavedQuery {
                selected: &self.selected,
                saved: &self.saved,
                operation: &self.operation,
                available,
            },
            &mut batch,
        )
    }
}

fn distinct_history() -> Box<History> {
    let mut history = Box::new(History::new());
    for publication in [1, 3] {
        let mut point = Box::new(Point {
            observation: observation(publication),
            segment: 0,
            elapsed_ns: Some(1_000_000_000),
            apps: Vec::with_capacity(12),
            groups: None,
            membership_complete: true,
            resident_ungrouped_members: 0,
            ungrouped_members: 0,
            usage_refused: 0,
            usage_gone: 0,
        });
        for index in 0..12 {
            let bytes = format!("/Apps/{index:02}.app").into_bytes();
            let key = Key {
                kind: KeyKind::Bundle,
                display: "App".into(),
                bytes,
            };
            let mut previous = 0;
            let zero = amount(0);
            point.apps.push(App {
                key: compact::capture_key(&key, &[], &mut previous),
                members: 1,
                membership_complete: true,
                cpu_ns: amount(100 - index),
                energy_nj: amount(100 - (index + 4) % 12),
                disk_io_bytes: amount(100 - (index + 8) % 12),
                disk_read_bytes: zero,
                disk_written_bytes: zero,
                resident_bytes: zero,
                footprint_bytes: zero,
            });
        }
        history.push(point, 0);
    }
    history.observations.insert(1, observation(2));

    history
}

fn amount(known: u64) -> CompactMeasure {
    CompactMeasure::capture(&Measure {
        known,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    })
}

fn observation(publication: u64) -> Observation {
    Observation {
        recording: [1, 2],
        stamp: Stamp {
            boot: [3, 4],
            run: [5, 6],
            publication,
            continuous_ns: Some(publication * 1_000_000_000),
            awake_ns: Some(publication * 1_000_000_000),
        },
        run_tick: publication,
        wall_ns: Some((1000 + publication) * 1_000_000_000),
    }
}

fn assert_batch(batch: &AppAttributionHistories, expected: &[Value; 3]) {
    for (actual, expected) in [&batch.cpu, &batch.power, &batch.disk]
        .into_iter()
        .zip(expected)
    {
        assert_payload(actual.as_ref().unwrap(), expected);
    }
}

fn assert_payload(actual: &Value, expected: &Value) {
    let mut actual = actual.clone();
    let mut expected = expected.clone();
    for payload in [&mut actual, &mut expected] {
        for field in [
            "cache_bytes",
            "saved_cache_limited",
            "saved_observations_cached",
            "saved_observations_uncached",
        ] {
            drop(payload["coverage"].as_object_mut().unwrap().remove(field));
        }
    }
    assert_eq!(
        serde_json::to_vec(&actual).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
}

fn retire(fixture: &Fixture) {
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
}
