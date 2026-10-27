use std::path::Path;

use serde_json::{Value, json};

use super::{SealedRecording, SnapshotReader};

#[path = "live_native_measurement_diagnostics.rs"]
mod diagnostics;
#[path = "live_native_measurement_ownership.rs"]
mod ownership;
#[path = "live_native_measurement_payloads.rs"]
pub(crate) mod payloads;
#[path = "live_native_measurement_raw.rs"]
mod raw;
#[path = "live_native_measurement_selection.rs"]
mod selection;

#[test]
fn measures_the_ranking_step_after_histories_are_read_early() {
    // Goal: a complete measured request includes the ranks deferred after early histories.
    // Method: drive the existing sealed-part fixture through the measurement's actual reader.
    let recording = SealedRecording::with_kept_parts();
    let mut reader = SnapshotReader::new();

    let (snapshot, count) = read_complete(&mut reader, &recording.root);

    assert_eq!(count, 2);
    assert!(super::super::reader_published(
        &reader.reader,
        reader.command.generation
    ));
    assert!(!snapshot.histories_pending);
    assert!(snapshot.app_ranks.is_some());
}

#[test]
fn requires_a_started_read_to_observe_interruption_before_replacement() {
    // Goal: cancellation coverage requires a real entered operation and a completed replacement.
    // Method: hold the Worker after its epoch begins, supersede it, and observe Interrupted.
    let recording = SealedRecording::with_kept_parts();
    let mut reader = SnapshotReader::new();
    let (snapshot, _) = read_complete(&mut reader, &recording.root);
    let expected = json!({"selected": snapshot.selection["selected"],
        "observations": snapshot.selection["trends"],
        "payloads": {"overview": payloads::report(&snapshot).unwrap()}});

    crate::live_probe::matched_tests::checks_interrupted_read(&recording.root, &expected);
}

#[test]
fn requires_thread_exit_before_teardown_success() {
    // Goal: a stopped request cannot substitute for observed Worker thread termination.
    // Method: retain a real read at its checkpoint, reject teardown, then release and join it.
    let recording = SealedRecording::with_kept_parts();

    crate::live_probe::matched_tests::checks_held_teardown(&recording.root);
}

#[test]
fn withholds_mailbox_completion_until_pending_ranks_finish() {
    // Goal: matching early payloads cannot complete a generation with ranking work held.
    // Method: gate the actual Worker before pending ranks and exercise the harness predicate.
    let recording = SealedRecording::with_kept_parts();
    let mut reader = SnapshotReader::new();
    let (snapshot, _) = read_complete(&mut reader, &recording.root);
    let expected = json!({"selected": snapshot.selection["selected"],
        "observations": snapshot.selection["trends"],
        "payloads": {"overview": payloads::report(&snapshot).unwrap()}});

    crate::live_probe::matched_tests::checks_pending_completion(&recording.root, &expected);
}

#[test]
fn waits_for_expected_completed_payloads_through_the_real_mailbox() {
    // Goal: the gated mailbox caller includes deferred rankings before reporting success.
    // Method: use the existing early-history fixture and an exact completed reader expectation.
    let recording = SealedRecording::with_kept_parts();
    let mut reader = SnapshotReader::new();
    let (snapshot, _) = read_complete(&mut reader, &recording.root);
    let expected = json!({"selected": snapshot.selection["selected"],
        "observations": snapshot.selection["trends"],
        "payloads": {"overview": payloads::report(&snapshot).unwrap()}});

    let delivered = crate::live_probe::matched_tests::read_recorded(&recording.root, &expected);

    assert!(!delivered.histories_pending);
    assert_eq!(
        payloads::validate(&delivered, "overview", &expected).unwrap(),
        expected["payloads"]["overview"]
    );
}

