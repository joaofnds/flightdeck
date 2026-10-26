use std::path::Path;

use serde_json::{Value, json};

use super::{SealedRecording, SnapshotReader};

#[path = "live_native_measurement_payloads.rs"]
mod payloads;

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
    // Method: run each route cold on an externally completed, immutable fixture source.
    let root = std::env::var_os("FLIGHTDECK_ITEM1_RECORDING").unwrap();
    let root = Path::new(&root);
    let expected = std::env::var("FLIGHTDECK_ITEM1_EXPECTED_JSON").unwrap();
    assert!(expected.len() <= 1024 * 1024);
    let expected: Value = serde_json::from_str(&expected).unwrap();
    for route in ["network", "memory", "cpu", "apps", "overview"] {
        measure_route(root, route, &expected);
    }
}

fn measure_route(root: &Path, route: &str, expected: &Value) {
    let mut reader = SnapshotReader::new();
    reader.command.mode = crate::live_source_mode::SourceMode::Live;
    reader.command.request.route = route.to_owned();
    let process = || {
        collector::processes::read_usage(
            collector::clock::Timebase::read().unwrap(),
            i32::try_from(std::process::id()).unwrap(),
        )
        .unwrap()
        .unwrap()
    };
    let before = process();
    let thread_before = thread_cpu_ns();
    let started = std::time::Instant::now();
    let work = persistence::work::observe();
    let ((snapshot, snapshots), allocation) =
        crate::allocation_tests::measure(|| read_complete(&mut reader, root));
    let wall_ns = started.elapsed().as_nanos();
    let thread_ns = thread_cpu_ns().checked_sub(thread_before).unwrap();
    let after = process();
    let counts = work.counts();
    drop(work);
    let footprint = persistence::resources::Footprint::read().unwrap();
    let payloads = payloads::validate(&snapshot, route, expected).unwrap();
    assert!(!snapshot.histories_pending);
    assert!(snapshot.catalog["collector_completed"].as_bool().unwrap());
    assert!(
        snapshot.selection["selected"]["publication"]
            .as_u64()
            .unwrap()
            > 0
    );
    let report = json!({"route": route, "root": root, "snapshots_to_complete": snapshots,
        "selected": snapshot.selection["selected"], "summary": snapshot.selection["summary"],
        "point_count": snapshot.selection["trends"].as_array().unwrap().len(),
        "payloads": payloads, "reader_complete": true,
        "wall_ns": wall_ns, "worker_thread_cpu_ns": thread_ns,
        "process_cpu_ns": after.user_ns + after.system_ns - before.user_ns - before.system_ns,
        "allocation_request_calls": allocation.calls,
        "allocation_requested_bytes": allocation.bytes,
        "retained_cache_charge": snapshot.catalog["tail_cache_bytes"],
        "physical_footprint_bytes": footprint.current_bytes,
        "physical_footprint_lifetime_max_bytes": footprint.lifetime_bytes_max,
        "work": {"staged_ticks": counts.staged_ticks, "frames": counts.frames,
            "sql_plans": counts.sql_plans, "collections": counts.collections,
            "table_string_cells": counts.string_cells}});
    let text = serde_json::to_string(&report).unwrap();
    assert!(text.len() <= 1024 * 1024);
    println!("{text}");
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
