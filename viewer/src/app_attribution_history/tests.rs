use collector::processes::Usage;

use crate::live::identity_fixture::Fixture;
use crate::{AppAttributionMetric, LiveSession};

#[path = "tests/pressure_fixture.rs"]
mod pressure_fixture;

#[test]
fn attribution_shares_repeated_long_keys_and_compact_rates_under_real_cache_pressure() {
    // The old per-observation key copies alone exceeded 64 MiB across this real sealed context.
    let fixture = pressure_fixture::Recording::new();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1127, 0).unwrap();

    let cpu = session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    let reads = session.history.as_ref().unwrap().source_reads.get();
    let power = session
        .app_attribution_history(AppAttributionMetric::Power)
        .unwrap();

    assert_eq!(cpu["points"].as_array().unwrap().len(), 128);
    assert_eq!(power["points"].as_array().unwrap().len(), 128);
    assert_eq!(cpu["points"][127]["apps"][0]["cpu_percent_one_core"], 0.03);
    assert_eq!(power["points"][127]["apps"][0]["energy_watts"], 0.0006);
    assert_eq!(cpu["coverage"]["saved_observations_cached"], 128);
    assert_eq!(cpu["coverage"]["saved_cache_limited"], false);
    assert!(cpu["coverage"]["cache_bytes"].as_u64().unwrap() < 8 * 1024 * 1024);
    assert_eq!(
        session.history.as_ref().unwrap().source_reads.get() - reads,
        3
    );
}

#[test]
fn attribution_preserves_cpu_above_one_core_and_actual_interval_power() {
    // Compare captured App attribution with the existing public per-App trace.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1_200_000_000));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let cpu = session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    let power = session
        .app_attribution_history(AppAttributionMetric::Power)
        .unwrap();

    assert_eq!(cpu["metric"], "cpu");
    assert_eq!(cpu["points"][1]["elapsed_ns"], "1000000000");
    assert_eq!(cpu["points"][1]["apps"][0]["cpu_percent_one_core"], 120.0);
    assert_eq!(cpu["points"][1]["apps"][0]["cpu_ns"]["known"], "1200000000");
    assert_eq!(power["metric"], "power");
    assert_eq!(power["points"][1]["apps"][0]["energy_watts"], 2.4);
    assert_eq!(
        cpu["points"][0]["apps"][0]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
}

fn usage(cpu_ns: u64) -> Usage {
    Usage {
        awake_ns_at_start: 0,
        user_ns: cpu_ns,
        system_ns: 0,
        energy_nj: cpu_ns * 2,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes: 11,
        footprint_bytes: 0,
        disk_read_bytes: 0,
        disk_written_bytes: 0,
    }
}

#[test]
fn attribution_restores_both_metrics_and_reuses_saved_intervals() {
    // Retire real rings, then compare all rate points and the metadata-only repeat budget.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1_200_000_000));
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.follow_latest().unwrap();
    let cpu = live
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    let power = live
        .app_attribution_history(AppAttributionMetric::Power)
        .unwrap();
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut saved = LiveSession::open(&fixture.root).unwrap();
    saved.select(1001, 0).unwrap();

    let restored = saved
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    let reads = saved.history.as_ref().unwrap().source_reads.get();
    let restored_power = saved
        .app_attribution_history(AppAttributionMetric::Power)
        .unwrap();

    assert_eq!(restored["points"], cpu["points"]);
    assert_eq!(restored["cohort"], cpu["cohort"]);
    assert_eq!(restored_power["points"], power["points"]);
    assert_eq!(
        saved.history.as_ref().unwrap().source_reads.get() - reads,
        1
    );
    assert!(restored_power["coverage"]["cache_bytes"].as_u64().unwrap() <= 67_108_864);
}

#[test]
fn attribution_cache_keeps_resident_gauges_identical_in_both_route_orders() {
    // Warm each saved cache in both orders and compare the resident public gauge payload.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1_200_000_000));
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let before = session.app_resident_history().unwrap();

    let cpu = session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    let after = session.app_resident_history().unwrap();

    assert_eq!(after["points"], before["points"]);
    assert_eq!(after["cohort"], before["cohort"]);
    assert_eq!(cpu["points"][1]["apps"][0]["cpu_percent_one_core"], 120.0);
    assert!(after["coverage"]["cache_bytes"].as_u64().unwrap() <= 67_108_864);
}

#[test]
fn attribution_preserves_same_second_observations_and_breaks_their_segment() {
    // Two actual publications in one wall second retain separate identities and no joined line.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1000, &usage(1_200_000_000));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1000, 0).unwrap();

    let cpu = session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();

    assert_eq!(cpu["points"].as_array().unwrap().len(), 2);
    assert_eq!(cpu["points"][0]["observation"]["stamp"]["publication"], "1");
    assert_eq!(cpu["points"][1]["observation"]["stamp"]["publication"], "2");
    assert_eq!(cpu["points"][0]["segment"], "0");
    assert_eq!(cpu["points"][1]["segment"], "1");
}

#[test]
fn attribution_rejects_replaced_saved_files_on_a_cache_hit() {
    // Replacing a committed file with identical bytes cannot preserve its captured identity.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1_200_000_000));
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    let path = fixture
        .root
        .join(&session.history.as_ref().unwrap().windows[0].data_directory)
        .join("events.parquet");
    std::fs::rename(&path, fixture.root.join("events-original.parquet")).unwrap();
    std::fs::copy(fixture.root.join("events-original.parquet"), &path).unwrap();

    let result = session.app_attribution_history(AppAttributionMetric::Power);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound("attribution cache file changed"))
    ));
}

