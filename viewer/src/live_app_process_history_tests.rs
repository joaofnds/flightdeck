use serde_json::Value;

use crate::LiveSession;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::KeyKind;

#[path = "live_app_process_history_tests/fixture.rs"]
mod named;

#[test]
fn retains_selected_group_rates_and_exact_bytes_across_sealing() {
    // Publish large cumulative counters, then compare the group interval before and after sealing.
    let mut fixture = Fixture::new();
    fixture.set_clock_intervals(2_000_000_000, 100_000_000);
    let mut usage = usage();
    usage.awake_ns_at_start = 0;
    usage.disk_read_bytes = 9_007_199_254_740_993;
    let mut session = LiveSession::open(&fixture.root).unwrap();
    fixture.publish_with_usage(1001, &usage);
    session.poll().unwrap();
    usage.user_ns += 500_000_000;
    usage.energy_nj += 4_000_000_000;
    usage.disk_read_bytes += 4096;
    usage.disk_written_bytes += 1024;
    fixture.publish_with_usage(1002, &usage);
    session.poll().unwrap();

    let live = selected(&mut session);
    drop(session);
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    let saved = selected(&mut reopened);

    assert_eq!(live["points"], saved["points"]);
    assert_eq!(live["executable_name"]["bytes_hex"], "63757272656e74");
    let point = live["points"].as_array().unwrap().last().unwrap();
    assert_eq!(point["cpu_percent_one_core"], 25.0);
    assert_eq!(point["energy_watts"], 2.0);
    assert_eq!(point["disk_read_bytes"]["known"], "4096");
    assert_eq!(point["disk_read_bps"], 2048.0);
    assert_eq!(point["disk_written_bps"], 512.0);
    assert_eq!(point["disk_io_bps"], 2560.0);
}

#[test]
fn keeps_a_group_apart_from_the_other_executables_of_its_app_across_sealing() {
    // Goal: a group's history from the parts a sealed window keeps counts only the processes
    // of its App that run the group's executable, as the live history does.
    // Method: publish an App running two executables, seal the window with its Apps' parts,
    // and compare the history of one executable's group before and after sealing.
    let mut fixture = Fixture::new();
    let rows = [
        named::member(7, b"worker", b"/A.app", usage()),
        named::member(8, b"helper", b"/A.app", usage()),
    ];
    let mut session = LiveSession::open(&fixture.root).unwrap();
    named::publish(&mut fixture, 1, &rows);
    session.poll().unwrap();
    named::publish(&mut fixture, 2, &rows);
    session.poll().unwrap();
    let live = worker(&mut session);
    drop(session);
    fixture.seal_derived(&[], &crate::SealedApps::default());
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();

    let saved = worker(&mut reopened);

    assert_eq!(live["points"][1]["members"], 1);
    assert_eq!(saved["points"], live["points"]);
}

fn worker(session: &mut LiveSession) -> Value {
    session.select(1002, 0).unwrap();
    session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap()
}

fn selected(session: &mut LiveSession) -> Value {
    session.select(1002, 0).unwrap();
    session
        .app_process_history(KeyKind::Bundle, b"/Apps/Current.app", b"current")
        .unwrap()
}

fn usage() -> collector::processes::Usage {
    collector::processes::Usage {
        awake_ns_at_start: 0,
        user_ns: 100,
        system_ns: 200,
        energy_nj: 300,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes: 1024,
        footprint_bytes: 2048,
        disk_read_bytes: 0,
        disk_written_bytes: 0,
    }
}

#[test]
fn sums_same_byte_names_across_pids_without_merging_other_apps() {
    // Two process instances form one byte-named group; another App never joins that group.
    let mut fixture = Fixture::new();
    let rows = [
        named::member(7, b"worker\xff", b"/A.app", usage()),
        named::member(8, b"worker\xff", b"/A.app", usage()),
    ];
    named::publish(&mut fixture, 1, &rows);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    named::publish(&mut fixture, 2, &rows);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let grouped = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker\xff")
        .unwrap();
    let different = session
        .app_process_history(KeyKind::Bundle, b"/B.app", b"worker\xff")
        .unwrap();
    let other_name = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker\xfe")
        .unwrap();

    assert_eq!(grouped["points"][1]["members"], 2);
    assert_eq!(grouped["points"][1]["resident_bytes"]["known"], "2048");
    assert_eq!(grouped["executable_name"]["bytes_hex"], "776f726b6572ff");
    assert_eq!(different["points"][1]["members"], Value::Null);
    assert_eq!(different["points"][1]["source_available"], true);
    assert_eq!(other_name["points"][1]["resident_bytes"], Value::Null);
    assert_ne!(other_name["executable_name"], grouped["executable_name"]);
}

