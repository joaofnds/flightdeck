//! The Apps of seconds the store holds and the worker's App histories of them, which the tests
//! of Overview's App charts drawn from the store compare with the charts the worker's histories
//! draw.

use std::sync::Arc;

use serde_json::{Value, json};
use viewer::apps::Stamp;
use viewer::live_app_response::{App, Key, KeyKind, Measure, Observation, RankedApp};
use viewer::store::{AppRow, AppsSecond, MachineSecond, Totals, View};

use crate::live_store_fixtures::{NANOSECONDS_PER_SECOND, reading_at, view};

/// Seconds 100, 101, 102 and 104, the last in a segment of its own.
pub fn seconds() -> [MachineSecond; 4] {
    [
        reading_at(100, 0),
        reading_at(101, 0),
        reading_at(102, 0),
        reading_at(104, 1),
    ]
}

/// The View of `seconds`, at each of which but the third the executables A, B and C used 0.5,
/// 0.25 and 0.125 s of CPU and held as many bytes resident as they used nanoseconds, the third
/// holding no Apps, and whose CPU chart draws A and B on lines of their own.
pub fn charted(seconds: &[MachineSecond; 4]) -> View {
    let mut view = view(1, &[], seconds);
    let used = [("A", 500_000_000), ("B", 250_000_000), ("C", 125_000_000)];
    for (index, consecutive) in [(0, false), (1, true), (3, false)] {
        view.apps[index] = Arc::new(AppsSecond {
            consecutive,
            ..apps_at(&seconds[index], &used)
        });
    }
    view.apps[2] = Arc::new(AppsSecond {
        observation: Some(apps_at_observation(&seconds[2])),
        consecutive: true,
        ..AppsSecond::default()
    });
    view.cohorts.seconds = seconds.len();
    view.cohorts.cpu = vec![ranked("A"), ranked("B")];

    view
}

/// The executable `name`, as a chart's cohort names it.
pub fn ranked(name: &str) -> Arc<RankedApp> {
    Arc::new(RankedApp {
        kind: KeyKind::Executable,
        bytes: name.as_bytes().to_vec(),
        display_name: name.into(),
    })
}

pub fn worker_measure(known: u64) -> Value {
    json!({"known": known.to_string(), "measured_members": 1, "unavailable_members": 0,
        "overflowed": false})
}

pub fn worker_app(name: &str, bytes_hex: &str) -> Value {
    json!({"key": {"kind": "executable", "bytes_hex": bytes_hex, "display": name}})
}

pub fn worker_observation(observation: &Observation) -> Value {
    let pair = |words: [u64; 2]| [words[0].to_string(), words[1].to_string()];
    let text = |value: Option<u64>| value.map(|value| value.to_string());

    json!({"recording": pair(observation.recording),
        "stamp": {"boot": pair(observation.stamp.boot), "run": pair(observation.stamp.run),
            "publication": observation.stamp.publication.to_string(),
            "continuous_ns": text(observation.stamp.continuous_ns),
            "awake_ns": text(observation.stamp.awake_ns)},
        "run_tick": observation.run_tick.to_string(),
        "wall_ns": text(observation.wall_ns)})
}

/// The Apps of `second`, each of `apps` an executable of that name that spent that many
/// nanoseconds of CPU time in the second that ends there, held as many bytes resident and used
/// nothing else, every process of the second found. `apps` is in key order.
pub fn apps_at(second: &MachineSecond, apps: &[(&str, u64)]) -> AppsSecond {
    let rows: Vec<AppRow> = apps
        .iter()
        .map(|(name, cpu_ns)| apps_at_row(name, *cpu_ns))
        .collect();

    AppsSecond {
        missing: None,
        observation: Some(apps_at_observation(second)),
        consecutive: true,
        elapsed_ns: Some(NANOSECONDS_PER_SECOND),
        membership_complete: true,
        ungrouped_members: 0,
        live_ungrouped_members: 0,
        usage_refused: 0,
        usage_gone: 0,
        totals: Totals::of(&rows),
        apps: rows,
    }
}

/// The tick of `second`, of the run and boot the trend points of `selection` name.
pub fn apps_at_observation(second: &MachineSecond) -> Observation {
    Observation {
        recording: [5, 6],
        stamp: Stamp {
            boot: [3, 4],
            run: [1, 2],
            publication: second.publication,
            continuous_ns: second.continuous_ns,
            awake_ns: second.awake_ns,
        },
        run_tick: second.run_tick,
        wall_ns: Some(second.wall_ns),
    }
}

/// The App of one process, the executable `name`, that spent `cpu_ns` of CPU time and held as
/// many bytes resident.
fn apps_at_row(name: &str, cpu_ns: u64) -> AppRow {
    let measure = |known| Measure {
        known,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    let app = App {
        key: Key {
            kind: KeyKind::Executable,
            bytes: name.as_bytes().to_vec(),
            display: name.into(),
        },
        display_name: name.into(),
        members: 1,
        live_members: 1,
        membership_complete: true,
        cpu_ns: measure(cpu_ns),
        energy_nj: measure(0),
        gpu_ns: measure(0),
        disk_read_bytes: measure(0),
        disk_written_bytes: measure(0),
        disk_io_bytes: measure(0),
        resident_bytes: measure(cpu_ns),
        footprint_bytes: measure(0),
    };
    let ranked = Arc::new(RankedApp {
        kind: KeyKind::Executable,
        bytes: app.key.bytes.clone(),
        display_name: app.display_name.clone(),
    });

    AppRow::new(&app, ranked)
}
