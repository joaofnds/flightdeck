use super::{Fixture, LiveSession, usage};
use persistence::Error;

#[test]
fn resident_history_rejects_replaced_ticks_on_a_cache_hit() {
    // Goal: retained coverage remains pinned to the ticks file that supplied its endpoint metadata.
    // Method: replace ticks with identical bytes/new inode after warming real saved resident facts.
    let (fixture, session) = saved_context();
    session.app_resident_history().unwrap();
    let saved = session.history.as_ref().unwrap();
    let path = fixture
        .root
        .join(&saved.windows[0].data_directory)
        .join("ticks.parquet");
    let original = fixture.root.join("ticks-original.parquet");
    std::fs::rename(&path, &original).unwrap();
    std::fs::copy(&original, &path).unwrap();

    let result = session.app_resident_history();

    assert!(
        matches!(result, Err(Error::Bound("resident cache file changed"))),
        "{result:?}"
    );
}

#[test]
fn resident_history_keeps_exit_only_population_identical_after_cold_reopen() {
    // Goal: a terminal counter observation cannot create a resident gauge or inflate Other.
    // Method: capture measured/refused scheduled members, exit both, and compare real cold replay.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(11));
    publish_exit_only(&mut fixture);
    fixture.seal();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    crate::live::select_newest(&mut live);
    let before = live.app_resident_history().unwrap();
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut saved = LiveSession::open(&fixture.root).unwrap();
    saved.select(1001, 0).unwrap();

    let after = saved.app_resident_history().unwrap();

    assert_eq!(after["cohort"], before["cohort"]);
    assert_eq!(after["points"][1]["other"], before["points"][1]["other"]);
    assert_eq!(after["points"], before["points"]);
    assert_eq!(after["points"][0]["apps"][0]["known"], 11);
    assert_eq!(after["points"][1]["apps"][0], serde_json::Value::Null);
    assert_eq!(after["points"][1]["other"]["app_count"], 0);
}

