//! Opt-in component preflight. Reports retained payloads and process footprint, not UI gates.
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use persistence::resources::{Footprint, Watchdog};
use serde_json::json;
use viewer::store::{
    AppDetails, AppRow, AppsSecond, ByteBudget, MachineSecond, Missing, ProcessRow, SampledSecond,
    Store, View,
};

#[path = "store_payload/name_census.rs"]
mod name_census;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("store_payload: {error}");
            ExitCode::FAILURE
        }
    }
}

const SECONDS_MAX: u64 = 60;
const PAYLOAD_BYTES_MAX: usize = 1024 * 1024 * 1024;
const FOOTPRINT_BYTES_MAX: u64 = 830_000_000;
const STEPS_PER_SECOND: u64 = 10;

struct Arguments {
    recording: PathBuf,
    bytes: usize,
    seconds: u64,
    legacy: bool,
}

#[derive(Default)]
struct PayloadCounts {
    complete: usize,
    unread: usize,
    budget: usize,
    refused: usize,
    too_many: usize,
    processes: usize,
    processes_max: usize,
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    persistence::engine::initialize_process()?;
    let arguments = Arguments::parse(&std::env::args().skip(1).take(8).collect::<Vec<_>>())?;
    let watchdog = Watchdog::with_operation_limit(FOOTPRINT_BYTES_MAX, Duration::from_secs(90))?;
    let operation = watchdog.operation()?;
    let budget = Arc::new(ByteBudget::new(arguments.bytes));
    report_footprint("before_store", &budget)?;
    let store = if arguments.legacy {
        Store::start(&arguments.recording)?
    } else {
        Store::start_with_budget(&arguments.recording, Arc::clone(&budget))?
    };
    let mut latest = None;
    for _step in 0..arguments.seconds * STEPS_PER_SECOND {
        if let Some(view) = store.take() {
            latest = Some(view);
        }
        thread::sleep(Duration::from_millis(100));
    }
    let view = latest.ok_or_else(|| io::Error::other("store published no View"))?;
    report(&view, &budget)?;
    name_census::report(&view, &budget)?;
    drop(view);
    drop(store);
    report_footprint("after_store_drop", &budget)?;
    drop(operation);
    drop(watchdog);
    check_footprint(&Footprint::read()?)?;
    Ok(())
}

impl Arguments {
    fn parse(arguments: &[String]) -> io::Result<Self> {
        if !matches!(arguments.len(), 6 | 7) {
            return Err(io::Error::other(
                "--recording PATH --payload-bytes N --seconds N [--legacy]",
            ));
        }
        let legacy = arguments.len() == 7;
        if legacy {
            if arguments[6] != "--legacy" {
                return Err(io::Error::other("expected --legacy"));
            }
        }
        for (index, name) in [(0, "--recording"), (2, "--payload-bytes"), (4, "--seconds")] {
            if arguments[index] != name {
                return Err(io::Error::other(format!("expected {name}")));
            }
        }
        let bytes = arguments[3].parse().map_err(io::Error::other)?;
        let seconds = arguments[5].parse().map_err(io::Error::other)?;
        if !(1..=PAYLOAD_BYTES_MAX).contains(&bytes) {
            return Err(io::Error::other("payload bytes must be 1..1073741824"));
        }
        if !(1..=SECONDS_MAX).contains(&seconds) {
            return Err(io::Error::other("seconds must be 1..60"));
        }
        Ok(Self {
            recording: PathBuf::from(&arguments[1]),
            bytes,
            seconds,
            legacy,
        })
    }
}