#[test]
fn records_unavailable_history_as_a_failed_payload() {
    // Goal: a finished reader cannot make a failed history look like an available payload.
    // Method: corrupt the existing sealed fixture's App parts and report its completed request.
    let recording = SealedRecording::with_kept_parts();
    recording.corrupt_app_parts();
    let mut reader = SnapshotReader::new();

    let (snapshot, _) = read_complete(&mut reader, &recording.root);
    let report = payloads::report(&snapshot).unwrap();

    assert!(super::super::reader_published(
        &reader.reader,
        reader.command.generation
    ));
    assert_eq!(report["resident_history"]["state"], "unavailable");
    assert!(
        report["resident_history"]["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
    assert_eq!(
        report["overview_app_histories"]["cpu"]["state"],
        "unavailable"
    );
    let mut expected = json!({"selected": snapshot.selection["selected"],
        "observations": snapshot.selection["trends"], "payloads": {"overview": report}});
    expected["payloads"]["overview"]["resident_history"] = json!({"state": "ready"});
    let error = payloads::validate(&snapshot, "overview", &expected).unwrap_err();
    assert!(error.contains("payload availability differs"));
}

#[test]
#[ignore = "opt-in finite recording measurement; requires recording and expected-output env vars"]
fn measures_complete_worker_routes_on_one_frozen_native_recording() {
    // Goal: expose actual worker costs and complete coverage separately from UI paint.
    // Method: run one selected route in this fresh process over immutable native input.
    let root = std::env::var_os("FLIGHTDECK_ITEM1_RECORDING").unwrap();
    let root = Path::new(&root);
    let expected = match std::env::var_os("FLIGHTDECK_ITEM1_EXPECTED_FILE") {
        Some(path) => {
            assert!(std::fs::metadata(&path).unwrap().len() <= 32 * 1024 * 1024);
            std::fs::read_to_string(path).unwrap()
        }
        None => std::env::var("FLIGHTDECK_ITEM1_EXPECTED_JSON").unwrap(),
    };
    assert!(expected.len() <= 32 * 1024 * 1024);
    let expected: Value = serde_json::from_str(&expected).unwrap();
    let route = std::env::var("FLIGHTDECK_ITEM1_ROUTE").unwrap();
    assert!(["network", "memory", "cpu", "apps", "overview"].contains(&route.as_str()));
    let phase = std::env::var("FLIGHTDECK_ITEM1_PHASE").unwrap();
    match phase.as_str() {
        "cold" | "correctness" => measure_route(root, &route, &expected),
        "raw" => raw::run(root, &route, &expected),
        _ => panic!("phase must be cold, correctness or raw"),
    }
}

fn measure_route(root: &Path, route: &str, expected: &Value) {
    let mut reader = SnapshotReader::new();
    reader.command.mode = crate::live_source_mode::SourceMode::Live;
    reader.command.request.route = route.to_owned();
    let selection = std::env::var_os("FLIGHTDECK_ITEM1_SELECTION")
        .map_or_else(|| "follow".into(), |value| value.into_string().unwrap());
    selection::configure(&mut reader, root, expected, &selection);
    diagnostics::checkpoint("before_read");
    let ownership_before = ownership::snapshot();
    let before = process_usage();
    let thread_before = thread_cpu_ns();
    let started = std::time::Instant::now();
    let work = persistence::work::observe();
    let ((snapshot, snapshots), allocation) =
        crate::allocation_tests::measure(|| read_complete(&mut reader, root));
    let wall_ns = started.elapsed().as_nanos();
    let thread_ns = thread_cpu_ns().checked_sub(thread_before).unwrap();
    let after = process_usage();
    let counts = work.counts();
    drop(work);
    let footprint = persistence::resources::Footprint::read().unwrap();
    let ownership_held = ownership::snapshot();
    diagnostics::checkpoint("holding_result");
    let phase = std::env::var("FLIGHTDECK_ITEM1_PHASE").unwrap();
    let payloads = validate_output(
        &snapshot,
        expected,
        &RoutePhase {
            route,
            phase: &phase,
        },
    );
    let report = json!({"route": route, "phase": phase, "root": root, "selection_kind": selection,
        "snapshots_to_complete": snapshots,
        "selected": snapshot.selection["selected"], "summary": snapshot.selection["summary"],
        "context_values": snapshot.selection["trends"], "pages": snapshot.pages,
        "effective_threads": {"maximum": std::env::var("POLARS_MAX_THREADS").unwrap(),
            "asynchronous": std::env::var("POLARS_ASYNC_THREAD_COUNT").unwrap()},
        "point_count": snapshot.selection["trends"].as_array().map(Vec::len),
        "context_mode": expected["context_mode"],
        "kept_observations": expected["kept_observations"],
        "payloads": payloads, "reader_complete": true,
        "wall_ns": wall_ns, "calling_thread_cpu_ns": thread_ns,
        "process_cpu_ns": after.user_ns + after.system_ns - before.user_ns - before.system_ns,
        "calling_thread_allocation_request_calls": allocation.calls,
        "calling_thread_allocation_requested_bytes": allocation.bytes,
        "ownership_before_read": ownership_before, "ownership_held_result": ownership_held,
        "retained_cache_charge": snapshot.catalog["tail_cache_bytes"],
        "physical_footprint_bytes": footprint.current_bytes,
        "physical_footprint_lifetime_max_bytes": footprint.lifetime_bytes_max,
        "work": {"staged_ticks": counts.staged_ticks, "frames": counts.frames,
            "sql_plans": counts.sql_plans, "collections": counts.collections,
            "table_string_cells": counts.string_cells}});
    emit(&report);
    drop(report);
    drop(snapshot);
    let after_result_drop = ownership::snapshot();
    diagnostics::checkpoint("after_result_drop");
    drop(reader);
    diagnostics::checkpoint("after_reader_drop");
    println!(
        "{}",
        json!({"route": route, "phase": "ownership_teardown",
        "ownership_after_result_drop": after_result_drop,
        "ownership_after_reader_drop": ownership::snapshot()})
    );
}

struct RoutePhase<'a> {
    route: &'a str,
    phase: &'a str,
}

fn validate_output(
    snapshot: &crate::live_source::Snapshot,
    expected: &Value,
    request: &RoutePhase<'_>,
) -> Value {
    let payloads = if request.phase == "correctness" {
        assert_eq!(
            std::env::var("FLIGHTDECK_ITEM1_ORACLE_BASELINE").unwrap(),
            "controlled-counterfactual"
        );
        payloads::validate_input(snapshot, expected).unwrap();
        payloads::report(snapshot).unwrap()
    } else {
        payloads::validate(snapshot, request.route, expected).unwrap()
    };
    assert!(!snapshot.histories_pending);
    if request.phase == "cold" {
        assert!(snapshot.catalog["collector_completed"].as_bool().unwrap());
    }
    assert!(
        snapshot.selection["selected"]["publication"]
            .as_u64()
            .unwrap()
            > 0
    );
    payloads
}

fn process_usage() -> collector::processes::Usage {
    collector::processes::read_usage(
        collector::clock::Timebase::read().unwrap(),
        i32::try_from(std::process::id()).unwrap(),
    )
    .unwrap()
    .unwrap()
}

fn emit(report: &Value) {
    let text = serde_json::to_string(report).unwrap();
    assert!(text.len() <= 32 * 1024 * 1024);
    if let Some(path) = std::env::var_os("FLIGHTDECK_ITEM1_OUTPUT") {
        use std::io::Write;
        let mut file =
            collector::recording::private::open_private_file(Path::new(&path), true).unwrap();
        file.write_all(text.as_bytes()).unwrap();
        file.sync_all().unwrap();
    } else {
        println!("{text}");
    }
}

fn read_complete(
    reader: &mut SnapshotReader,
    root: &Path,
) -> (Box<crate::live_source::Snapshot>, u32) {
    for count in 1..=4 {
        let snapshot = reader.read_at(root).unwrap().unwrap();
        if super::super::reader_published(&reader.reader, reader.command.generation) {
            assert!(!snapshot.histories_pending);
            return (snapshot, count);
        }
    }
    panic!("The finite worker route left deferred context or histories pending");
}

fn thread_cpu_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut time) };
    assert_eq!(status, 0, "{}", std::io::Error::last_os_error());
    u64::try_from(time.tv_sec).unwrap() * 1_000_000_000 + u64::try_from(time.tv_nsec).unwrap()
}

