use std::io;
use std::path::Path;
use std::time::Instant;

use collector::clock::Timebase;
use serde_json::{Value, json};

use crate::projection_bench::{Counter, bits};

pub(super) fn run(envelope: &super::Envelope, iterations: u32, output: &Path) -> io::Result<()> {
    assert!((1..=100).contains(&iterations));
    let expected = preflight(envelope)?;
    let value = envelope.value();
    let timebase = Timebase::read()?;
    let pid = i32::try_from(std::process::id()).map_err(io::Error::other)?;
    let mut measurements =
        Vec::with_capacity(usize::try_from(iterations).map_err(io::Error::other)?);
    for iteration in 0..iterations {
        let cpu_before = cpu_ns(timebase, pid)?;
        let started = Instant::now();
        let counter = Counter::start();
        let panels =
            crate::live_projection::panels("apps", &value["selection"], Some(&value["history"]));
        let counts = counter.finish();
        let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).map_err(io::Error::other)?;
        let cpu_after = cpu_ns(timebase, pid)?;
        let panels = panels.map_err(io::Error::other)?;
        if bits(&panels) != expected {
            return Err(io::Error::other(
                "App projection output changed between iterations",
            ));
        }

        let process_cpu_ns = cpu_after
            .checked_sub(cpu_before)
            .ok_or_else(|| io::Error::other("Process CPU counter regressed"))?;
        measurements.push(json!({"iteration":iteration,"elapsed_ns":elapsed_ns,
            "process_cpu_ns":process_cpu_ns,"allocation_calls":counts.0,
            "requested_bytes":counts.1}));
    }

    super::write_new(
        output,
        &super::transcript(&super::Transcript {
            envelope,
            panels: &expected,
        }),
    )?;
    report(value, &measurements);

    Ok(())
}

pub(super) fn preflight(envelope: &super::Envelope) -> io::Result<Value> {
    let value = envelope.value();
    let panels =
        crate::live_projection::panels("apps", &value["selection"], Some(&value["history"]))
            .map_err(io::Error::other)?;
    if panels.len() != 6 {
        return Err(io::Error::other(
            "App projection requires all six App panels",
        ));
    }

    Ok(bits(&panels))
}

fn cpu_ns(timebase: Timebase, pid: i32) -> io::Result<u64> {
    let usage = collector::processes::read_usage(timebase, pid)?.map_err(|error| {
        io::Error::other(format!("Process CPU observation unavailable: {error:?}"))
    })?;

    usage
        .user_ns
        .checked_add(usage.system_ns)
        .ok_or_else(|| io::Error::other("Process CPU counter overflowed"))
}

fn report(value: &Value, measurements: &[Value]) {
    assert!(measurements.len() <= 100);
    println!(
        "{}",
        json!({"mode":"app-projection","route":"apps","app":value["app"],
        "selected":value["history"]["selected"],
        "history_points":value["history"]["points"].as_array().map(Vec::len),
        "coverage":value["history"]["coverage"],"warmup_projections":1,
        "measurements":measurements,
        "counter":"Rust GlobalAlloc requests during panels only; \
            requested bytes are not live memory",
        "timing":"Wall includes Counter/clock overhead; \
            process CPU includes syscall/counter overhead",
        "scope":"After one oracle/warmup projection; excludes query, decode, encoding and UI"})
    );
}