pub(super) fn publish_exit_only(fixture: &mut Fixture) {
    use collector::ring::{
        IdentityObservation, ProcessCreation, ProcessIdentity, UsageAvailability,
    };
    let events = [1, 7].map(|pid| collector::tracker::Event::Exit {
        pid,
        last: Ok(usage(17)),
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
                run_tick: 1,
                cpus: &[collector::machine::CPUTicks {
                    user: 1,
                    system: 0,
                    nice: 0,
                    idle: 1,
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
                wall_ns: 1_001_000_000_000,
                continuous_ns: 1_000_000_000,
                awake_ns: 1_000_000_000,
            }),
            &collector::ring::IdentityPublication {
                processes: &[],
                events: &identities,
            },
        )
        .unwrap();
}

#[test]
fn resident_history_keeps_unknown_program_exit_coverage_identical_after_reopen() {
    // Goal: exited unknown programs cannot remain in resident-only ungrouped coverage.
    // Method: publish a real refused-program member then Exit and compare full cold points.
    let mut fixture = Fixture::new();
    publish_unknown_program(&mut fixture);
    publish_exit_only(&mut fixture);
    fixture.seal();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    crate::live::select_newest(&mut live);
    let before = live.app_resident_history().unwrap();
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut saved = LiveSession::open(&fixture.root).unwrap();
    saved.select(1001, 0).unwrap();

    let after = saved.app_resident_history().unwrap();

    assert_eq!(after["points"][0]["ungrouped_members"], 1);
    assert_eq!(after["points"][1]["ungrouped_members"], 0);
    assert_eq!(after["points"], before["points"]);
    assert_eq!(after["cohort"], before["cohort"]);
}

pub(super) fn publish_unknown_program(fixture: &mut Fixture) {
    use collector::ring::{
        IdentityObservation, ProcessCreation, ProcessIdentity, UsageAvailability,
    };
    let member = IdentityObservation {
        pid: 1,
        usage: UsageAvailability::Refused,
        identity: ProcessIdentity::Unavailable {
            creation: Some(ProcessCreation {
                unique_id: 1,
                pid_version: -1,
            }),
            reason: collector::ring::IdentityUnavailable::Refused,
        },
    };
    fixture
        .writer
        .publish_with_identity(
            &collector::ring::Readings {
                run_tick: 0,
                cpus: &[collector::machine::CPUTicks {
                    user: 0,
                    system: 0,
                    nice: 0,
                    idle: 0,
                }],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 1,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: collector::ring::Dropped::default(),
            },
            Some(&collector::ring::ObservationTimes {
                wall_ns: 1_000_000_000_000,
                continuous_ns: 0,
                awake_ns: 0,
            }),
            &collector::ring::IdentityPublication {
                processes: &[member],
                events: &[],
            },
        )
        .unwrap();
}

#[test]
fn resident_history_reuses_saved_facts_after_refreshing_endpoint_metadata() {
    // Goal: an unchanged saved context repeats only metadata work and no tick query,
    // preserving its entire response.
    // Method: reopen two real sealed publications and count the existing normalized-source reads.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(11));
    fixture.publish_with_usage(1001, &usage(29));
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let first = session.app_resident_history().unwrap();
    let saved = session.history.as_ref().unwrap();
    let reads = saved.source_reads.get();
    let queries = saved.tick_queries.get();

    let repeated = session.app_resident_history().unwrap();

    assert_eq!(repeated, first);
    assert_eq!(saved.source_reads.get() - reads, 1);
    assert_eq!(saved.tick_queries.get() - queries, 0);
}

#[test]
fn resident_history_rejects_replaced_saved_files_on_a_cache_hit() {
    // Goal: unchanged commitment metadata cannot authorize resident facts from a replaced file.
    // Method: warm real saved facts, replace events with identical bytes/new inode, then repeat.
    let (fixture, session) = saved_context();
    session.app_resident_history().unwrap();
    let saved = session.history.as_ref().unwrap();
    let path = crate::live::saved_events(&fixture.root, &saved.windows[0]);
    let original = fixture.root.join("events-original.parquet");
    std::fs::rename(&path, &original).unwrap();
    std::fs::copy(&original, &path).unwrap();

    let result = session.app_resident_history();

    assert!(
        matches!(result, Err(Error::Bound("resident cache file changed"))),
        "{result:?}"
    );
}

#[test]
fn resident_history_keeps_saved_facts_when_the_session_catalog_is_replaced() {
    // Goal: saved resident reuse belongs to live History rather than one saved Session catalog.
    // Method: warm facts, replace the actual saved reader, warm selected detail, and repeat.
    for multipart in [false, true] {
        let (fixture, mut session) = saved_context_with(multipart);
        let first = session.app_resident_history().unwrap();
        session.history = Some(crate::Session::open(&fixture.root).unwrap());
        session.apps_page(0).unwrap();
        let saved = session.history.as_ref().unwrap();
        let reads = saved.source_reads.get();

        let repeated = session.app_resident_history().unwrap();

        assert_eq!(repeated, first);
        assert_eq!(saved.source_reads.get() - reads, 1);
    }
}

fn saved_context() -> (Fixture, LiveSession) {
    saved_context_with(false)
}

fn saved_context_with(multipart: bool) -> (Fixture, LiveSession) {
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1000, &usage(11));
    fixture.publish_with_usage(1001, &usage(29));
    if multipart {
        fixture.writer.finish();
        let outcome = persistence::worker::run(
            &fixture.root,
            persistence::worker::default_memory_limit(),
            &crate::live::identity_fixture::Underived,
        )
        .unwrap();
        assert!(matches!(outcome, persistence::worker::Outcome::Complete));
    } else {
        fixture.seal();
    }
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    (fixture, session)
}

#[test]
fn resident_history_releases_saved_facts_when_no_saved_reader_can_validate_them() {
    // Goal: optional saved storage cannot survive unreported when its validating reader is absent.
    // Method: warm real facts, remove the saved contribution, and compare shared cache diagnostics.
    let (_fixture, session) = saved_context();
    let first = session.app_resident_history().unwrap();

    assert!(first["coverage"]["cache_bytes"].as_u64().unwrap() > 0);
}
