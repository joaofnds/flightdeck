#[path = "../benchmark_helpers.rs"]
pub mod benchmark_helpers;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use benchmark_helpers::{LOG_LINES_MAX, read_log, write_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("benchmark_report: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Deserialize)]
struct Header {
    app_status: String,
    pid: u32,
    implementation: String,
    pair: u16,
}

#[derive(Deserialize)]
struct Sample {
    pid: u32,
    creation_awake_ns: String,
    elapsed_seconds: f64,
    cpu_seconds: f64,
    physical_footprint_bytes: u64,
    resident_bytes: u64,
    idle_wakeups: String,
    interrupt_wakeups: String,
    energy_nj: String,
}

#[derive(Serialize)]
struct Run {
    pid: u32,
    implementation: String,
    pair: u16,
    metadata_path: PathBuf,
    raw_process: PathBuf,
    raw_app: PathBuf,
    process_samples: usize,
    acceptance: Value,
    metrics: BTreeMap<String, f64>,
}

#[derive(Clone, Copy)]
struct AcceptanceWindow {
    start_ms: u64,
    end_ms: u64,
}

const RUNS_MAX: usize = 20;
const _: () = assert!(RUNS_MAX <= 256);

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 4 {
        return Err("benchmark_report --input DIRECTORY --output NEW_FILE".into());
    }
    if arguments[0] != "--input" {
        return Err("Expected --input DIRECTORY".into());
    }
    if arguments[2] != "--output" {
        return Err("Expected --output NEW_FILE".into());
    }
    let input = PathBuf::from(&arguments[1]);
    let manifest: Value = serde_json::from_str(&read_log(&input.join("manifest.json"))?)?;
    let window = acceptance_window(&manifest)?;
    let mut runs = Vec::with_capacity(RUNS_MAX);
    let mut excluded = Vec::with_capacity(RUNS_MAX);
    let mut identities = BTreeSet::new();
    for path in paths(&input)? {
        let header: Header = serde_json::from_str(&read_log(&path)?)?;
        if header.app_status != "exit status: 0" {
            excluded.push(json!({"raw_run":path,"reason":header.app_status}));
            continue;
        }
        if !identities.insert((header.implementation.clone(), header.pair)) {
            return Err("Duplicate implementation/pair run".into());
        }
        runs.push(report(path, &header, window)?);
    }
    if runs.is_empty() {
        return Err("No successful benchmark runs".into());
    }
    write_json(
        Path::new(&arguments[3]),
        &json!({"schema":1,"runs":runs,
        "per_implementation":aggregate(&runs),"excluded_runs":excluded,
        "acceptance_filter":{"scheduled_at_ms_start_inclusive":window.start_ms,
            "scheduled_at_ms_end_exclusive":window.end_ms,"quantiles":"nearest-rank"},
        "metric":"Process resources and identified CPU model timing; not presentation",
        "limitations":["Descriptive pair ranges; no confidence intervals",
            "Does not establish matched pixels or a framework performance verdict"]}),
    )?;
    Ok(())
}

fn acceptance_window(manifest: &Value) -> Result<AcceptanceWindow, String> {
    let start_ms = manifest["warmup_ms"]
        .as_u64()
        .ok_or("Missing benchmark warmup")?;
    let sample_ms = manifest["sample_seconds"]
        .as_u64()
        .ok_or("Missing sample duration")?
        .checked_mul(1000)
        .ok_or("Sample duration overflows")?;
    if sample_ms == 0 {
        return Err("Empty sample duration".into());
    }
    Ok(AcceptanceWindow {
        start_ms,
        end_ms: start_ms
            .checked_add(sample_ms)
            .ok_or("Sample window overflows")?,
    })
}

fn paths(input: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut paths = Vec::with_capacity(RUNS_MAX);
    for (index, entry) in fs::read_dir(input)?.take(257).enumerate() {
        if index == 256 {
            return Err("Benchmark directory exceeds 256 entries".into());
        }
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-run.json"))
        {
            if paths.len() == RUNS_MAX {
                return Err("Benchmark exceeds 20 runs".into());
            }
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn lines(path: &Path) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let text = read_log(path)?;
    let mut values = Vec::new();
    for (index, line) in text.lines().take(LOG_LINES_MAX + 1).enumerate() {
        if index == LOG_LINES_MAX {
            return Err("Benchmark log line bound exceeded".into());
        }
        if !line.trim().is_empty() {
            values.push(serde_json::from_str(line)?);
        }
    }
    Ok(values)
}

fn report(
    path: PathBuf,
    header: &Header,
    window: AcceptanceWindow,
) -> Result<Run, Box<dyn std::error::Error>> {
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix("-run.json"))
        .ok_or("Invalid run filename")?;
    let parent = path.parent().ok_or("Missing run directory")?;
    let process = parent.join(format!("{label}-process.jsonl"));
    let app = parent.join(format!("{label}-app.jsonl"));
    let samples: Vec<Sample> = lines(&process)?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()?;
    let mut metrics = resources(&samples, header.pid)?;
    let acceptance = acceptance(&lines(&app)?, &mut metrics, window)?;
    Ok(Run {
        pid: header.pid,
        implementation: header.implementation.clone(),
        pair: header.pair,
        metadata_path: path,
        raw_process: process,
        raw_app: app,
        process_samples: samples.len(),
        acceptance,
        metrics,
    })
}

fn resources(samples: &[Sample], pid: u32) -> Result<BTreeMap<String, f64>, String> {
    if samples.len() < 2 {
        return Err("At least two process samples are required".into());
    }
    for sample in samples {
        if sample.pid != pid {
            return Err("Observed process PID changed".into());
        }
        for value in [sample.elapsed_seconds, sample.cpu_seconds] {
            if !value.is_finite() {
                return Err("Nonfinite process counter".into());
            }
            if value < 0.0 {
                return Err("Negative process counter".into());
            }
        }
        sample
            .creation_awake_ns
            .parse::<u64>()
            .map_err(|error| error.to_string())?;
    }
    for pair in samples.windows(2) {
        validate_pair(&pair[0], &pair[1])?;
    }
    let first = &samples[0];
    let last = &samples[samples.len() - 1];
    let seconds = last.elapsed_seconds - first.elapsed_seconds;
    let maximum = samples
        .iter()
        .map(|sample| sample.physical_footprint_bytes)
        .max()
        .ok_or("Missing process samples")?;
    let mut values = BTreeMap::new();
    for (name, value) in [
        (
            "cpu_percent_one_core",
            (last.cpu_seconds - first.cpu_seconds) / seconds * 100.0,
        ),
        ("elapsed_seconds", seconds),
        (
            "footprint_mib_first",
            real(first.physical_footprint_bytes)? / 1_048_576.0,
        ),
        (
            "footprint_mib_end",
            real(last.physical_footprint_bytes)? / 1_048_576.0,
        ),
        ("footprint_mib_max", real(maximum)? / 1_048_576.0),
        ("resident_mib_end", real(last.resident_bytes)? / 1_048_576.0),
        (
            "idle_wakeups_per_second",
            counter_delta(&first.idle_wakeups, &last.idle_wakeups)? / seconds,
        ),
        (
            "interrupt_wakeups_per_second",
            counter_delta(&first.interrupt_wakeups, &last.interrupt_wakeups)? / seconds,
        ),
    ] {
        if !value.is_finite() {
            return Err("Nonfinite process statistic".into());
        }
        values.insert(name.into(), value);
    }
    Ok(values)
}

fn validate_pair(first: &Sample, last: &Sample) -> Result<(), String> {
    if first.creation_awake_ns != last.creation_awake_ns {
        return Err("Observed process creation identity changed".into());
    }
    if last.elapsed_seconds <= first.elapsed_seconds {
        return Err("Process sample elapsed delta is not positive".into());
    }
    if last.cpu_seconds < first.cpu_seconds {
        return Err("Process CPU counter decreased".into());
    }
    for (before, after) in [
        (&first.idle_wakeups, &last.idle_wakeups),
        (&first.interrupt_wakeups, &last.interrupt_wakeups),
        (&first.energy_nj, &last.energy_nj),
    ] {
        counter_delta(before, after)?;
    }
    Ok(())
}

fn counter_delta(first: &str, last: &str) -> Result<f64, String> {
    let first = first.parse::<u64>().map_err(|error| error.to_string())?;
    let last = last.parse::<u64>().map_err(|error| error.to_string())?;
    real(last.checked_sub(first).ok_or("Process counter decreased")?)
}

fn real(value: u64) -> Result<f64, String> {
    value
        .to_string()
        .parse()
        .map_err(|error: std::num::ParseFloatError| error.to_string())
}

fn acceptance(
    lines: &[Value],
    metrics: &mut BTreeMap<String, f64>,
    window: AcceptanceWindow,
) -> Result<Value, String> {
    let mut values = Vec::new();
    let mut action_count = 0_u64;
    let mut timing_schema = None;
    for line in lines {
        if line["event"] != "replay_action" {
            continue;
        }
        validate_timing_schema(line, &mut timing_schema)?;
        let scheduled = line["scheduled_at_ms"]
            .as_u64()
            .ok_or("Missing scheduled action time")?;
        let accepted = line["accepted_at_ms"]
            .as_u64()
            .ok_or("Missing accepted action time")?;
        let lateness = accepted
            .checked_sub(scheduled)
            .ok_or("Action accepted before schedule")?;
        if scheduled >= window.start_ms {
            if scheduled < window.end_ms {
                values.push(real(lateness)?);
            }
        }
        action_count += 1;
    }
    let summary = lines
        .iter()
        .find(|line| line["event"] == "replay_summary")
        .ok_or("Missing replay summary")?;
    if summary["actions"].as_u64() != Some(action_count) {
        return Err("Replay action log is incomplete".into());
    }
    if summary["accepted"].as_u64() != Some(action_count) {
        return Err("Replay did not accept every action".into());
    }
    if values.is_empty() {
        return Ok(Value::Null);
    }
    values.sort_by(f64::total_cmp);
    let prefix = if timing_schema == Some(2) {
        "cpu_completion_lateness_ms"
    } else {
        "cpu_batch_dispatch_lateness_ms"
    };
    for (name, percent) in [("p50", 50), ("p95", 95), ("p99", 99), ("max", 100)] {
        let index = (values.len() * percent).div_ceil(100).saturating_sub(1);
        metrics.insert(format!("{prefix}_{name}"), values[index]);
    }
    Ok(
        json!({"actions":values.len(),"scheduled_at_ms_min":window.start_ms,
        "scheduled_at_ms_end_exclusive":window.end_ms,
        "timing_schema":timing_schema,"metric":format!("{prefix}; not presentation")}),
    )
}

fn validate_timing_schema(line: &Value, previous: &mut Option<u64>) -> Result<(), String> {
    let schema = line["schema"]
        .as_u64()
        .ok_or("Missing action timing schema")?;
    if ![1, 2].contains(&schema) {
        return Err("Unknown action timing schema".into());
    }
    if let Some(previous) = *previous {
        if previous != schema {
            return Err("Mixed action timing schemas".into());
        }
    }
    *previous = Some(schema);
    Ok(())
}

fn aggregate(runs: &[Run]) -> Value {
    let mut groups: BTreeMap<&str, BTreeMap<&str, Vec<f64>>> = BTreeMap::new();
    for run in runs {
        let group = groups.entry(&run.implementation).or_default();
        for (name, value) in &run.metrics {
            group.entry(name).or_default().push(*value);
        }
    }
    let mut result = serde_json::Map::new();
    for (implementation, metrics) in groups {
        let mut statistics = serde_json::Map::new();
        for (name, mut values) in metrics {
            values.sort_by(f64::total_cmp);
            let middle = values.len() / 2;
            let median = if values.len().is_multiple_of(2) {
                values[middle - 1] / 2.0 + values[middle] / 2.0
            } else {
                values[middle]
            };
            statistics.insert(
                name.into(),
                json!({"count":values.len(),"median":median,
                "min":values[0],"max":values[values.len()-1]}),
            );
        }
        result.insert(implementation.into(), Value::Object(statistics));
    }
    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> [Sample; 2] {
        [
            Sample {
                pid: 42,
                creation_awake_ns: "123".into(),
                elapsed_seconds: 10.0,
                cpu_seconds: 2.0,
                physical_footprint_bytes: 1_048_576,
                resident_bytes: 3_145_728,
                idle_wakeups: "100".into(),
                interrupt_wakeups: "200".into(),
                energy_nj: "1000".into(),
            },
            Sample {
                pid: 42,
                creation_awake_ns: "123".into(),
                elapsed_seconds: 20.0,
                cpu_seconds: 4.0,
                physical_footprint_bytes: 2_097_152,
                resident_bytes: 4_194_304,
                idle_wakeups: "140".into(),
                interrupt_wakeups: "230".into(),
                energy_nj: "2000".into(),
            },
        ]
    }

    #[test]
    fn measures_counter_deltas_over_the_observed_interval() {
        // Literal counters establish 20% of one core and four idle wakeups per second.
        let metrics = resources(&samples(), 42).unwrap();

        assert!((metrics["cpu_percent_one_core"] - 20.0).abs() < f64::EPSILON);
        assert!((metrics["idle_wakeups_per_second"] - 4.0).abs() < f64::EPSILON);
        assert!((metrics["footprint_mib_end"] - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rejects_a_counter_reset_inside_the_observation() {
        // A middle/reset sample must invalidate the report, rather than conceal its decrease.
        let mut values = samples();
        values[1].idle_wakeups = "99".into();

        assert!(resources(&values, 42).is_err());
    }

    #[test]
    fn rejects_a_cpu_counter_reset() {
        // A negative CPU delta invalidates the observation instead of becoming a utilization rate.
        let mut values = samples();
        values[1].cpu_seconds = 1.0;

        assert!(resources(&values, 42).is_err());
    }

    #[test]
    fn rejects_reused_process_identity() {
        // The same PID cannot join counters from two different process creations.
        let mut values = samples();
        values[1].creation_awake_ns = "124".into();

        assert!(resources(&values, 42).is_err());
    }

    #[test]
    fn rejects_nonpositive_elapsed_time() {
        // Equal observation times cannot be used as a rate denominator.
        let mut values = samples();
        values[1].elapsed_seconds = 10.0;

        assert!(resources(&values, 42).is_err());
    }

    #[test]
    fn filters_warmup_actions_before_computing_acceptance_quantiles() {
        // The 900 ms warmup delay must not inflate the two observed action delays.
        let values = [
            json!({"event":"replay_action","schema":2,"scheduled_at_ms":0,"accepted_at_ms":900}),
            json!({"event":"replay_action","schema":2,
                "scheduled_at_ms":19900,"accepted_at_ms":20800}),
            json!({"event":"replay_action","schema":2,
                "scheduled_at_ms":20000,"accepted_at_ms":20010}),
            json!({"event":"replay_action","schema":2,
                "scheduled_at_ms":20100,"accepted_at_ms":20130}),
            json!({"event":"replay_action","schema":2,
                "scheduled_at_ms":21000,"accepted_at_ms":21900}),
            json!({"event":"replay_summary","actions":5,"accepted":5}),
        ];
        let mut metrics = BTreeMap::new();

        let report = acceptance(
            &values,
            &mut metrics,
            AcceptanceWindow {
                start_ms: 20_000,
                end_ms: 21_000,
            },
        )
        .unwrap();

        assert_eq!(report["actions"], 2);
        assert!((metrics["cpu_completion_lateness_ms_p50"] - 10.0).abs() < f64::EPSILON);
        assert!((metrics["cpu_completion_lateness_ms_p99"] - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn legacy_dispatch_records_never_claim_completed_action_latency() {
        // Schema one sampled the timer before applying actions and must retain that limitation.
        let values = [
            json!({"event":"replay_action","schema":1,
            "scheduled_at_ms":10,"accepted_at_ms":20}),
            json!({"event":"replay_summary","actions":1,"accepted":1}),
        ];
        let mut metrics = BTreeMap::new();
        acceptance(
            &values,
            &mut metrics,
            AcceptanceWindow {
                start_ms: 0,
                end_ms: 30,
            },
        )
        .unwrap();
        assert!(metrics.contains_key("cpu_batch_dispatch_lateness_ms_p50"));
        assert!(!metrics.contains_key("cpu_completion_lateness_ms_p50"));
    }
}
