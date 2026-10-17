#[path = "../benchmark_helpers.rs"]
mod benchmark_helpers;
#[path = "../replay.rs"]
pub mod replay;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use benchmark_helpers::{OwnedChild, canonical, hash, logs, wall_ms, write_json};
use serde_json::{Value, json};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("benchmark_runner: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Default)]
struct Options {
    gpui: PathBuf,
    swift: PathBuf,
    actions: PathBuf,
    output: PathBuf,
    watcher: PathBuf,
    pairs: u16,
    warmup_ms: u64,
    sample_seconds: u16,
    gpui_animate: bool,
}

struct Workload {
    duration_ms: u64,
    actions: u64,
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut options = Options::parse()?;
    let replay = replay::Replay::load(&options.actions)?;
    if replay.shows_hardware() {
        options.gpui_animate = true;
    }
    let workload = Workload {
        duration_ms: replay.duration_ms(),
        actions: replay.report()["actions"]
            .as_u64()
            .ok_or("Missing action count")?,
    };
    let required = options.warmup_ms + u64::from(options.sample_seconds) * 1000 + 2000;
    if workload.duration_ms < required {
        return Err("Replay duration must cover warmup, observation and 2000 ms grace".into());
    }
    fs::create_dir(&options.output)?;
    fs::copy(&options.actions, options.output.join("actions.json"))?;
    options.actions = canonical(options.output.join("actions.json"))?;
    let manifest = manifest(&options)?;
    write_json(&options.output.join("manifest.json"), &manifest)?;
    let mut baseline = None;
    for pair in 0..options.pairs {
        let order = if pair.is_multiple_of(2) {
            [("gpui", &options.gpui), ("swift", &options.swift)]
        } else {
            [("swift", &options.swift), ("gpui", &options.gpui)]
        };
        for (name, executable) in order {
            if manifest[name]["sha256"].as_str() != Some(hash(executable)?.as_str()) {
                return Err(format!("{name} executable changed during the benchmark").into());
            }
            let trace = measure(&options, &workload, pair, name, executable)?;
            if let Some(expected) = &baseline {
                if *expected != trace {
                    return Err(
                        format!("{name} pair {pair} executed a different effective trace").into(),
                    );
                }
            } else {
                baseline = Some(trace);
            }
        }
    }
    println!("{}", options.output.display());
    Ok(())
}

impl Options {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let arguments: Vec<_> = std::env::args_os().skip(1).collect();
        if arguments.len() > 18 {
            return Err("Too many benchmark arguments".into());
        }
        let mut paths = [None, None, None, None, None];
        let mut options = Self {
            pairs: 3,
            warmup_ms: 5000,
            sample_seconds: 15,
            gpui_animate: false,
            ..Self::default()
        };
        let mut arguments = arguments.iter().peekable();
        for _attempt in 0..18 {
            let Some(flag) = arguments.next() else { break };
            let flag = flag.to_str().ok_or("Invalid benchmark flag")?;
            if flag == "--gpui-animate" {
                options.gpui_animate = true;
                if let Some(value) = arguments.peek() {
                    if matches!(value.to_str(), Some("true" | "false")) {
                        let consumed = arguments.next().ok_or("Missing animation value")?;
                        options.gpui_animate = consumed.to_str() == Some("true");
                    }
                }
                continue;
            }
            let value = arguments.next().ok_or("Benchmark flag needs a value")?;
            match flag {
                "--gpui" => paths[0] = Some(PathBuf::from(value)),
                "--swift" => paths[1] = Some(PathBuf::from(value)),
                "--actions" => paths[2] = Some(PathBuf::from(value)),
                "--output" => paths[3] = Some(PathBuf::from(value)),
                "--watcher" => paths[4] = Some(PathBuf::from(value)),
                "--pairs" => {
                    options.pairs = value.to_str().ok_or("Invalid pairs")?.parse()?;
                }
                "--warmup-ms" => {
                    options.warmup_ms = value.to_str().ok_or("Invalid warmup")?.parse()?;
                }
                "--sample-seconds" => {
                    options.sample_seconds = value.to_str().ok_or("Invalid duration")?.parse()?;
                }
                _ => return Err(format!("Unknown benchmark flag {flag}").into()),
            }
        }
        options.gpui = canonical(paths[0].take().ok_or("--gpui EXE is required")?)?;
        options.swift = canonical(paths[1].take().ok_or("--swift EXE is required")?)?;
        options.actions = canonical(paths[2].take().ok_or("--actions PATH is required")?)?;
        options.output = paths[3].take().ok_or("--output DIR is required")?;
        options.watcher = canonical(paths[4].take().ok_or("--watcher EXE is required")?)?;
        options.validate()?;
        Ok(options)
    }

    fn validate(&self) -> Result<(), &'static str> {
        if !(1..=10).contains(&self.pairs) {
            return Err("Pairs must be 1..10");
        }
        if self.warmup_ms > 60_000 {
            return Err("Warmup must be 0..60000 ms");
        }
        if !(15..=600).contains(&self.sample_seconds) {
            return Err("Sample duration must be 15..600 seconds");
        }
        Ok(())
    }
}

