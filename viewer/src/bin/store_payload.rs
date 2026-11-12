//! Opt-in component preflight. Reports retained payloads and process footprint, not UI gates.
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use persistence::resources::{Footprint, Watchdog};
use serde_json::json;
use viewer::apps::Amount;
use viewer::store::{
    AppDetails, AppRow, AppsSecond, ByteBudget, MachineSecond, Missing, Payload, ProcessRow,
    SampledSecond, Store, View,
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
const POLL_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / STEPS_PER_SECOND);
const _: () = assert!(1_000_000_000 % STEPS_PER_SECOND == 0);

struct Arguments {
    recording: PathBuf,
    bytes: usize,
    seconds: u64,
}

#[derive(Default)]
struct PayloadCounts {
    complete: usize,
    unread: usize,
    budget: usize,
    refused: usize,
    too_many: usize,
    processes: usize,
    processes_distinct: usize,
    processes_max: usize,
    app_details: usize,
    app_details_distinct: usize,
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    persistence::engine::initialize_process()?;
    let arguments = Arguments::parse(&std::env::args().skip(1).take(7).collect::<Vec<_>>())?;
    let watchdog = Watchdog::with_operation_limit(FOOTPRINT_BYTES_MAX, Duration::from_secs(90))?;
    let operation = watchdog.operation()?;
    let budget = Arc::new(ByteBudget::new(arguments.bytes));
    report_footprint("before_store", &budget)?;
    let started = Instant::now();
    let store = Store::start_with_budget(&arguments.recording, Arc::clone(&budget))?;
    let mut latest = None;
    for _step in 0..arguments.seconds * STEPS_PER_SECOND {
        if let Some(view) = store.take() {
            take_view(&mut latest, view, |first| {
                println!("{}", first_view(started.elapsed(), first, &budget)?);
                Ok(())
            })?;
        }
        thread::sleep(POLL_INTERVAL);
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

fn take_view(
    latest: &mut Option<Arc<View>>,
    view: Arc<View>,
    first: impl FnOnce(&View) -> io::Result<()>,
) -> io::Result<()> {
    if latest.is_none() {
        first(&view)?;
    }
    *latest = Some(view);

    Ok(())
}

fn first_view(
    elapsed: Duration,
    view: &View,
    budget: &ByteBudget,
) -> io::Result<serde_json::Value> {
    let footprint = Footprint::read()?;
    Ok(json!({
        "component_only": true,
        "phase": "first_view",
        "observed_wall_ns": u64::try_from(elapsed.as_nanos()).map_err(io::Error::other)?,
        "poll_interval_ns": u64::try_from(POLL_INTERVAL.as_nanos()).map_err(io::Error::other)?,
        "version": view.version,
        "recent": view.seconds.len(),
        "earlier": view.earlier.len(),
        "reserved_bytes": budget.used(),
        "reserved_bytes_peak": budget.used_peak(),
        "footprint_bytes": footprint.current_bytes,
        "footprint_peak_bytes": footprint.lifetime_bytes_max,
    }))
}

impl Arguments {
    fn parse(arguments: &[String]) -> io::Result<Self> {
        if arguments.len() != 6 {
            return Err(io::Error::other(
                "--recording PATH --payload-bytes N --seconds N",
            ));
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
        })
    }
}

fn report(view: &View, budget: &ByteBudget) -> io::Result<()> {
    let footprint = Footprint::read()?;
    let walk = RowsWalk::of(view);
    let walk_ns = u64::try_from(walk.elapsed.as_nanos()).map_err(io::Error::other)?;
    let details_walk = RowsWalk::of_app_details(view);
    let details_walk_ns =
        u64::try_from(details_walk.elapsed.as_nanos()).map_err(io::Error::other)?;
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
            "process_rows_distinct": counts.processes_distinct,
            "process_rows_max": counts.processes_max,
            "process_row_reserved_bytes": Payload::ROW_BYTES,
            "process_rows_walk_ns": walk_ns,
            "process_rows_walk_sum": walk.sum,
            "app_details_walk_ns": details_walk_ns,
            "app_details_walk_sum": details_walk.sum,
            "app_details_rows": counts.app_details,
            "app_details_rows_distinct": counts.app_details_distinct,
            "app_details_row_reserved_bytes": Payload::APP_DETAILS_BYTES,
            "app_row_capacity_bytes_by_reference": view.apps.iter()
                .map(|apps| apps.apps.capacity() * size_of::<AppRow>()).sum::<usize>(),
            "app_row_initialized_bytes_by_reference": view.apps.iter()
                .map(|apps| apps.apps.len() * size_of::<AppRow>()).sum::<usize>(),
            "reserved_bytes": budget.used(),
            "reserved_bytes_max": budget.maximum(),
            "reserved_bytes_peak": budget.used_peak(),
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
            "reserved_bytes_peak": budget.used_peak(),
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
                    counts.app_details += payload.apps().len();
                }
                Err(Missing::Unread) => counts.unread += 1,
                Err(Missing::Budget) => counts.budget += 1,
                Err(Missing::Refused(_)) => counts.refused += 1,
                Err(Missing::TooMany) => counts.too_many += 1,
            }
        }
        let mut addresses = Self::of_addresses(view, counts.processes, Payload::iter);
        counts.processes_distinct = Self::of_distinct(&mut addresses);
        let mut addresses = Self::of_addresses(view, counts.app_details, Payload::apps);
        counts.app_details_distinct = Self::of_distinct(&mut addresses);

        counts
    }

    /// Where each row of one kind is kept, for each second that lists it.
    fn of_addresses<'view, Row: 'view, Rows: Iterator<Item = &'view Row>>(
        view: &'view View,
        count: usize,
        rows: fn(&'view Payload) -> Rows,
    ) -> Vec<usize> {
        let mut addresses = Vec::with_capacity(count);
        for payload in payloads(view) {
            addresses.extend(rows(payload).map(|row| std::ptr::from_ref(row).addr()));
        }
        assert_eq!(addresses.len(), count);

        addresses
    }

    fn of_distinct(addresses: &mut [usize]) -> usize {
        addresses.sort_unstable();

        addresses.chunk_by(|left, right| left == right).count()
    }
}

