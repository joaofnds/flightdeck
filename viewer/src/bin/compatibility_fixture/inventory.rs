use std::io::Write;

use collector::recording::identity_text;
use persistence::{Error, Result};
use serde_json::{Value, json};

use super::source::Source;
use super::{Interruption, Layout, Options, SourceLayout};

pub fn print(options: &Options, sources: &[Source]) -> Result<()> {
    assert!(sources.len() <= 2);
    let interruption = match options.interruption {
        None => "none",
        Some(Interruption::PendingSynced) => "pending-synced",
        Some(Interruption::ReadyRenamed) => "ready-renamed",
    };
    let layout = match options.layout {
        Layout::Legacy => "legacy",
        Layout::Population => "population",
        Layout::Identity => "identity",
        Layout::Mixed => "mixed",
    };
    let runs: Vec<_> = sources.iter().map(print_source).collect();
    let inventory = json!({
        "format": "flightdeck-compatibility-fixture-1", "recording": options.root,
        "layout": layout, "interruption": interruption, "sources": runs,
        "normal_sealing": "external retained persister only",
        "bounds": if options.layout == Layout::Population {
            json!({"runs": 1, "publications_per_run": super::population::PUBLICATIONS,
                "processes_per_publication": super::population::PROCESSES,
                "events_per_publication": super::population::EVENTS,
                "ring_bytes": super::population::MAPPED_BYTES_MAX,
                "inventory_bytes": 16384, "operation_seconds": 30,
                "working_bytes": 2_147_483_648_u64})
        } else { json!({ "runs": 2, "publications_per_run": 2, "processes_per_publication": 3,
            "events_per_publication": 1, "dictionary_entries": 2, "dictionary_arena_bytes": 256,
            "ring_bytes": 65536, "inventory_bytes": 16384, "operation_seconds": 30,
            "working_bytes": 2_147_483_648_u64 }) },
    });
    let bytes = serde_json::to_vec(&inventory).map_err(std::io::Error::other)?;
    if bytes.len() >= 16_384 {
        return Err(Error::Bound("fixture inventory bytes"));
    }

    let mut output = std::io::stdout().lock();
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()?;

    Ok(())
}

fn print_source(source: &Source) -> Value {
    if source.population {
        return print_population(source);
    }
    let captured = source.layout == SourceLayout::Identity;
    json!({
        "layout": if captured { 4 } else { 3 },
        "recording_id": identity_text(source.identity.recording).to_string(),
        "run_id": identity_text(source.identity.run).to_string(),
        "boot_id": identity_text(source.identity.boot).to_string(),
        "ring_name": source.ring, "lock_path": source.lock, "completed": true,
        "mapped_bytes": source.mapped_bytes, "window": 0,
        "seconds": [source.second_first, source.second_first + 1],
        "expected_rows": { "ticks": 2, "memory": 2, "cpus": 2, "interfaces": 0,
            "gpu_states": 0, "core_states": 0, "fans": 0, "coalitions": 0,
            "processes": if captured { 6 } else { 2 }, "events": 2 },
        "expected_tick": { "run_tick": [0, 1], "processes_refused": 1, "processes_gone": 1,
            "continuous_ns": ["100000000000", "101000000000"],
            "awake_ns": ["80000000000", "81000000000"] },
        "expected_memory": { "total_bytes": "8589934592", "free_bytes": "4294967296" },
        "expected_cpus": { "cpu": 0, "user": [10, 11], "system": 20, "nice": 30, "idle": 40 },
        "expected_measured": {
            "pid": 41001, "user_ns": ["9007199254740993", "9007199454740993"],
            "system_ns": ["9007199254741003", "9007199354741003"],
            "energy_nj": ["9007199254741093", "9007200254741093"],
            "cycles": ["18446744073709551605", "18446744073709551606"],
            "resident_bytes": "104857600", "footprint_bytes": "125829120",
            "awake_ns_at_start": "70000000000", "instructions": "9007199254741993",
            "super_instructions": "9007199254742993", "idle_wakeups": "11",
            "interrupt_wakeups": "12", "pageins": "13", "disk_read_bytes": "9007199254743993",
            "disk_written_bytes": "9007199254744993", "threads": 4,
            "mach_syscalls": 17, "unix_syscalls": 18,
            "faults": 2_147_483_647, "context_switches": -2_147_483_648_i64,
        },
        "expected_identity": if captured { print_identity() } else { Value::Null },
        "expected_legacy_identity_availability": if captured { Value::Null } else { json!(3) },
    })
}

fn print_identity() -> Value {
    json!({
        "identity_version": 1, "identity_availability": 0,
        "executable_name_hex": "776f726b6572ff0a5c",
        "app_path_hex": "2f4170706c69636174696f6e732f46697874757265ff0a5c2e617070",
        "processes": [
            { "pid": 41001, "usage_availability": 0, "unique_id": "18446744073709551615" },
            { "pid": 41002, "usage_availability": 1, "unique_id": "18446744073709551614" },
            { "pid": 41003, "usage_availability": 2, "unique_id": "18446744073709551613" },
        ],
        "pid_version": -2_147_483_648_i64, "unmeasured_original_payload": "all 19 columns NULL",
        "events": { "pid": 42001, "unique_id": "1311768467463790320",
            "pid_version": 2_147_483_647, "executable_name_hex": "6576656e74fe095c",
            "app_path": null, "kinds": [1, 3], "usage_availability": [3, 0],
            "start_coalition": 17 },
    })
}

fn print_population(source: &Source) -> Value {
    use super::population::{EVENTS, MAPPED_BYTES_MAX, PROCESSES, PUBLICATIONS};
    json!({"layout": 3, "completed": true, "ring_name": source.ring,
        "lock_path": source.lock,
        "recording_id": identity_text(source.identity.recording).to_string(),
        "run_id": identity_text(source.identity.run).to_string(),
        "boot_id": identity_text(source.identity.boot).to_string(),
        "publications": PUBLICATIONS, "processes_per_publication": PROCESSES,
        "events_per_publication": EVENTS, "mapped_bytes": source.mapped_bytes,
        "mapped_bytes_max": MAPPED_BYTES_MAX,
        "first_second": source.second_first, "last_second": source.second_first + PUBLICATIONS - 1,
        "expected_selected_publication": PUBLICATIONS, "expected_cpu_percent": 30.0,
        "expected_rx_bps": 1_048_576.0, "expected_fans": [1234.1, 0.0],
        "source_units": "native counters and gauges; unavailable legacy identity",
        "sealed_windows": 0, "publisher_after_generation": "none"})
}
