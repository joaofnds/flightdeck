use super::identity_fixture::Fixture;
use collector::processes::Usage;
use viewer::LiveSession;

#[test]
fn reads_captured_app_totals_without_a_registered_live_source() {
    // Goal: committed App endpoints stay readable after their live source is retired.
    // Method: seal two real identity publications, retire registration, and open a fresh reader.
    let fixture = commit_and_retire(Fixture::new());
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let selected = session.select(1002, 0).unwrap();
    let page = session.apps_page(0).unwrap();
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Apps/Current.app",
            0,
        )
        .unwrap();
    let history = session
        .app_history(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Apps/Current.app",
        )
        .unwrap();

    assert_eq!(selected["selected"]["publication"], 2);
    assert_eq!(page["selected"]["stamp"]["publication"], "2");
    assert_eq!(history["selected"], page["selected"]);
    assert_eq!(page["app_count"], 2);
    assert_eq!(page["process_count"], 2);
    assert_eq!(detail["app"]["key"]["display"], "/Apps/Current.app");
    assert_eq!(detail["app"]["cpu_ns"]["known"], "1");
    assert_eq!(detail["app"]["cpu_ns"]["measured_members"], 1);
    assert_eq!(detail["app"]["energy_nj"]["known"], "0");
    assert_eq!(detail["app"]["energy_nj"]["measured_members"], 1);
    assert_eq!(detail["app"]["resident_bytes"]["known"], "11");
    assert_eq!(detail["processes"][0]["pid"], 7);
    assert_eq!(detail["processes"][0]["creation"]["unique_id"], "7");
    assert_eq!(detail["processes"][0]["creation"]["pid_version"], -7);
}

#[test]
fn restores_app_history_without_a_registered_live_source() {
    // Goal: a cold client fills the rolling App trace from exact committed observations.
    // Method: retire two real publications and compare the final trace point with App detail.
    let fixture = commit_and_retire(Fixture::new());
    let mut session = LiveSession::open(&fixture.root).unwrap();

    session.select(1002, 0).unwrap();
    let history = session
        .app_history(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Apps/Current.app",
        )
        .unwrap();
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Apps/Current.app",
            0,
        )
        .unwrap();

    let points = history["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    assert_eq!(points[0]["observation"]["stamp"]["publication"], "1");
    assert_eq!(points[0]["cpu_percent_one_core"], serde_json::Value::Null);
    assert_eq!(points[1]["observation"], history["selected"]);
    assert_eq!(points[1]["cpu_ns"]["known"], "1");
    assert_eq!(points[1]["resident_bytes"]["known"], "11");
    assert_eq!(points[1]["cpu_ns"], detail["app"]["cpu_ns"]);
    assert_eq!(points[1]["energy_nj"], detail["app"]["energy_nj"]);
}

#[test]
fn keeps_live_app_readings_when_committed_history_becomes_unreadable() {
    // Goal: losing optional saved history cannot blank independently retained live readings.
    // Method: warm the saved trace, withhold its real data directory, then recover it.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();
    let kind = viewer::live_app_response::KeyKind::Bundle;
    let key = b"/Apps/Current.app";
    let before = session.app_history(kind, key).unwrap();
    let data = persistence::layout::base(&fixture.root, persistence::layout::Kind::Data);
    let withheld = fixture.root.join("withheld-data");
    std::fs::rename(&data, &withheld).unwrap();

    let history = session.app_history(kind, key).unwrap();
    let detail = session.app_detail(kind, key, 0).unwrap();

    assert_eq!(history["points"], before["points"]);
    assert_eq!(history["selected"], before["selected"]);
    assert_eq!(history["points"][1]["cpu_ns"]["known"], "1");
    assert_eq!(detail["app"]["resident_bytes"]["known"], "11");
    assert!(history["restoration_error"].as_str().unwrap().len() <= 2048);
    assert!(!history["restoration_error"].as_str().unwrap().is_empty());
    std::fs::rename(&withheld, &data).unwrap();
    let recovered = session.app_history(kind, key).unwrap();
    assert_eq!(recovered["points"], before["points"]);
    assert!(recovered["restoration_error"].is_null());
}

#[test]
fn preserves_refused_members_and_raw_app_bytes_in_committed_observations() {
    // Goal: cold App identity preserves byte keys and keeps refused quantities unavailable.
    // Method: reopen a sealed member with non-UTF-8 App/program bytes and refused usage.
    let fixture = commit_and_retire(Fixture::new());
    let mut session = LiveSession::open(&fixture.root).unwrap();

    session.select(1002, 0).unwrap();
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Apps/Outer\xfe.app",
            0,
        )
        .unwrap();

    assert_eq!(
        detail["app"]["key"]["bytes_hex"],
        "2f417070732f4f75746572fe2e617070"
    );
    assert_eq!(detail["app"]["resident_bytes"]["measured_members"], 0);
    assert_eq!(detail["app"]["resident_bytes"]["unavailable_members"], 1);
    assert_eq!(
        detail["app"]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(detail["processes"][0]["pid"], 1);
    assert_eq!(detail["processes"][0]["creation"]["unique_id"], "1");
    assert_eq!(
        detail["processes"][0]["executable_name"]["bytes_hex"],
        "72656675736564ff"
    );
    assert_eq!(detail["processes"][0]["cpu_ns"]["unavailable"], "refused");
    assert_eq!(
        detail["processes"][0]["resident_bytes"]["unavailable"],
        "unknown"
    );
}