/// The seconds of `view` that hold their process rows.
fn payloads(view: &View) -> impl Iterator<Item = &Payload> {
    view.apps
        .iter()
        .filter_map(|apps| apps.processes.as_deref().ok())
}

/// One reader's pass over every reading of every held row of one kind, taken before anything
/// else reads those rows, with a sum that shows what it read.
struct RowsWalk {
    elapsed: Duration,
    sum: u64,
}

impl RowsWalk {
    fn of(view: &View) -> Self {
        let started = Instant::now();
        let mut sum = 0_u64;
        for payload in payloads(view) {
            for row in payload {
                sum = sum.wrapping_add(Self::of_row(row));
            }
        }

        Self {
            elapsed: started.elapsed(),
            sum,
        }
    }

    fn of_app_details(view: &View) -> Self {
        let started = Instant::now();
        let mut sum = 0_u64;
        for payload in payloads(view) {
            for details in payload.apps() {
                sum = sum
                    .wrapping_add(u64::from(details.live_members))
                    .wrapping_add(details.disk_read_bytes.known)
                    .wrapping_add(details.disk_written_bytes.known)
                    .wrapping_add(details.footprint_bytes.known);
            }
        }

        Self {
            elapsed: started.elapsed(),
            sum,
        }
    }

    fn of_row(row: &ProcessRow) -> u64 {
        let amounts = [
            row.cpu_ns(),
            row.energy_nj(),
            row.gpu_ns(),
            row.disk_read_bytes(),
            row.disk_written_bytes(),
            row.disk_io_bytes(),
            row.resident_bytes(),
            row.footprint_bytes(),
            row.idle_wakeups(),
            row.pageins(),
            row.threads(),
        ];
        let mut sum = u64::from(row.pid.unsigned_abs());
        for amount in amounts {
            if let Amount::Known(known) = amount {
                sum = sum.wrapping_add(known);
            }
        }

        sum
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
    fn reports_only_the_first_arrival_while_retaining_the_latest_view() {
        // Goal: later Views cannot replace the first arrival or leave the final View stale.
        // Method: deliver two real Views and inspect the first record and final selection.
        let budget = ByteBudget::new(1_000);
        let mut latest = None;
        let mut output = Vec::new();

        take_view(&mut latest, Arc::new(empty_view(1)), |view| {
            output.push(first_view(Duration::from_millis(123), view, &budget)?);
            Ok(())
        })
        .unwrap();
        take_view(&mut latest, Arc::new(empty_view(2)), |view| {
            output.push(first_view(Duration::from_millis(456), view, &budget)?);
            Ok(())
        })
        .unwrap();

        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["phase"], "first_view");
        assert_eq!(output[0]["version"], 1);
        assert_eq!(output[0]["observed_wall_ns"], 123_000_000);
        assert_eq!(latest.unwrap().version, 2);
    }

    fn empty_view(version: u64) -> View {
        View {
            version,
            seconds: Vec::new(),
            apps: Vec::new(),
            earlier: Vec::new(),
            stretches: Vec::new(),
            sampled: Vec::new(),
            apps_loaded: true,
            failure: None,
            left_out: None,
            ring_complete: false,
            full_detail_evicted_through_ns: None,
        }
    }

    #[test]
    fn a_row_two_seconds_hold_counts_once() {
        // Goal: a shared row is one row of storage. Method: count two aliases around another owner.
        let shared = Arc::new(7_u8);
        let alias = Arc::clone(&shared);
        let other = Arc::new(7_u8);
        let mut addresses = vec![
            Arc::as_ptr(&shared).addr(),
            Arc::as_ptr(&other).addr(),
            Arc::as_ptr(&alias).addr(),
        ];

        assert_eq!(PayloadCounts::of_distinct(&mut addresses), 2);
    }

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
