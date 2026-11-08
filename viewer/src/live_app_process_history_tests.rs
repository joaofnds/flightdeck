use serde_json::Value;

use crate::LiveSession;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::KeyKind;

#[path = "live_app_process_history_tests/fixture.rs"]
mod named;

#[test]
fn retains_selected_group_rates_and_exact_bytes_in_a_saved_window() {
    // Goal: a saved window keeps a group's rates and the exact bytes of large counters.
    // Method: publish large cumulative counters, seal them, and read the group with a new reader.
    let mut fixture = Fixture::new();
    fixture.set_clock_intervals(2_000_000_000, 100_000_000);
    let mut usage = usage();
    usage.awake_ns_at_start = 0;
    usage.disk_read_bytes = 9_007_199_254_740_993;
    fixture.publish_with_usage(1001, &usage);
    usage.user_ns += 500_000_000;
    usage.energy_nj += 4_000_000_000;
    usage.disk_read_bytes += 4096;
    usage.disk_written_bytes += 1024;
    fixture.publish_with_usage(1002, &usage);
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    let saved = selected(&mut reopened);

    assert_eq!(saved["executable_name"]["bytes_hex"], "63757272656e74");
    let point = saved["points"].as_array().unwrap().last().unwrap();
    assert_eq!(point["cpu_percent_one_core"], 25.0);
    assert_eq!(point["energy_watts"], 2.0);
    assert_eq!(point["disk_read_bytes"]["known"], 4096);
    assert_eq!(point["disk_read_bps"], 2048.0);
    assert_eq!(point["disk_written_bps"], 512.0);
    assert_eq!(point["disk_io_bps"], 2560.0);
}

#[test]
fn keeps_a_group_apart_from_the_other_executables_of_its_app_in_a_window_s_parts() {
    // Goal: a group's history from the parts a sealed window keeps counts only the processes
    // of its App that run the group's executable, as its history from the window's rows does.
    // Method: publish an App running two executables, read one executable's group from the
    // window sealed without its Apps' parts, then seal it with them and read the group again.
    let mut fixture = Fixture::new();
    let rows = [
        named::member(7, b"worker", b"/A.app", usage()),
        named::member(8, b"helper", b"/A.app", usage()),
    ];
    let mut session = LiveSession::open(&fixture.root).unwrap();
    named::publish(&mut fixture, 1, &rows);
    session.list_saved().unwrap();
    named::publish(&mut fixture, 2, &rows);
    fixture.seal();
    session.list_saved().unwrap();
    let from_rows = worker(&mut session);
    drop(session);
    fixture.seal_derived(&[], &persistence::sealed_apps::SealedApps::default());
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();

    let from_parts = worker(&mut reopened);

    assert_eq!(from_rows["points"][1]["members"], 1);
    assert_eq!(from_parts["points"], from_rows["points"]);
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
    fixture.seal();
    session.list_saved().unwrap();
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
    assert_eq!(grouped["points"][1]["resident_bytes"]["known"], 2048);
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
    fixture.seal();
    session.list_saved().unwrap();
    session.select(1002, 0).unwrap();

    let group = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"after")
        .unwrap();

    assert_eq!(group["points"][0]["members"], Value::Null);
    assert_eq!(group["points"][1]["cpu_percent_one_core"], Value::Null);
    assert_eq!(group["points"][1]["cpu_ns"]["unavailable_members"], 1);
    assert_eq!(group["points"][1]["resident_bytes"]["known"], 1024);
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
    fixture.seal();
    session.list_saved().unwrap();
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

    assert_eq!(saved["points"][1]["cpu_ns"], amount(100_000_000, 1));
    assert_eq!(saved["points"][1]["energy_nj"], amount(200_000_000, 1));
    assert_eq!(saved["points"][1]["disk_read_bytes"], amount(4_096, 1));
    assert_eq!(saved["points"][1]["disk_written_bytes"], amount(1_024, 1));
    assert_eq!(saved["points"][1]["disk_io_bytes"], amount(5_120, 1));
    assert_eq!(saved["points"][1]["resident_bytes"], amount(0, 0));
    assert_eq!(saved["points"][1]["footprint_bytes"], amount(0, 0));
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
    fixture.seal();
    session.list_saved().unwrap();
    session.select(1002, 0).unwrap();

    let group = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(group["points"][1]["resident_bytes"]["known"], 1024);
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
    fixture.seal();
    session.list_saved().unwrap();
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
    fixture.seal();
    let mut session =
        LiveSession::open_with(&fixture.root, std::sync::Arc::clone(&cancellation), 0).unwrap();
    session.select(1001, 0).unwrap();
    cancellation.cancel().unwrap();

    let result = session.app_process_history(KeyKind::Bundle, b"/Apps/Current.app", b"current");

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
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
fn refuses_a_warm_group_trace_after_its_commitment_disappears() {
    // A discarded live payload cannot turn a failed saved commitment into successful output.
    let mut fixture = Fixture::new();
    fixture.publish_with_usage(1001, &usage());
    fixture.publish_with_usage(1002, &usage());
    fixture.seal();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    selected(&mut session);
    let marker = fixture.marker(session.history.as_ref().unwrap().windows[0].identity.window);
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
    fixture.seal();
    session.list_saved().unwrap();
    session.select(1002, 0).unwrap();

    let group = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(
        group["points"][1]["cpu_ns"]["known"],
        9_007_199_254_740_993_u64
    );
    assert_eq!(group["points"][1]["resident_bytes"]["overflowed"], true);
    assert_eq!(group["points"][1]["resident_bytes"]["measured_members"], 1);
    assert_eq!(
        group["points"][1]["resident_bytes"]["unavailable_members"],
        1
    );
}

