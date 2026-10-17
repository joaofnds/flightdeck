//! Opt-in, real-recording query measurements. No fixture generation or collector startup.
use std::ffi::c_int;
use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use persistence::resources::Footprint;
use serde_json::json;
use viewer::Session;

const ITERATIONS_MAX: u64 = 100;
const CLOCK_PROCESS_CPUTIME_ID: c_int = 12;

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: c_int, time: *mut Timespec) -> c_int;
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("query_bench: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if !matches!(arguments.len(), 4 | 6) {
        return Err(io::Error::other("--recording PATH --iterations N [--second N]").into());
    }
    if arguments[0] != "--recording" {
        return Err(io::Error::other("expected --recording PATH").into());
    }
    if arguments[2] != "--iterations" {
        return Err(io::Error::other("expected --iterations N").into());
    }
    let iterations: u64 = arguments[3].parse()?;
    if !(1..=ITERATIONS_MAX).contains(&iterations) {
        return Err(io::Error::other("iterations must be 1..100").into());
    }
    let started = Instant::now();
    let cpu = cpu_ns()?;
    let mut session = Session::open(Path::new(&arguments[1]))?;
    let catalog = session.catalog();
    let latest = catalog["last_second"]
        .as_u64()
        .ok_or_else(|| io::Error::other("no known recorded second"))?;
    let second = if arguments.len() == 6 {
        if arguments[4] != "--second" {
            return Err(io::Error::other("expected --second N").into());
        }
        arguments[5].parse()?
    } else {
        latest
    };
    report("open", 0, started, cpu)?;
    for iteration in 0..iterations {
        let started = Instant::now();
        let cpu = cpu_ns()?;
        let selection = session.select(second.saturating_sub(iteration % 30), 0)?;
        if !selection["selected"].is_null() {
            for (table, _, _) in persistence::schema::TABLES {
                let page = session.page(table.name(), 0)?;
                if page["rows"].as_array().is_none() {
                    return Err(io::Error::other("invalid page response").into());
                }
            }
        }
        let operation = if selection["selected"].is_null() {
            "select_missing_second_without_pages"
        } else {
            "select_and_ten_first_pages"
        };
        report(operation, iteration + 1, started, cpu)?;
    }
    Ok(())
}

fn report(operation: &str, iteration: u64, started: Instant, cpu_before: u64) -> io::Result<()> {
    let elapsed = started.elapsed().as_secs_f64();
    let cpu = cpu_ns()?
        .checked_sub(cpu_before)
        .ok_or_else(|| io::Error::other("CPU reset"))?;
    let footprint = Footprint::read()?;
    println!(
        "{}",
        json!({"operation": operation, "iteration": iteration,
        "wall_seconds": elapsed, "cpu_seconds": Duration::from_nanos(cpu).as_secs_f64(),
        "cpu_percent_one_core": 100.0 * Duration::from_nanos(cpu).as_secs_f64() / elapsed,
        "physical_footprint_bytes": footprint.current_bytes,
        "physical_footprint_peak_bytes": footprint.lifetime_bytes_max,
        "resident_bytes": footprint.resident_bytes})
    );
    Ok(())
}

fn cpu_ns() -> io::Result<u64> {
    let mut time = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &raw mut time) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let seconds = u64::try_from(time.tv_sec).map_err(io::Error::other)?;
    let nanoseconds = u64::try_from(time.tv_nsec).map_err(io::Error::other)?;
    if nanoseconds >= 1_000_000_000 {
        return Err(io::Error::other("CPU nanoseconds"));
    }
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanoseconds))
        .ok_or_else(|| io::Error::other("CPU time overflow"))
}