#[test]
fn leaves_legacy_committed_processes_ungrouped_without_current_pid_lookups() {
    // Goal: old recordings expose unsupported identity without guessing a present-day App.
    // Method: retire a real legacy source, reopen its bytes, and inspect cold App membership.
    let fixture = commit_and_retire(Fixture::new_mode(false, 2, None));
    let mut session = LiveSession::open(&fixture.root).unwrap();

    session.select(1002, 0).unwrap();
    let page = session.apps_page(0).unwrap();

    assert_eq!(page["selected"]["stamp"]["publication"], "2");
    assert_eq!(page["app_count"], 0);
    assert_eq!(page["process_count"], 1);
    assert_eq!(page["ungrouped_members"], 1);
    assert_eq!(page["membership_complete"], false);
}

#[test]
fn follows_an_exact_predecessor_across_sealed_minutes() {
    // Goal: minute boundaries preserve rates without inventing a first baseline.
    // Method: reopen adjacent ticks 59 and 60 from separate immutable commitments.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1059);
    fixture.publish_with_system_rows(1060);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();

    session.select(1059, 0).unwrap();
    let first = session.apps_page(0).unwrap();
    session.select(1060, 0).unwrap();
    let second = session.apps_page(0).unwrap();

    assert_eq!(first["apps"][0]["cpu_ns"]["measured_members"], 0);
    assert_eq!(
        first["apps"][0]["cpu_percent_one_core"],
        serde_json::Value::Null
    );
    assert_eq!(second["selected"]["run_tick"], "60");
    assert_eq!(second["elapsed_ns"], "1000000000");
    assert_eq!(second["apps"][0]["cpu_ns"]["known"], "1");
    assert_eq!(second["apps"][0]["cpu_ns"]["measured_members"], 1);
    assert_eq!(second["apps"][0]["resident_bytes"]["known"], "11");
}

#[test]
fn sums_positive_committed_user_system_and_energy_intervals() {
    // Goal: cold App and process totals include both CPU counters and positive energy.
    // Method: seal independent 200 ms user, 300 ms system and 3 J increments, then retire live.
    let mut fixture = Fixture::new();
    let previous = Usage {
        awake_ns_at_start: 0,
        user_ns: 1_000_000_000,
        system_ns: 2_000_000_000,
        energy_nj: 5_000_000_000,
        instructions: 5,
        super_instructions: 6,
        cycles: 7,
        idle_wakeups: 8,
        interrupt_wakeups: 9,
        pageins: 10,
        resident_bytes: 11,
        footprint_bytes: 12,
        disk_read_bytes: 13,
        disk_written_bytes: 14,
    };
    let selected = Usage {
        user_ns: 1_200_000_000,
        system_ns: 2_300_000_000,
        energy_nj: 8_000_000_000,
        ..previous
    };
    fixture.publish_with_usage(1001, &previous);
    fixture.publish_with_usage(1002, &selected);
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();

    session.select(1002, 0).unwrap();
    let detail = session
        .app_detail(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Apps/Current.app",
            0,
        )
        .unwrap();

    let history = session
        .app_history(
            viewer::live_app_response::KeyKind::Bundle,
            b"/Apps/Current.app",
        )
        .unwrap();
    assert_eq!(history["points"][1]["cpu_percent_one_core"], 50.0);
    assert_eq!(history["points"][1]["energy_nj"]["known"], "3000000000");
    assert_eq!(history["points"][1]["energy_joules"], 3.0);
    assert_eq!(history["points"][1]["energy_watts"], 3.0);

    assert_eq!(detail["elapsed_ns"], "1000000000");
    assert_eq!(detail["app"]["cpu_ns"]["known"], "500000000");
    assert_eq!(detail["app"]["cpu_percent_one_core"], 50.0);
    assert_eq!(detail["app"]["energy_nj"]["known"], "3000000000");
    assert_eq!(detail["app"]["energy_joules"], 3.0);
    assert_eq!(detail["app"]["energy_watts"], 3.0);
    assert_eq!(detail["processes"][0]["cpu_ns"]["known"], "500000000");
    assert_eq!(detail["processes"][0]["cpu_percent_one_core"], 50.0);
    assert_eq!(detail["processes"][0]["energy_nj"]["known"], "3000000000");
    assert_eq!(detail["processes"][0]["energy_joules"], 3.0);
    assert_eq!(detail["processes"][0]["energy_watts"], 3.0);
}

fn commit_and_retire(mut fixture: Fixture) -> Fixture {
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();

    fixture
}
