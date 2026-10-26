//! Measures the real store's complete resume handoff over an existing recording.
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use collector::clock::Timebase;
use collector::processes::{Usage, read_usage};
use persistence::resources::Footprint;
use serde_json::json;
use viewer::store::{Store, View};

#[path = "resume_bench/fixture_control.rs"]
mod fixture_control;
#[path = "resume_bench/frozen.rs"]
mod frozen;

const WAIT_MAX: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(1);
const POLLS_MAX: u32 = 120_000;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("resume_bench: {error}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    recording: PathBuf,
    minutes: u64,
    iterations: u32,
    gated: bool,
}

struct Meter {
    timebase: Timebase,
    pid: i32,
}

impl Options {
    fn parse() -> io::Result<Self> {
        let words: Vec<_> = std::env::args().skip(1).take(8).collect();
        if !(6..=7).contains(&words.len()) {
            return Err(io::Error::other(
                "--recording PATH --minutes N --iterations N",
            ));
        }
        if words[0] != "--recording" {
            return Err(io::Error::other("expected --recording"));
        }
        if words[2] != "--minutes" {
            return Err(io::Error::other("expected --minutes"));
        }
        if words[4] != "--iterations" {
            return Err(io::Error::other("expected --iterations"));
        }
        let minutes = words[3].parse().map_err(io::Error::other)?;
        let iterations = words[5].parse().map_err(io::Error::other)?;
        let gated = match words.get(6).map(String::as_str) {
            None => false,
            Some("--gated") => true,
            Some(_) => return Err(io::Error::other("expected --gated")),
        };
        if !(1..=60).contains(&minutes) {
            return Err(io::Error::other("minutes must be 1..60"));
        }
        if !(1..=100).contains(&iterations) {
            return Err(io::Error::other("iterations must be 1..100"));
        }

        Ok(Self {
            recording: PathBuf::from(&words[1]),
            minutes,
            iterations,
            gated,
        })
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    persistence::engine::initialize_process_with(persistence::engine::ThreadCounts {
        maximum: 6,
        ..persistence::engine::THREAD_DEFAULTS
    })?;
    if std::env::args().nth(1).as_deref() == Some("--freeze") {
        return fixture_control::run();
    }
    let options = Options::parse()?;
    let meter = Meter {
        timebase: Timebase::read()?,
        pid: i32::try_from(std::process::id())?,
    };
    let before = meter.read()?;
    let started = Instant::now();
    let store = Store::start(&options.recording)?;
    let mut view = wait_view(&store)?;
    meter.report("startup", 0, started, &before, &view)?;
    if store.set_window(options.minutes * 60) {
        drop(view);
        view = wait_view(&store)?;
    }
    let first = u32::from(options.gated);
    for iteration in first..=options.iterations {
        store.set_active(false);
        wait_paused(&store)?;
        drop(view);
        if options.gated {
            wait_advance(iteration)?;
        }
        let before = meter.read()?;
        let started = Instant::now();

        store.set_active(true);
        view = wait_view(&store)?;

        let phase = if iteration == 0 { "warmup" } else { "resume" };
        meter.report(phase, iteration, started, &before, &view)?;
    }
    drop(view);
    drop(store);
    Ok(())
}

fn wait_advance(iteration: u32) -> io::Result<()> {
    println!("{}", json!({"phase":"paused", "iteration":iteration}));
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().take(16).read_line(&mut line)?;
    if line != "advance\n" {
        return Err(io::Error::other("gated resume requires advance on stdin"));
    }
    Ok(())
}

fn wait_view(store: &Store) -> io::Result<Arc<View>> {
    let started = Instant::now();
    for _ in 0..POLLS_MAX {
        if let Some(view) = store.take() {
            if view.failure.is_some() {
                return Err(io::Error::other(format!(
                    "store failed: {:?}",
                    view.failure
                )));
            }
            if view.left_out.is_some() {
                return Err(io::Error::other(format!(
                    "incomplete input: {:?}",
                    view.left_out
                )));
            }
            if view.seconds.is_empty() {
                return Err(io::Error::other("no readings"));
            }
            if view.apps.len() != view.seconds.len() {
                return Err(io::Error::other("machine/App handoff differs"));
            }
            return Ok(view);
        }
        if started.elapsed() >= WAIT_MAX {
            return Err(io::Error::other("store handoff exceeded 120 seconds"));
        }
        thread::sleep(POLL);
    }
    Err(io::Error::other("store polling bound reached"))
}

fn wait_paused(store: &Store) -> io::Result<()> {
    let started = Instant::now();
    for _ in 0..POLLS_MAX {
        if store.is_paused() {
            return Ok(());
        }
        if started.elapsed() >= WAIT_MAX {
            return Err(io::Error::other("store pause exceeded 120 seconds"));
        }
        thread::sleep(POLL);
    }
    Err(io::Error::other("store pause polling bound reached"))
}

impl Meter {
    fn read(&self) -> io::Result<Usage> {
        read_usage(self.timebase, self.pid)?
            .map_err(|error| io::Error::other(format!("process counters: {error:?}")))
    }

    fn report(
        &self,
        phase: &str,
        iteration: u32,
        started: Instant,
        before: &Usage,
        view: &View,
    ) -> io::Result<()> {
        let elapsed = started.elapsed();
        let after = self.read()?;
        let footprint = Footprint::read()?;
        let cpu_ns = after.user_ns + after.system_ns - before.user_ns - before.system_ns;
        let app_rows: usize = view.apps.iter().map(|apps| apps.apps.len()).sum();
        println!(
            "{}",
            json!({
                "phase": phase, "iteration": iteration,
                "wall_ms": elapsed.as_secs_f64() * 1000.0,
                "cpu_ms": Duration::from_nanos(cpu_ns).as_secs_f64() * 1000.0,
                "disk_read_bytes": after.disk_read_bytes - before.disk_read_bytes,
                "footprint_bytes": footprint.current_bytes,
                "footprint_peak_bytes": footprint.lifetime_bytes_max,
                "resident_bytes": footprint.resident_bytes,
                "recent": view.seconds.len(), "earlier": view.earlier.len(), "app_rows": app_rows,
                "first_ns": view.seconds.first().map(|second| second.wall_ns),
                "last_ns": view.seconds.last().map(|second| second.wall_ns),
                "version": view.version,
                "scope": "Store pause/resume through complete View; excludes UI and legacy reader",
            })
        );
        Ok(())
    }
}
