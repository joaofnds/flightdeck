//! A gated real mailbox caller; the parent advances immutable input between commands.
use std::io::{BufRead, Read, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Probe, WAIT_INTERVAL, WAIT_STEPS_MAX, probe_request};
use crate::live_source::tests::native_measurements::payloads;
use crate::live_source::{Snapshot, Update, Worker};

const COMMANDS_MAX: u32 = 1024;

struct CommandReport<'a> {
    name: &'a str,
    route: &'a str,
    index: usize,
    expected: &'a Value,
}

pub(crate) fn read_recorded(root: &std::path::Path, step: &Value) -> Box<Snapshot> {
    let mut probe = Probe {
        worker: Worker::start_recorded(root.to_owned()).unwrap(),
        deadline: Instant::now() + Duration::from_secs(10),
        snapshots: 0,
    };
    let generation = probe.worker.request(probe_request("overview")).unwrap();
    let snapshot = wait(&mut probe, generation, "overview", step);
    let mut thread = probe.worker.retire();
    join_observed(&mut thread, Instant::now() + Duration::from_secs(10)).unwrap();
    snapshot
}

#[test]
#[ignore = "coordinated finite native replay; requires input-side step expectations and stdin"]
fn follows_matched_replay_through_the_real_worker_mailbox() {
    // Goal: deferred payloads, hover, pin/resume and supersession stay in measured coverage.
    // Method: reuse the production Worker and probe requests with externally frozen step oracles.
    let root = std::env::var_os("FLIGHTDECK_ITEM1_RECORDING").unwrap();
    let expected = std::env::var_os("FLIGHTDECK_ITEM1_EXPECTED_FILE").unwrap();
    assert!(std::fs::metadata(&expected).unwrap().len() <= 32 * 1024 * 1024);
    let expected: Value = serde_json::from_slice(&std::fs::read(expected).unwrap()).unwrap();
    let route = std::env::var("FLIGHTDECK_ITEM1_ROUTE").unwrap();
    assert!(["network", "memory", "cpu", "apps", "overview"].contains(&route.as_str()));
    let mut probe = Probe {
        worker: Worker::start(root.into()).unwrap(),
        deadline: Instant::now() + Duration::from_secs(300),
        snapshots: 0,
    };

    commands(&mut probe, &route, &expected);

    let mut thread = probe.worker.retire();
    let result = join_observed(&mut thread, Instant::now() + Duration::from_secs(10));
    println!(
        "{}",
        json!({"event": "worker_dropped", "success": result.is_ok(),
        "termination": if result.is_ok() {"joined"} else {"unconfirmed"},
        "error": result.as_ref().err()})
    );
    result.unwrap();
    std::thread::sleep(Duration::from_secs(5));
    println!("{}", json!({"event": "settled"}));
}

fn commands(probe: &mut Probe, route: &str, expected: &Value) {
    let mut input = std::io::stdin().lock();
    let mut context = Value::Null;
    for _command in 0..COMMANDS_MAX {
        println!("{}", json!({"event": "await_command"}));
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        assert!(input.by_ref().take(129).read_line(&mut line).unwrap() <= 128);
        if line == "stop\n" {
            return;
        }
        let words: Vec<_> = line.split_whitespace().take(3).collect();
        assert_eq!(words.len(), 2);
        let index: usize = words[1].parse().unwrap();
        let step = &expected["steps"][index];
        assert!(step.is_object());
        let mut request = probe_request(route);
        match words[0] {
            "follow" | "resume" | "cancel" => {}
            "pin" | "hover" => {
                request.second =
                    Some(step["selected"]["wall_ns"].as_u64().unwrap() / 1_000_000_000);
                request.identity = Some(step["selected"].clone());
            }
            "supersede" => {
                for route in ["apps", "cpu", "overview"] {
                    let generation = probe.worker.request(probe_request(route)).unwrap();
                    assert!(generation > 0);
                }
            }
            _ => panic!("expected follow, pin, hover, resume, supersede, cancel or stop"),
        }
        let generation = if words[0] == "hover" {
            payloads::validate_context(&context, &step["kept_observations"]).unwrap();
            request.load_resident_history = false;
            request.load_app_attribution_history = false;
            request.load_process_history = false;
            probe.worker.inspect(request).unwrap()
        } else {
            if words[0] == "cancel" {
                cancel_running(&mut probe.worker, request)
            } else {
                probe.worker.request(request).unwrap()
            }
        };
        let snapshot = wait(probe, generation, route, step);
        if words[0] != "hover" {
            context = snapshot.selection["trends"].clone();
        }
        emit_complete(
            &snapshot,
            &context,
            &CommandReport {
                name: words[0],
                route,
                index,
                expected: step,
            },
        );
    }
    panic!("matched command count bound reached");
}