fn report(view: &View, budget: &ByteBudget) -> io::Result<()> {
    let footprint = Footprint::read()?;
    let counts = PayloadCounts::of(view);
    println!(
        "{}",
        json!({
            "component_only": true,
            "phase": "held_view",
            "version": view.version,
            "recent": view.seconds.len(),
            "earlier": view.earlier.len(),
            "sampled": view.sampled.len(),
            "sampled_complete": view.sampled.iter().filter(|sample| sample.ranks().is_ok()).count(),
            "payload_complete": counts.complete,
            "payload_unread": counts.unread,
            "payload_budget": counts.budget,
            "payload_refused": counts.refused,
            "payload_too_many": counts.too_many,
            "process_rows": counts.processes,
            "process_rows_max": counts.processes_max,
            "app_row_capacity_bytes_by_reference": view.apps.iter()
                .map(|apps| apps.apps.capacity() * size_of::<AppRow>()).sum::<usize>(),
            "app_row_initialized_bytes_by_reference": view.apps.iter()
                .map(|apps| apps.apps.len() * size_of::<AppRow>()).sum::<usize>(),
            "reserved_bytes": budget.used(),
            "reserved_bytes_max": budget.maximum(),
            "footprint_bytes": footprint.current_bytes,
            "footprint_peak_bytes": footprint.lifetime_bytes_max,
            "resident_bytes": footprint.resident_bytes,
            "failure": format!("{:?}", view.failure),
            "left_out": format!("{:?}", view.left_out),
            "type_bytes": {
                "process": size_of::<ProcessRow>(), "app": size_of::<AppRow>(),
                "app_details": size_of::<AppDetails>(), "apps_second": size_of::<AppsSecond>(),
                "machine_second": size_of::<MachineSecond>(),
                "sampled_second": size_of::<SampledSecond>(),
            },
        })
    );
    Ok(())
}

fn report_footprint(phase: &str, budget: &ByteBudget) -> io::Result<()> {
    let footprint = Footprint::read()?;
    println!(
        "{}",
        json!({
            "component_only": true,
            "phase": phase,
            "reserved_bytes": budget.used(),
            "footprint_bytes": footprint.current_bytes,
            "footprint_peak_bytes": footprint.lifetime_bytes_max,
            "resident_bytes": footprint.resident_bytes,
        })
    );
    Ok(())
}

impl PayloadCounts {
    fn of(view: &View) -> Self {
        let mut counts = Self::default();
        for apps in &view.apps {
            match &apps.processes {
                Ok(payload) => {
                    counts.complete += 1;
                    counts.processes += payload.len();
                    counts.processes_max = counts.processes_max.max(payload.len());
                }
                Err(Missing::Unread) => counts.unread += 1,
                Err(Missing::Budget) => counts.budget += 1,
                Err(Missing::Refused(_)) => counts.refused += 1,
                Err(Missing::TooMany) => counts.too_many += 1,
            }
        }
        counts
    }
}

fn check_footprint(footprint: &Footprint) -> io::Result<()> {
    let bytes = footprint.current_bytes.max(footprint.lifetime_bytes_max);
    if bytes > FOOTPRINT_BYTES_MAX {
        return Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!("payload preflight footprint {bytes} exceeds {FOOTPRINT_BYTES_MAX}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod footprint_tests {
    use super::*;

    #[test]
    fn accepts_the_exact_footprint_ceiling() {
        // Goal: the declared ceiling is inclusive. Method: check its exact boundary.
        let memory = Footprint {
            current_bytes: FOOTPRINT_BYTES_MAX,
            lifetime_bytes_max: FOOTPRINT_BYTES_MAX,
            resident_bytes: 0,
        };

        assert!(check_footprint(&memory).is_ok());
    }

    #[test]
    fn rejects_a_peak_above_the_last_watchdog_sample() {
        // Goal: short-lived excess cannot pass at shutdown. Method: check a recovered peak.
        let memory = Footprint {
            current_bytes: 1,
            lifetime_bytes_max: FOOTPRINT_BYTES_MAX + 1,
            resident_bytes: 0,
        };

        let refused = check_footprint(&memory).unwrap_err();

        assert_eq!(refused.kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    fn rejects_current_footprint_before_the_peak_catches_up() {
        // Goal: a delayed peak counter cannot hide excess. Method: check current bytes alone.
        let memory = Footprint {
            current_bytes: FOOTPRINT_BYTES_MAX + 1,
            lifetime_bytes_max: FOOTPRINT_BYTES_MAX,
            resident_bytes: 0,
        };

        let refused = check_footprint(&memory).unwrap_err();

        assert_eq!(refused.kind(), io::ErrorKind::OutOfMemory);
    }
}
