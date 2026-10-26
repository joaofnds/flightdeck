//! A gated real mailbox caller; the parent advances immutable input between commands.
use std::io::{BufRead, Read, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Probe, WAIT_INTERVAL, WAIT_STEPS_MAX, probe_request};
use crate::live_source::tests::native_measurements::payloads;
use crate::live_source::{Snapshot, Update, Worker};

const COMMANDS_MAX: u32 = 1024;

pub(crate) fn read_recorded(root: &std::path::Path, step: &Value) -> Box<Snapshot> {
    let mut probe = Probe {
        worker: Worker::start_recorded(root.to_owned()).unwrap(),
        deadline: Instant::now() + Duration::from_secs(10),
        snapshots: 0,
    };
    let generation = probe.worker.request(probe_request("overview")).unwrap();
    wait(&mut probe, generation, "overview", step)
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

    probe.worker.stop();
    drop(probe);
    println!("{}", json!({"event": "worker_dropped", "success": true}));
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
            "follow" | "resume" => {}
            "pin" | "hover" => {
                request.second =
                    Some(step["selected"]["wall_ns"].as_u64().unwrap() / 1_000_000_000);
                request.identity = Some(step["selected"].clone());
            }
            "supersede" | "cancel" => {
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
            probe.worker.request(request).unwrap()
        };
        let snapshot = wait(probe, generation, route, step);
        if words[0] != "hover" {
            context = snapshot.selection["trends"].clone();
        }
        println!(
            "{}",
            json!({"event": "complete", "step": index,
            "command": words[0], "generation": generation,
            "selected": snapshot.selection["selected"], "summary": snapshot.selection["summary"],
            "context_values": snapshot.selection["trends"],
            "kept_context_values": context,
            "payloads": payloads::validate(&snapshot, route, step).unwrap()})
        );
    }
    panic!("matched command count bound reached");
}

fn wait(probe: &mut Probe, generation: u64, route: &str, step: &Value) -> Box<Snapshot> {
    let mut last_mismatch = String::new();
    for _attempt in 0..WAIT_STEPS_MAX {
        assert!(Instant::now() < probe.deadline, "{last_mismatch}");
        match probe.worker.take().unwrap() {
            Some(Update::Ready(snapshot)) => {
                if snapshot.generation < generation {
                    continue;
                }
                assert_eq!(snapshot.generation, generation);
                probe.snapshots += 1;
                match payloads::validate(&snapshot, route, step) {
                    Ok(_) if !snapshot.histories_pending => return snapshot,
                    Ok(_) => last_mismatch = "histories still pending".into(),
                    Err(error) => last_mismatch = error,
                }
            }
            Some(Update::Failed { message, .. }) => panic!("{message}"),
            None => std::thread::sleep(WAIT_INTERVAL),
        }
    }
    panic!("matched completion bound reached: {last_mismatch}");
}
