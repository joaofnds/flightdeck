use super::*;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::Page;
use std::sync::Arc;

fn page(sample: &Sample) -> Result<Value> {
    sample.apps_page(Page {
        offset: 0,
        limit: 512,
    })
}

#[test]
fn restores_exact_selection_lazily_and_reuses_one_owned_sample() {
    // Goal: saved App queries use the explicit selected identity and reuse their projection.
    // Method: select a different scalar second, then read the earlier App twice from disk.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1002, 0).unwrap()["selected"].clone();
    session.select(1001, 0).unwrap();
    assert!(session.app_sample_cache.borrow().is_none());

    let restored = session.with_app_sample(&selected, page).unwrap();
    let reads = session.source_reads.get();
    let repeated = session.with_app_sample(&selected, page).unwrap();

    assert_eq!(restored, repeated);
    assert_eq!(session.source_reads.get(), reads);
    assert_eq!(restored["selected"]["stamp"]["publication"], "2");
    assert_eq!(restored["elapsed_ns"], "1000000000");
    assert_eq!(restored["apps"][0]["cpu_ns"]["known"], "1");
    assert_eq!(restored["apps"][1]["resident_bytes"]["measured_members"], 0);
    assert!(!restored["membership_complete"].as_bool().unwrap());
    assert!(
        session
            .app_sample_cache
            .borrow()
            .as_ref()
            .unwrap()
            .sample
            .bytes()
            <= SAMPLE_BYTES_MAX
    );
}

#[test]
fn clears_the_cached_sample_when_the_selected_identity_is_refused() {
    // Goal: a miss with invalid selection facts never leaves the previous success installed.
    // Method: warm the cache, change run_tick, and inspect the refusal and empty cache.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    let mut selected = session.select(1001, 0).unwrap()["selected"].clone();
    session.with_app_sample(&selected, page).unwrap();
    selected["run_tick"] = serde_json::json!(999);

    let result = session.with_app_sample(&selected, page);

    assert!(matches!(
        result,
        Err(Error::Bound("committed App selected run tick"))
    ));
    assert!(session.app_sample_cache.borrow().is_none());
}

#[test]
fn cancels_a_warm_cache_hit_and_releases_its_sample() {
    // Goal: cancellation blocks even a warm committed response and releases its sample.
    // Method: advance the owning cancellation epoch and request the already retained sample.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let cancellation = Arc::new(crate::Cancellation::default());
    let mut session = Session::open_with(&fixture.root, Arc::clone(&cancellation), 0).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    session.with_app_sample(&selected, page).unwrap();
    cancellation.cancel().unwrap();

    let result = session.with_app_sample(&selected, page);

    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
    );
    assert!(session.app_sample_cache.borrow().is_none());
}

#[test]
fn invalidates_a_warm_sample_when_begin_changes_the_epoch() {
    // Goal: beginning a new epoch directly releases the previous epoch's retained sample.
    // Method: cancel after warming, begin the new epoch without an intervening App operation.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let cancellation = Arc::new(crate::Cancellation::default());
    let mut session = Session::open_with(&fixture.root, Arc::clone(&cancellation), 0).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    session.with_app_sample(&selected, page).unwrap();
    assert!(session.app_sample_cache.borrow().is_some());
    cancellation.cancel().unwrap();

    session.begin(1).unwrap();

    assert!(session.app_sample_cache.borrow().is_none());
}

#[test]
fn retains_a_warm_sample_when_begin_keeps_the_same_epoch() {
    // Goal: beginning the current epoch keeps a valid projection available for reuse.
    // Method: warm the sample, begin epoch zero again, and observe no source reread.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    let restored = session.with_app_sample(&selected, page).unwrap();
    let reads = session.source_reads.get();

    session.begin(0).unwrap();

    assert!(session.app_sample_cache.borrow().is_some());
    assert_eq!(session.with_app_sample(&selected, page).unwrap(), restored);
    assert_eq!(session.source_reads.get(), reads);
}