#[test]
fn sums_independent_nonzero_member_amounts_and_rates_of_saved_history() {
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
    named::publish(
        &mut fixture,
        2,
        &[
            named::member(7, b"worker", b"/A.app", sums_first_member_usage()),
            named::member(8, b"worker", b"/A.app", sums_second_member_usage()),
        ],
    );
    fixture.seal();
    session.list_saved().unwrap();
    session.select(1002, 0).unwrap();

    let listed = session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();
    drop(session);
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    reopened.select(1002, 0).unwrap();
    let unregistered = reopened
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap();

    assert_eq!(
        metric_values(&listed["points"][1]),
        serde_json::json!({
        "cpu_ns":amount(360_000_000,2),"energy_nj":amount(1_000_000_000,2),
        "resident_bytes":amount(3_000,2),"footprint_bytes":amount(8_000,2),
        "disk_read_bytes":amount(3_072,2),"disk_written_bytes":amount(768,2),
        "disk_io_bytes":amount(3_840,2),"cpu_percent_one_core":36.0,
        "energy_joules":1.0,"energy_watts":1.0,"disk_read_bps":3072.0,
        "disk_written_bps":768.0,"elapsed_ns":1_000_000_000,
        "members":2})
    );
    assert!((listed["points"][1]["disk_io_bps"].as_f64().unwrap() - 3840.0).abs() < 1e-9);
    assert_eq!(unregistered["points"], listed["points"]);
}

fn sums_first_member_usage() -> collector::processes::Usage {
    let mut first = usage();
    first.user_ns += 100_000_000;
    first.system_ns += 20_000_000;
    first.energy_nj += 400_000_000;
    first.disk_read_bytes = 1024;
    first.disk_written_bytes = 256;
    first.resident_bytes = 1000;
    first.footprint_bytes = 3000;
    first
}

fn sums_second_member_usage() -> collector::processes::Usage {
    let mut second = usage();
    second.user_ns += 200_000_000;
    second.system_ns += 40_000_000;
    second.energy_nj += 600_000_000;
    second.disk_read_bytes = 2048;
    second.disk_written_bytes = 512;
    second.resident_bytes = 2000;
    second.footprint_bytes = 5000;
    second
}

fn amount(known: u64, measured_members: u32) -> Value {
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