#[test]
fn attribution_restores_a_predecessor_outside_the_visible_context_across_minutes() {
    // The first visible endpoint needs a real predecessor from the preceding sealed minute.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1900, &usage(1_200_000_000));
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1900, 0).unwrap();

    let cpu = session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();

    assert_eq!(cpu["points"].as_array().unwrap().len(), 1);
    assert_eq!(cpu["points"][0]["apps"][0]["cpu_percent_one_core"], 120.0);
    assert_eq!(cpu["points"][0]["elapsed_ns"], "1000000000");
}

#[test]
fn attribution_uses_continuous_elapsed_time_and_withholds_refused_members() {
    // A two-second captured interval halves both rates; the refused App stays unmeasured.
    let mut fixture = Fixture::new();
    fixture.set_clock_intervals(2_000_000_000, 1_000_000_000);
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1_200_000_000));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();

    let cpu = session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    let power = session
        .app_attribution_history(AppAttributionMetric::Power)
        .unwrap();

    assert_eq!(cpu["points"][1]["elapsed_ns"], "2000000000");
    assert_eq!(cpu["points"][1]["apps"][0]["cpu_percent_one_core"], 60.0);
    assert_eq!(power["points"][1]["apps"][0]["energy_watts"], 1.2);
    assert_eq!(
        cpu["points"][1]["other"]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(
        cpu["points"][1]["other"]["cpu_ns"]["unavailable_members"],
        1
    );
}

#[test]
fn attribution_refuses_a_superseded_operation_even_with_warm_saved_facts() {
    // A warm interval cache cannot authorize work after the owning request is canceled.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1_200_000_000));
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    session.cancellation.cancel().unwrap();

    let result = session.app_attribution_history(AppAttributionMetric::Power);

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
}

#[test]
fn attribution_counts_saved_live_overlap_once_and_prefers_the_live_observation() {
    // Sealing observations already held live must not duplicate their work or alter their cohort.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1_200_000_000));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();
    let before = session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    fixture.seal();
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let after = session
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();

    assert_eq!(after["points"], before["points"]);
    assert_eq!(after["cohort"], before["cohort"]);
    assert_eq!(after["coverage"]["committed_observations"], 2);
}

#[test]
fn attribution_keeps_verified_exit_deltas_after_cold_reopen_without_resident_gauges() {
    // Actual terminal usage remains CPU/power work while its resident gauge ceases to exist.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(0));
    fixture.publish_with_usage(1001, &usage(1_000_000_000));
    publish_exits(&mut fixture);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    live.follow_latest().unwrap();
    let before = live
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();
    let resident = live.app_resident_history().unwrap();
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut saved = LiveSession::open(&fixture.root).unwrap();
    saved.select(1002, 0).unwrap();

    let after = saved
        .app_attribution_history(AppAttributionMetric::Cpu)
        .unwrap();

    assert_eq!(
        before["points"][2]["apps"][0]["cpu_percent_one_core"],
        200.0
    );
    assert_eq!(after["points"], before["points"]);
    assert_eq!(resident["points"][2]["apps"][0], serde_json::Value::Null);
}

fn publish_exits(fixture: &mut Fixture) {
    use collector::ring::{
        IdentityObservation, ProcessCreation, ProcessIdentity, UsageAvailability,
    };
    let events = [1, 7].map(|pid| collector::tracker::Event::Exit {
        pid,
        last: Ok(usage(3_000_000_000)),
        noticed: collector::tracker::Noticed::OnExit,
    });
    let identities = [1, 7].map(|pid| IdentityObservation {
        pid,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Unavailable {
            creation: Some(ProcessCreation {
                unique_id: u64::try_from(pid).unwrap(),
                pid_version: -pid,
            }),
            reason: collector::ring::IdentityUnavailable::Unsupported,
        },
    });
    fixture
        .writer
        .publish_with_identity(
            &collector::ring::Readings {
                run_tick: 2,
                cpus: &[collector::machine::CPUTicks {
                    user: 2,
                    system: 0,
                    nice: 0,
                    idle: 2,
                }],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &events,
                dropped: collector::ring::Dropped::default(),
            },
            Some(&collector::ring::ObservationTimes {
                wall_ns: 1_002_000_000_000,
                continuous_ns: 2_000_000_000,
                awake_ns: 2_000_000_000,
            }),
            &collector::ring::IdentityPublication {
                processes: &[],
                events: &identities,
            },
        )
        .unwrap();
}
