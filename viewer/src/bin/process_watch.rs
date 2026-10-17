//! Opt-in external process observation; never starts or changes the observed application.
use std::io;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use collector::clock::Timebase;
use collector::processes::{self, Usage};
use serde_json::json;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("process_watch: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 4 {
        return Err(io::Error::other("--pid PID --seconds N (1..600)").into());
    }
    if arguments[0] != "--pid" {
        return Err(io::Error::other("expected --pid PID").into());
    }
    if arguments[2] != "--seconds" {
        return Err(io::Error::other("expected --seconds N").into());
    }
    let pid: i32 = arguments[1].parse()?;
    if !(1..=processes::PID_MAX).contains(&pid) {
        return Err(io::Error::other("pid outside supported range").into());
    }
    let seconds: u64 = arguments[3].parse()?;
    if !(1..=600).contains(&seconds) {
        return Err(io::Error::other("seconds must be 1..600").into());
    }
    let timebase = Timebase::read()?;
    let started = Instant::now();
    let initial = usage(timebase, pid)?;
    for sample in 0..=seconds {
        if sample > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        let current = usage(timebase, pid)?;
        if current.awake_ns_at_start != initial.awake_ns_at_start {
            return Err(io::Error::other("observed pid changed creation identity").into());
        }
        let user_ns = current.user_ns.checked_sub(initial.user_ns);
        let system_ns = current.system_ns.checked_sub(initial.system_ns);
        let cpu_ns = user_ns
            .zip(system_ns)
            .and_then(|(user, system)| user.checked_add(system))
            .ok_or_else(|| io::Error::other("process CPU counter reset or overflow"))?;
        println!(
            "{}",
            json!({"pid": pid, "sample": sample,
                "creation_awake_ns": initial.awake_ns_at_start.to_string(),
                "elapsed_seconds": started.elapsed().as_secs_f64(),
                "cpu_seconds": Duration::from_nanos(cpu_ns).as_secs_f64(),
                "physical_footprint_bytes": current.footprint_bytes,
                "resident_bytes": current.resident_bytes,
                "idle_wakeups": current.idle_wakeups.to_string(),
                "interrupt_wakeups": current.interrupt_wakeups.to_string(),
                "energy_nj": current.energy_nj.to_string()})
        );
    }
    Ok(())
}

fn usage(timebase: Timebase, pid: i32) -> io::Result<Usage> {
    processes::read_usage(timebase, pid)?.map_err(|unreadable| {
        io::Error::other(format!("process {pid} unavailable: {unreadable:?}"))
    })
}