fn emit_complete(snapshot: &Snapshot, context: &Value, report: &CommandReport<'_>) {
    println!(
        "{}",
        json!({"event": "complete", "step": report.index,
        "command": report.name, "generation": snapshot.generation,
        "selected": snapshot.selection["selected"], "summary": snapshot.selection["summary"],
        "context_values": snapshot.selection["trends"], "kept_context_values": context,
        "payloads": payloads::validate(snapshot, report.route, report.expected).unwrap()})
    );
}

fn cancel_running(worker: &mut Worker, request: crate::live_source::Request) -> u64 {
    let hold = worker.hold_measurement_stage(crate::live_source::measurement::Stage::Reading);
    let predecessor = worker.request(request.clone()).unwrap();
    assert_eq!(hold.entered(), predecessor);
    let replacement = worker.request(request).unwrap();
    hold.release();
    assert!(
        hold.observed_interruption(),
        "predecessor did not observe interruption"
    );
    println!(
        "{}",
        json!({"event": "interrupted", "generation": predecessor,
        "replacement_generation": replacement})
    );
    replacement
}

fn wait(probe: &mut Probe, generation: u64, route: &str, step: &Value) -> Box<Snapshot> {
    let mut last_mismatch = String::new();
    for _attempt in 0..WAIT_STEPS_MAX {
        assert!(Instant::now() < probe.deadline, "{last_mismatch}");
        match probe.worker.take().unwrap() {
            Some(Update::Ready(snapshot)) => {
                assert_eq!(snapshot.generation, generation);
                probe.snapshots += 1;
                match completed(&probe.worker, &snapshot, route, step) {
                    Ok(()) => return snapshot,
                    Err(error) => last_mismatch = error,
                }
            }
            Some(Update::Failed { message, .. }) => panic!("{message}"),
            None => std::thread::sleep(WAIT_INTERVAL),
        }
    }
    panic!("matched completion bound reached: {last_mismatch}");
}

fn completed(
    worker: &Worker,
    snapshot: &Snapshot,
    route: &str,
    step: &Value,
) -> Result<(), String> {
    if !worker.last_output_complete() {
        return Err("reader generation remains pending".into());
    }

    payloads::validate(snapshot, route, step)?;
    if snapshot.histories_pending {
        return Err("histories still pending".into());
    }

    Ok(())
}

pub(crate) fn checks_pending_completion(root: &std::path::Path, step: &Value) {
    let mut probe = Probe {
        worker: Worker::start_recorded(root.to_owned()).unwrap(),
        deadline: Instant::now() + Duration::from_secs(10),
        snapshots: 0,
    };
    let hold = probe
        .worker
        .hold_measurement_stage(crate::live_source::measurement::Stage::Pending);
    let generation = probe.worker.request(probe_request("overview")).unwrap();
    assert_eq!(hold.entered(), generation);
    let Some(Update::Ready(snapshot)) = probe.worker.take().unwrap() else {
        panic!("missing early snapshot");
    };
    payloads::validate(&snapshot, "overview", step).unwrap();
    assert!(!snapshot.histories_pending);
    assert!(!probe.worker.last_output_complete());
    assert!(completed(&probe.worker, &snapshot, "overview", step).is_err());
    hold.release();
    drop(hold);
    let snapshot = wait(&mut probe, generation, "overview", step);
    assert!(probe.worker.last_output_complete());
    assert!(!snapshot.histories_pending);
}

pub(crate) fn checks_interrupted_read(root: &std::path::Path, step: &Value) {
    let mut probe = Probe {
        worker: Worker::start(root.to_owned()).unwrap(),
        deadline: Instant::now() + Duration::from_secs(10),
        snapshots: 0,
    };
    let generation = cancel_running(&mut probe.worker, probe_request("overview"));
    let snapshot = wait(&mut probe, generation, "overview", step);
    assert_eq!(snapshot.generation, generation);
    assert!(probe.worker.last_output_complete());
    let mut thread = probe.worker.retire();
    join_observed(&mut thread, Instant::now() + Duration::from_secs(10)).unwrap();
}

pub(crate) fn checks_held_teardown(root: &std::path::Path) {
    let mut worker = Worker::start(root.to_owned()).unwrap();
    let hold = worker.hold_measurement_stage(crate::live_source::measurement::Stage::Reading);
    let generation = worker.request(probe_request("overview")).unwrap();
    assert_eq!(hold.entered(), generation);
    let mut thread = worker.retire();

    assert!(join_observed(&mut thread, Instant::now()).is_err());
    assert!(!thread.as_ref().unwrap().is_finished());
    hold.release();
    join_observed(&mut thread, Instant::now() + Duration::from_secs(10)).unwrap();
    assert!(thread.is_none());
}

fn join_observed(
    slot: &mut Option<std::thread::JoinHandle<()>>,
    deadline: Instant,
) -> Result<(), String> {
    for _attempt in 0..WAIT_STEPS_MAX {
        if slot.as_ref().ok_or("missing retired worker")?.is_finished() {
            return slot
                .take()
                .unwrap()
                .join()
                .map_err(|_| "worker thread panicked".into());
        }

        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("worker termination deadline elapsed")?;
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    }
    Err("worker termination attempt bound".into())
}
