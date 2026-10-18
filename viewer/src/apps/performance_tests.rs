use collector::clock::Timebase;
use serde_json::json;
use std::time::Instant;

use super::tests::{interval, observation, usage};
use super::*;

#[test]
#[ignore = "opt-in release fixture observation; no timing assertion"]
fn measures_twelve_thousand_processes_across_two_hundred_fifty_six_apps() {
    // Goal: observe the actual bounded aggregate API's populated release cost.
    // Method: aggregate 12,000 recorded-shaped processes across 256 Apps 100 times.
    let interval = interval();
    let paths: Vec<_> = (0..256)
        .map(|index| format!("/Applications/Fixture {index}.app"))
        .collect();
    let programs =
        measures_twelve_thousand_processes_across_two_hundred_fifty_six_apps_programs(&paths);
    let mut before_usage = usage(9_007_199_254_740_993, 9_007_199_254_741_993, 1000);
    before_usage.disk_read_bytes = 9_007_199_254_740_993;
    before_usage.disk_written_bytes = 9_007_199_254_741_993;
    let mut selected_usage = usage(9_007_199_254_741_093, 9_007_199_254_742_193, 2000);
    selected_usage.disk_read_bytes = before_usage.disk_read_bytes + 4096;
    selected_usage.disk_written_bytes = before_usage.disk_written_bytes + 1024;
    let mut before = Vec::with_capacity(12_000);
    let mut selected = Vec::with_capacity(12_000);
    for index in 0..12_000 {
        let mut previous = observation(&interval.previous, &programs[index % 256], &before_usage);
        previous.pid = i32::try_from(index + 1).unwrap();
        previous.creation.as_mut().unwrap().unique_id = u64::try_from(index + 1).unwrap();
        before.push(previous);
        let mut current = observation(&interval.selected, &programs[index % 256], &selected_usage);
        current.pid = i32::try_from(index + 1).unwrap();
        current.creation.as_mut().unwrap().unique_id = u64::try_from(index + 1).unwrap();
        selected.push(current);
    }
    let spans = measures_twelve_thousand_processes_across_two_hundred_fifty_six_apps_intervals(
        &before, &selected,
    );
    let mut result = tests::aggregate();
    let timebase = Timebase::read().unwrap();
    let pid = i32::try_from(std::process::id()).unwrap();
    let footprint_before = persistence::resources::Footprint::read().unwrap();
    let usage_before = collector::processes::read_usage(timebase, pid)
        .unwrap()
        .unwrap();
    let started = Instant::now();

    for _index in 0..100 {
        result.compute(&interval, &spans).unwrap();
    }
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap();
    let usage_after = collector::processes::read_usage(timebase, pid)
        .unwrap()
        .unwrap();
    let footprint_after = persistence::resources::Footprint::read().unwrap();

    assert_populated(&result);
    report(
        &paths,
        &result,
        &CostObservation {
            elapsed_ns,
            process_cpu_ns: usage_after.user_ns - usage_before.user_ns + usage_after.system_ns
                - usage_before.system_ns,
            footprint_before_bytes: footprint_before.current_bytes,
            footprint_after_bytes: footprint_after.current_bytes,
            footprint_lifetime_max_bytes: footprint_after.lifetime_bytes_max,
        },
    );
}

fn measures_twelve_thousand_processes_across_two_hundred_fifty_six_apps_programs(
    paths: &[String],
) -> Vec<ProgramBytes<'_>> {
    paths
        .iter()
        .map(|path| ProgramBytes {
            executable_name: b"helper",
            app_path: Some(path.as_bytes()),
        })
        .collect()
}

fn measures_twelve_thousand_processes_across_two_hundred_fifty_six_apps_intervals<'a>(
    before: &'a [Box<ProcessObservation<'a>>],
    selected: &'a [Box<ProcessObservation<'a>>],
) -> Vec<ProcessInterval<'a>> {
    before
        .iter()
        .zip(selected)
        .map(|(previous, selected)| ProcessInterval {
            previous: Some(previous),
            selected,
            start: None,
        })
        .collect()
}

struct CostObservation {
    elapsed_ns: u64,
    process_cpu_ns: u64,
    footprint_before_bytes: u64,
    footprint_after_bytes: u64,
    footprint_lifetime_max_bytes: u64,
}

fn report(paths: &[String], result: &Aggregate, cost: &CostObservation) {
    println!(
        "{}",
        json!({
            "fixture_only": true, "process_rows": 12000, "app_rows": 256, "operations": 100,
            "input_endpoint_program_bytes": program_bytes(paths), "elapsed_ns": cost.elapsed_ns,
            "process_cpu_ns": cost.process_cpu_ns,
            "physical_footprint_before_bytes": cost.footprint_before_bytes,
            "physical_footprint_after_bytes": cost.footprint_after_bytes,
            "physical_footprint_lifetime_max_bytes": cost.footprint_lifetime_max_bytes,
            "process_result_capacity": result.processes.capacity(),
            "process_result_size": size_of::<ProcessTotals<'_>>(),
            "app_result_size": size_of::<AppTotals>(),
            "disk_read_delta_per_process": 4096, "disk_written_delta_per_process": 1024,
        })
    );
}

fn program_bytes(paths: &[String]) -> usize {
    (0..12_000)
        .map(|index| 2 * (6 + paths[index % 256].len()))
        .sum()
}

fn assert_populated(result: &Aggregate) {
    assert_eq!(result.processes.len(), 12_000);
    assert_eq!(result.apps.len(), 256);
    assert!(result.membership_complete);
}