#[test]
fn derives_a_populated_store_second_without_adapter_allocations() {
    // Goal: shared reducers add no temporary collections to native Store ingestion.
    // Method: project populated real-ring CPU/network rows and explicit GPU states.
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish_resources();
    recording.publish_resources();
    let place =
        collector::ring::Place::for_user(recording.root.file_name().unwrap().to_str().unwrap())
            .unwrap();
    let mut reader = collector::ring::Reader::open(&place.lock_path).unwrap();
    let mut minute = viewer::recording::MachineMinute::default();
    for publication in 1..=2 {
        assert!(matches!(
            reader.read().unwrap(),
            collector::ring::Found::Publication { .. }
        ));
        minute.push_tick(publication, reader.tick());
        minute.ticks.last_mut().unwrap().performance_levels = Some([
            collector::system::PerformanceLevel {
                name: collector::system::FactText::new(b"Fast").unwrap(),
                logical_cpus: 1,
            },
            collector::system::PerformanceLevel {
                name: collector::system::FactText::new(b"Slow").unwrap(),
                logical_cpus: 0,
            },
        ]);
        minute.gpu_states.push(viewer::recording::GpuStateRow {
            publication,
            state: 0,
            active_ns: publication * 100_000_000,
        });
    }
    let mut readings = viewer::store::readings(&minute);
    let previous = readings.next().unwrap();
    let current = readings.next().unwrap();

    let (second, allocation) = crate::allocation_tests::measure(|| {
        viewer::store::MachineSecond::of(&current, Some(&previous)).unwrap()
    });

    assert_eq!(second.cpu_percent, Some(50.0));
    assert_eq!(second.cpu_percent_one_core, Some(50.0));
    assert_eq!(second.cpu_level_percent, [Some(50.0), None]);
    assert_eq!(second.gpu_active_percent, Some(10.0));
    assert_eq!(
        second.network.unwrap().received_bps.to_bits(),
        1.0_f64.to_bits()
    );
    assert_eq!(allocation.calls, 0);
    assert_eq!(allocation.bytes, 0);
}