#[test]
fn selection_invalidates_the_committed_sample_without_eager_app_reads() {
    // Goal: scalar selection invalidates App retention without querying process metadata.
    // Method: warm Apps, select the same scalar second, and inspect the empty cache.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    session.with_app_sample(&selected, page).unwrap();

    session.select(1001, 0).unwrap();

    assert!(session.app_sample_cache.borrow().is_none());
}

#[test]
fn retains_legacy_numeric_gauges_with_unsupported_app_identity() {
    // Goal: legacy saved rows keep their measured gauges without invented process identity.
    // Method: seal the legacy fixture, restore Apps, and inspect its ungrouped member.
    let mut fixture = Fixture::new_mode(false, 2, None);
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();

    let restored = session
        .with_app_sample(&selected, |sample| {
            sample.ungrouped_page(Page {
                offset: 0,
                limit: 512,
            })
        })
        .unwrap();

    assert_eq!(restored["app_count"], 0);
    assert_eq!(restored["ungrouped_members"], 1);
    assert_eq!(restored["processes"][0]["resident_bytes"]["known"], "11");
    assert_eq!(restored["processes"][0]["creation"], Value::Null);
    assert_eq!(
        restored["processes"][0]["program_unavailable"],
        "unsupported"
    );
}

#[test]
fn refuses_installation_when_canceled_after_projection() {
    // Goal: a superseded projection cannot install a retained committed Sample.
    // Method: cancel at the installation checkpoint after real source decoding completes.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let cancellation = Arc::new(crate::Cancellation::default());
    let mut session = Session::open_with(&fixture.root, Arc::clone(&cancellation), 0).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    *session.before_app_sample_install.borrow_mut() = Some(Box::new(move || {
        cancellation.cancel().unwrap();
    }));

    let result = session.with_app_sample(&selected, page);

    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
    );
    assert!(session.app_sample_cache.borrow().is_none());
}

#[test]
fn restores_the_exact_predecessor_from_the_adjacent_committed_minute() {
    // Goal: publication-adjacent saved App rates survive the elapsed-minute boundary.
    // Method: seal real run ticks 59 and 60 into separate commitments and restore tick 60.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1002, 0).unwrap()["selected"].clone();

    let restored = session.with_app_sample(&selected, page).unwrap();

    assert_eq!(session.windows.len(), 2);
    assert_eq!(restored["selected"]["run_tick"], "60");
    assert_eq!(restored["selected"]["stamp"]["publication"], "2");
    assert_eq!(restored["previous"]["publication"], "1");
    assert_eq!(restored["elapsed_ns"], "1000000000");
    assert_eq!(restored["apps"][0]["cpu_ns"]["known"], "1");
}

#[test]
fn refuses_a_warm_sample_after_its_committed_marker_disappears() {
    // Goal: retained App projections cannot bypass commitment revalidation.
    // Method: warm a real sealed sample, remove its existing marker, and request a cache hit.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    session.with_app_sample(&selected, page).unwrap();
    let marker =
        persistence::layout::WindowKey::from(&session.windows[0].identity).marker(&fixture.root);
    assert!(!std::fs::read(&marker).unwrap().is_empty());
    std::fs::remove_file(&marker).unwrap();

    let result = session.with_app_sample(&selected, page);

    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(session.app_sample_cache.borrow().is_none());
}

#[test]
fn rejects_publication_adjacent_rates_when_a_scheduled_run_tick_is_missing() {
    // Goal: a missing scheduled tick prevents an App delta across adjacent publications.
    // Method: publish run ticks zero and two, seal them, and restore the second observation.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.skip_run_ticks(1);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1002, 0).unwrap()["selected"].clone();

    let restored = session.with_app_sample(&selected, page).unwrap();

    assert_eq!(restored["selected"]["run_tick"], "2");
    assert_missing_interval(&restored);
}

#[test]
fn rejects_a_predecessor_without_a_saved_wall_clock() {
    // Goal: a clockless predecessor cannot supply rates even when its counters are readable.
    // Method: commit publication one's absent wall clock with valid interval clocks.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_with_observations(&[persistence::Observation {
        publication: 1,
        wall_ns: None,
        continuous_ns: Some(0),
        awake_ns: Some(0),
    }]);
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1002, 0).unwrap()["selected"].clone();

    let restored = session.with_app_sample(&selected, page).unwrap();

    assert_missing_interval(&restored);
}