#[test]
fn gaps_an_exec_crossing_inside_the_same_app() {
    // A counter interval spanning two names cannot belong wholly to its final named group.
    let mut fixture = Fixture::new();
    named::publish(
        &mut fixture,
        1,
        &[named::member(7, b"before", b"/A.app", usage())],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut current = usage();
    current.user_ns += 100;
    named::publish(
        &mut fixture,
        2,
        &[named::member(7, b"after", b"/A.app", current)],
    );
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let group = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"after")
        .unwrap();

    assert_eq!(group["points"][0]["members"], Value::Null);
    assert_eq!(group["points"][1]["cpu_percent_one_core"], Value::Null);
    assert_eq!(group["points"][1]["cpu_ns"]["unavailable_members"], 1);
    assert_eq!(group["points"][1]["resident_bytes"]["known"], "1024");
}

#[test]
fn retains_verified_exit_counters_without_carrying_memory_forward() {
    // Terminal usage contributes its final delta, while the process is no longer resident.
    let mut fixture = Fixture::new();
    named::publish(
        &mut fixture,
        1,
        &[named::member(7, b"worker", b"/A.app", usage())],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut current = usage();
    current.user_ns += 100_000_000;
    current.energy_nj += 200_000_000;
    current.disk_read_bytes = 4096;
    current.disk_written_bytes = 1024;
    let mut exited = named::member(7, b"worker", b"/A.app", current);
    exited.exited = true;
    named::publish(&mut fixture, 2, &[exited]);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let group = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();
    fixture.seal();
    drop(session);
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    reopened.select(1002, 0).unwrap();
    let saved = reopened
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(reopened.catalog()["tail_ticks"], 0);
    assert_eq!(saved["points"][1]["cpu_ns"], amount("100000000", 1));
    assert_eq!(saved["points"][1]["energy_nj"], amount("200000000", 1));
    assert_eq!(saved["points"][1]["disk_read_bytes"], amount("4096", 1));
    assert_eq!(saved["points"][1]["disk_written_bytes"], amount("1024", 1));
    assert_eq!(saved["points"][1]["disk_io_bytes"], amount("5120", 1));
    assert_eq!(saved["points"][1]["resident_bytes"], amount("0", 0));
    assert_eq!(saved["points"][1]["footprint_bytes"], amount("0", 0));
    assert_eq!(saved["points"], group["points"]);
}

#[test]
fn retains_partial_group_coverage_instead_of_fabricating_missing_members() {
    // Refused member gauges and counter resets keep their coverage beside a known member.
    let mut fixture = Fixture::new();
    let rows = [
        named::member(7, b"worker", b"/A.app", usage()),
        named::member(8, b"worker", b"/A.app", usage()),
    ];
    named::publish(&mut fixture, 1, &rows);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut refused = named::member(8, b"worker", b"/A.app", usage());
    refused.usage = None;
    let mut reset = usage();
    reset.user_ns = 0;
    named::publish(
        &mut fixture,
        2,
        &[named::member(7, b"worker", b"/A.app", reset), refused],
    );
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let group = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(group["points"][1]["resident_bytes"]["known"], "1024");
    assert_eq!(group["points"][1]["resident_bytes"]["measured_members"], 1);
    assert_eq!(
        group["points"][1]["resident_bytes"]["unavailable_members"],
        1
    );
    assert_eq!(group["points"][1]["cpu_ns"]["measured_members"], 0);
    assert_eq!(group["points"][1]["cpu_ns"]["unavailable_members"], 2);
    assert_eq!(group["points"][1]["cpu_percent_one_core"], Value::Null);
}

#[test]
fn retains_app_and_name_identity_after_the_last_instance_is_gone() {
    // A departed named selection keeps its envelope and earlier readings, with a current gap.
    let mut fixture = Fixture::new();
    named::publish(
        &mut fixture,
        1,
        &[named::member(7, b"worker", b"/A.app", usage())],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    named::publish(&mut fixture, 2, &[]);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let group = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(group["app_key"]["bytes_hex"], "2f412e617070");
    assert_eq!(group["executable_name"]["bytes_hex"], "776f726b6572");
    assert_eq!(group["scope"], "selected_named_process_group");
    assert_eq!(group["points"][0]["members"], 1);
    assert_eq!(group["points"][1]["members"], Value::Null);
    assert_eq!(group["points"][1]["source_available"], true);
}

#[test]
fn cancels_selected_group_replay_without_returning_a_partial_response() {
    // A superseded operation cannot publish a seemingly successful partial group trace.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1001, &usage());
    let cancellation = std::sync::Arc::new(crate::Cancellation::default());
    let mut session =
        LiveSession::open_with(&fixture.root, std::sync::Arc::clone(&cancellation), 0).unwrap();
    session.select(1001, 0).unwrap();
    cancellation.cancel().unwrap();

    let result = session.app_process_history(KeyKind::Bundle, b"/Apps/Current.app", b"current");

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
}

#[test]
fn trims_optional_groups_before_app_facts_under_a_full_live_context() {
    // A real 600-App, 900-tick tail exceeds the group budget while ordinary App facts still fit.
    let mut fixture = named::dense();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let started = std::time::Instant::now();
    named::dense_context(&mut fixture, &mut session);
    session.select(1900, 0).unwrap();
    let mut path = b"/Apps/0000/".to_vec();
    path.resize(1020, b'x');
    path.extend_from_slice(b".app");

    let group = session
        .app_process_history(KeyKind::Bundle, &path, b"worker")
        .unwrap();
    let app = session.app_history(KeyKind::Bundle, &path).unwrap();
    let cpu = session
        .app_attribution_history(crate::AppAttributionMetric::Cpu)
        .unwrap();
    let power = session
        .app_attribution_history(crate::AppAttributionMetric::Power)
        .unwrap();
    let repeated = session
        .app_attribution_history(crate::AppAttributionMetric::Cpu)
        .unwrap();

    assert_eq!(app["points"].as_array().unwrap().len(), 900);
    assert_eq!(app["coverage"]["retention_limited"], false);
    assert_eq!(group["coverage"]["retention_limited"], true);
    assert_eq!(group["points"][0]["source_available"], false);
    assert_eq!(group["points"][899]["source_available"], true);
    assert_eq!(group["points"][899]["cpu_ns"]["known"], "1000");
    assert_eq!(cpu["points"], repeated["points"]);
    assert_eq!(power["points"].as_array().unwrap().len(), 900);
    assert!(group["coverage"]["cache_bytes"].as_u64().unwrap() <= 67_108_864);
    println!(
        "{}",
        serde_json::json!({"fixture_only":true,
        "apps":600,"observations":900,"wall_ms":started.elapsed().as_millis(),
        "group_coverage":group["coverage"],"app_coverage":app["coverage"],
        "cpu_coverage":cpu["coverage"],"power_coverage":power["coverage"]})
    );
}

#[test]
fn reuses_separate_saved_app_and_group_targets_when_the_ui_alternates_them() {
    // The Apps worker loads both targets; alternating them must retain both warm results.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1001, &usage());
    fixture.publish_with_usage(1002, &usage());
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1002, 0).unwrap();
    let app = session
        .app_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();
    let group = selected(&mut session);
    session
        .app_attribution_history(crate::AppAttributionMetric::Cpu)
        .unwrap();
    let reads = session.history.as_ref().unwrap().source_reads.get();

    let repeated_app = session
        .app_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();
    let repeated_group = session
        .app_process_history(KeyKind::Bundle, b"/Apps/Current.app", b"current")
        .unwrap();
    session
        .app_attribution_history(crate::AppAttributionMetric::Power)
        .unwrap();
    let warm_reads = session.history.as_ref().unwrap().source_reads.get() - reads;

    assert_eq!(app["points"], repeated_app["points"]);
    assert_eq!(group["points"], repeated_group["points"]);
    assert_eq!(app["selected"], repeated_app["selected"]);
    assert_eq!(group["executable_name"], repeated_group["executable_name"]);
    // Power's existing replay-plan tick lookup is one source read; neither target restores again.
    assert_eq!(warm_reads, 1);
    println!(
        "{}",
        serde_json::json!({"fixture_only":true,
        "alternating_app_group_target_reads":warm_reads,"expected_power_plan_reads":1})
    );
}

#[test]
fn preserves_group_facts_and_absent_name_semantics_across_committed_overlap() {
    // Committed overlap preserves measured live facts and observed absence of another name.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1001, &usage());
    let mut session = LiveSession::open(&fixture.root).unwrap();
    fixture.publish_with_usage(1002, &usage());
    session.poll().unwrap();
    let live = selected(&mut session);
    fixture.seal();

    session.refresh_history().unwrap();
    let restored = selected(&mut session);
    let absent = session
        .app_process_history(KeyKind::Bundle, b"/Apps/Current.app", b"absent")
        .unwrap();

    assert_eq!(restored["points"], live["points"]);
    assert_eq!(
        restored["coverage"]["live_group_cache_bytes"],
        live["coverage"]["live_group_cache_bytes"]
    );
    assert_eq!(restored["coverage"]["live_group_observations_retained"], 2);
    assert_eq!(restored["coverage"]["retention_limited"], false);
    assert_eq!(absent["points"][1]["source_available"], true);
    assert_eq!(absent["points"][1]["members"], Value::Null);
    assert_eq!(absent["executable_name"]["bytes_hex"], "616273656e74");
}

#[test]
fn refuses_a_warm_group_trace_after_its_commitment_disappears() {
    // A discarded live payload cannot turn a failed saved commitment into successful output.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1001, &usage());
    fixture.publish_with_usage(1002, &usage());
    fixture.seal();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    selected(&mut session);
    let marker = persistence::layout::WindowKey::from(
        &session.history.as_ref().unwrap().windows[0].identity,
    )
    .marker(&fixture.root);
    std::fs::remove_file(marker).unwrap();

    let result = session.app_process_history(KeyKind::Bundle, b"/Apps/Current.app", b"current");

    assert!(result.is_err());
}

#[test]
fn preserves_large_group_amounts_and_suppresses_an_overflowed_gauge() {
    // Exact integers survive above float precision, while a summed memory overflow stays absent.
    let mut fixture = Fixture::new();
    named::publish(
        &mut fixture,
        1,
        &[
            named::member(7, b"worker", b"/A.app", usage()),
            named::member(8, b"worker", b"/A.app", usage()),
        ],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut large = usage();
    large.user_ns += 9_007_199_254_740_993;
    large.resident_bytes = u64::MAX;
    let mut extra = usage();
    extra.resident_bytes = 1;
    named::publish(
        &mut fixture,
        2,
        &[
            named::member(7, b"worker", b"/A.app", large),
            named::member(8, b"worker", b"/A.app", extra),
        ],
    );
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let group = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(group["points"][1]["cpu_ns"]["known"], "9007199254740993");
    assert_eq!(group["points"][1]["resident_bytes"]["overflowed"], true);
    assert_eq!(group["points"][1]["resident_bytes"]["measured_members"], 1);
    assert_eq!(
        group["points"][1]["resident_bytes"]["unavailable_members"],
        1
    );
}

#[test]
fn retains_a_live_group_publication_omitted_inside_a_committed_range() {
    // A real sparse sealed minute encloses p2 without recording its measured group interval.
    let mut fixture = Fixture::new();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish_interval(&mut fixture, &mut session, 1, false);
    publish_interval(&mut fixture, &mut session, 2, false);
    publish_interval(&mut fixture, &mut session, 3, false);
    session.select(1003, 0).unwrap();
    let before = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();
    named::seal_selected(&fixture, &[1, 3]);

    session.refresh_history().unwrap();
    let after = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(
        session.history.as_ref().unwrap().windows[0].publication_first,
        1
    );
    assert_eq!(
        session.history.as_ref().unwrap().windows[0].publication_end,
        4
    );
    assert_eq!(before["points"][1]["cpu_ns"]["known"], "100000000");
    assert_eq!(before["points"][1]["disk_read_bytes"]["known"], "4096");
    assert_eq!(after["points"], before["points"]);
    assert_eq!(after["coverage"]["retention_limited"], false);
}

#[test]
fn retains_a_live_group_interval_when_only_its_endpoint_is_committed() {
    // Saving p2 alone cannot replace the valid interval captured against uncommitted p1.
    let mut fixture = Fixture::new();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish_interval(&mut fixture, &mut session, 1, false);
    publish_interval(&mut fixture, &mut session, 2, false);
    session.select(1002, 0).unwrap();
    let before = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();
    named::seal_selected(&fixture, &[2]);

    session.refresh_history().unwrap();
    let after = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(before["points"][1]["energy_nj"]["known"], "200000000");
    assert_eq!(before["points"][1]["disk_io_bytes"]["known"], "5120");
    assert_eq!(after["points"], before["points"]);
}

#[test]
fn retains_a_live_terminal_interval_when_its_baseline_is_not_committed() {
    // A terminal saved endpoint without p1 cannot erase the captured verified exit work.
    let mut fixture = Fixture::new();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    publish_interval(&mut fixture, &mut session, 1, false);
    publish_interval(&mut fixture, &mut session, 2, true);
    session.select(1002, 0).unwrap();
    let before = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();
    named::seal_selected(&fixture, &[2]);

    session.refresh_history().unwrap();
    let after = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(before["points"][1]["disk_read_bytes"]["known"], "4096");
    assert_eq!(before["points"][1]["resident_bytes"]["measured_members"], 0);
    assert_eq!(after["points"], before["points"]);
}

fn publish_interval(
    fixture: &mut Fixture,
    session: &mut LiveSession,
    publication: u64,
    exited: bool,
) {
    let mut amount = usage();
    amount.user_ns += publication * 100_000_000;
    amount.energy_nj += publication * 200_000_000;
    amount.disk_read_bytes = publication * 4096;
    amount.disk_written_bytes = publication * 1024;
    let mut row = named::member(7, b"worker", b"/A.app", amount);
    row.exited = exited;
    named::publish(fixture, publication, &[row]);
    session.poll().unwrap();
}

#[test]
fn sums_independent_nonzero_member_amounts_and_rates_in_live_and_saved_history() {
    // Distinct contributions pin every exact group subtotal independently of shared aggregation.
    let mut fixture = Fixture::new();
    named::publish(
        &mut fixture,
        1,
        &[
            named::member(7, b"worker", b"/A.app", usage()),
            named::member(8, b"worker", b"/A.app", usage()),
        ],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut first = usage();
    first.user_ns += 100_000_000;
    first.system_ns += 20_000_000;
    first.energy_nj += 400_000_000;
    first.disk_read_bytes = 1024;
    first.disk_written_bytes = 256;
    first.resident_bytes = 1000;
    first.footprint_bytes = 3000;
    let mut second = usage();
    second.user_ns += 200_000_000;
    second.system_ns += 40_000_000;
    second.energy_nj += 600_000_000;
    second.disk_read_bytes = 2048;
    second.disk_written_bytes = 512;
    second.resident_bytes = 2000;
    second.footprint_bytes = 5000;
    named::publish(
        &mut fixture,
        2,
        &[
            named::member(7, b"worker", b"/A.app", first),
            named::member(8, b"worker", b"/A.app", second),
        ],
    );
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let live = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();
    fixture.seal();
    drop(session);
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    reopened.select(1002, 0).unwrap();
    let saved = reopened
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(
        metric_values(&live["points"][1]),
        serde_json::json!({
        "cpu_ns":amount("360000000",2),"energy_nj":amount("1000000000",2),
        "resident_bytes":amount("3000",2),"footprint_bytes":amount("8000",2),
        "disk_read_bytes":amount("3072",2),"disk_written_bytes":amount("768",2),
        "disk_io_bytes":amount("3840",2),"cpu_percent_one_core":36.0,
        "energy_joules":1.0,"energy_watts":1.0,"disk_read_bps":3072.0,
        "disk_written_bps":768.0,"elapsed_ns":"1000000000",
        "members":2})
    );
    assert!((live["points"][1]["disk_io_bps"].as_f64().unwrap() - 3840.0).abs() < 1e-9);
    assert_eq!(saved["points"], live["points"]);
}

fn amount(known: &str, measured_members: u32) -> Value {
    serde_json::json!({"known":known,"measured_members":measured_members,
        "unavailable_members":0,"overflowed":false})
}

fn metric_values(point: &Value) -> Value {
    serde_json::json!({"cpu_ns":point["cpu_ns"],"energy_nj":point["energy_nj"],
        "resident_bytes":point["resident_bytes"],"footprint_bytes":point["footprint_bytes"],
        "disk_read_bytes":point["disk_read_bytes"],"disk_written_bytes":point["disk_written_bytes"],
        "disk_io_bytes":point["disk_io_bytes"],"cpu_percent_one_core":point["cpu_percent_one_core"],
        "energy_joules":point["energy_joules"],"energy_watts":point["energy_watts"],
        "disk_read_bps":point["disk_read_bps"],"disk_written_bps":point["disk_written_bps"],
        "elapsed_ns":point["elapsed_ns"],"members":point["members"]})
}