fn manifest(options: &Options) -> io::Result<Value> {
    Ok(json!({"schema":1,"started_wall_ms":wall_ms()?.to_string(),
        "gpui":{"path":options.gpui,"sha256":hash(&options.gpui)?},
        "swift":{"path":options.swift,"sha256":hash(&options.swift)?},
        "actions":{"path":options.actions,"sha256":hash(&options.actions)?},
        "watcher":{"path":options.watcher,"sha256":hash(&options.watcher)?},
        "pairs":options.pairs,"warmup_ms":options.warmup_ms,
        "sample_seconds":options.sample_seconds,
        "gpui_animate":options.gpui_animate,
        "scope":"Captured model-action replay; CPU acceptance and process resources",
        "limitations":["No OS input or actual presentation measurement",
            "Visibility, viewport, display, thermal state and source revision require recording",
            "First replay acceptance is not first presented frame"]}))
}

fn measure(
    options: &Options,
    workload: &Workload,
    pair: u16,
    name: &str,
    executable: &Path,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let label = format!("pair-{pair:02}-{name}");
    let app_logs = logs(&options.output, &format!("{label}-app"));
    let launched_wall_ms = wall_ms()?;
    let started = Instant::now();
    let mut command = app_command(options, name, executable);
    let arguments: Vec<_> = command
        .get_args()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    let mut app = OwnedChild::start(&mut command, &app_logs)?;
    write_json(
        &options.output.join(format!("{label}-start.json")),
        &json!({
        "pid":app.id(),"implementation":name,"pair":pair,
        "launched_wall_ms":launched_wall_ms.to_string(),"executable":executable,
        "arguments":arguments}),
    )?;
    let ready_after = benchmark_helpers::wait_ready(&mut app, &app_logs.stdout)?;
    benchmark_helpers::warmup(&mut app, Duration::from_millis(options.warmup_ms))?;
    let observed_wall_ms = wall_ms()?;
    let watcher_logs = logs(&options.output, &format!("{label}-process"));
    let mut watch = Command::new(&options.watcher);
    watch
        .arg("--pid")
        .arg(app.id().to_string())
        .arg("--seconds")
        .arg(options.sample_seconds.to_string());
    let mut watcher = OwnedChild::start(&mut watch, &watcher_logs)?;
    let status = watcher.wait(Duration::from_secs(u64::from(options.sample_seconds) + 15))?;
    if !status.success() {
        return Err(format!("Process watcher failed: {status}").into());
    }
    let remaining =
        Duration::from_millis(workload.duration_ms + 15_000).saturating_sub(started.elapsed());
    let status = app.wait(remaining)?;
    let report = json!({"schema":1,"pair":pair,"implementation":name,"pid":app.id(),
        "launched_wall_ms":launched_wall_ms.to_string(),
        "observed_wall_ms":observed_wall_ms.to_string(),
        "finished_wall_ms":wall_ms()?.to_string(),"elapsed_ms":started.elapsed().as_millis(),
        "replay_ready_after_ms":ready_after.as_millis(),"app_status":status.to_string(),
        "app_executable":executable,"app_arguments":arguments,
        "watcher_command":[options.watcher.display().to_string(),"--pid".to_owned(),
            app.id().to_string(),"--seconds".to_owned(),options.sample_seconds.to_string()]});
    write_json(&options.output.join(format!("{label}-run.json")), &report)?;
    if !status.success() {
        return Err(format!("{name} replay failed: {status}").into());
    }
    let trace = benchmark_helpers::trace(&app_logs.stdout, workload.actions)?;
    write_json(
        &options.output.join(format!("{label}-effective.json")),
        &json!(trace),
    )?;
    Ok(trace)
}

fn app_command(options: &Options, name: &str, executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("--reference")
        .arg("--replay")
        .arg(&options.actions);
    if name == "gpui" {
        if options.gpui_animate {
            command.arg("--animate").arg("--hardware");
        }
    }
    command
}