#[test]
fn rejects_a_predecessor_whose_awake_clock_is_after_the_selected_clock() {
    // Goal: decreasing awake time prevents a committed interval from becoming measured.
    // Method: commit an increasing continuous pair with predecessor awake two seconds.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_with_observations(&[persistence::Observation {
        publication: 1,
        wall_ns: Some(1_001_000_000_000),
        continuous_ns: Some(0),
        awake_ns: Some(2_000_000_000),
    }]);
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1002, 0).unwrap()["selected"].clone();

    let restored = session.with_app_sample(&selected, page).unwrap();

    assert_missing_interval(&restored);
}

#[test]
fn rejects_a_predecessor_without_a_saved_awake_clock() {
    // Goal: missing awake time keeps a committed interval unavailable.
    // Method: commit a predecessor with wall and continuous clocks but no awake clock.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_with_observations(&[persistence::Observation {
        publication: 1,
        wall_ns: Some(1_001_000_000_000),
        continuous_ns: Some(0),
        awake_ns: None,
    }]);
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1002, 0).unwrap()["selected"].clone();

    let restored = session.with_app_sample(&selected, page).unwrap();

    assert_missing_interval(&restored);
}

#[test]
fn retains_a_measured_interval_when_the_saved_awake_clocks_are_equal() {
    // Goal: equal awake clocks remain compatible when continuous time advances.
    // Method: commit both awake clocks at one second and retain the ordinary counter delta.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_with_observations(&[persistence::Observation {
        publication: 1,
        wall_ns: Some(1_001_000_000_000),
        continuous_ns: Some(0),
        awake_ns: Some(1_000_000_000),
    }]);
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1002, 0).unwrap()["selected"].clone();

    let restored = session.with_app_sample(&selected, page).unwrap();

    assert_eq!(restored["previous"]["publication"], "1");
    assert_eq!(restored["elapsed_ns"], "1000000000");
    assert_eq!(restored["apps"][0]["cpu_ns"]["known"], "1");
    assert_eq!(restored["apps"][0]["cpu_ns"]["measured_members"], 1);
}

fn assert_missing_interval(restored: &Value) {
    assert_eq!(restored["selected"]["stamp"]["publication"], "2");
    assert_eq!(restored["previous"], Value::Null);
    assert_eq!(restored["elapsed_ns"], Value::Null);
    assert_eq!(restored["apps"][0]["key"]["display"], "/Apps/Current.app");
    assert_eq!(restored["apps"][0]["resident_bytes"]["known"], "11");
    assert_eq!(restored["apps"][0]["cpu_ns"]["measured_members"], 0);
    assert_eq!(restored["apps"][0]["energy_nj"]["measured_members"], 0);
    assert_eq!(restored["apps"][0]["cpu_percent_one_core"], Value::Null);
    assert_eq!(restored["apps"][0]["energy_watts"], Value::Null);
}

#[test]
fn refuses_a_warm_cross_minute_sample_when_only_its_predecessor_marker_disappears() {
    // Goal: a warm interval revalidates its predecessor's distinct commitment.
    // Method: warm tick 60, remove minute zero's marker, and preserve minute one's marker.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(1002, 0).unwrap()["selected"].clone();
    let restored = session.with_app_sample(&selected, page).unwrap();
    assert_eq!(restored["previous"]["publication"], "1");
    assert_eq!(restored["apps"][0]["cpu_ns"]["measured_members"], 1);
    let predecessor =
        persistence::layout::WindowKey::from(&session.windows[0].identity).marker(&fixture.root);
    let current =
        persistence::layout::WindowKey::from(&session.windows[1].identity).marker(&fixture.root);
    assert!(!std::fs::read(&predecessor).unwrap().is_empty());
    std::fs::remove_file(&predecessor).unwrap();

    let result = session.with_app_sample(&selected, page);

    assert!(!std::fs::read(&current).unwrap().is_empty());
    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(session.app_sample_cache.borrow().is_none());
}