#[test]
fn copies_native_readings_exactly_and_refuses_json_past_one_mibibyte() {
    // Goal: native capture preserves Copy JSON's exact values and inclusive 1 MiB limit.
    // Method: copy a real Worker snapshot, then grow one value to the boundary and past it.
    let mut fixture = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    fixture.publish_resources();
    fixture.publish_resources();
    let mut reader = SnapshotReader::new();
    reader.command.mode = crate::live_source_mode::SourceMode::Live;
    let mut snapshot = reader.read_at(&fixture.root).unwrap().unwrap();
    let copy = |snapshot: &crate::live_source::Snapshot| {
        crate::native_workflows::selected_copy(Some(snapshot), &reader.command.request, None)
    };

    let original = copy(&snapshot).unwrap();
    let original: serde_json::Value = serde_json::from_str(&original).unwrap();
    assert_eq!(
        original["summary"]["memory_total_bytes"].as_u64(),
        Some(9_007_199_254_740_993)
    );
    assert_eq!(original["observation"], snapshot.selection["selected"]);
    snapshot.selection["summary"]["padding"] = json!("");
    let overhead = copy(&snapshot).unwrap().len();
    snapshot.selection["summary"]["padding"] = json!("x".repeat(1_048_576 - overhead));
    let boundary = copy(&snapshot).unwrap();
    assert_eq!(boundary.len(), 1_048_576);
    let oversized = format!(
        "{}x",
        snapshot.selection["summary"]["padding"].as_str().unwrap()
    );
    snapshot.selection["summary"]["padding"] = json!(oversized);

    assert_eq!(
        copy(&snapshot).unwrap_err(),
        "Selected details exceed the 1 MiB clipboard limit"
    );
}
