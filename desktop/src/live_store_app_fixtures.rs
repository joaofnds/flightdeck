//! The Apps of seconds the store holds and the worker's App histories of them, which the tests
//! of Overview's App charts drawn from the store compare with the charts the worker's histories
//! draw.

use std::rc::Rc;
use std::sync::Arc;

use num_traits::ToPrimitive;
use serde_json::{Value, json};
use viewer::apps::Stamp;
use viewer::live_app_response::{App, Key, KeyKind, Measure, Observation, RankedApp};
use viewer::store::{AppRow, AppsSecond, AppsStretch, Cohorts, MachineSecond, Totals, View};

use crate::data::Panel;
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
/// holding no Apps, and the cohorts that draw A and B on lines of their own on the CPU chart.
pub fn charted(seconds: &[MachineSecond; 4]) -> (View, Cohorts) {
    let mut view = view(1, &[], seconds);
    view.apps_loaded = true;
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
    let cohorts = Cohorts {
        seconds: seconds.len(),
        cpu: vec![ranked("A"), ranked("B")],
        ..Cohorts::default()
    };

    (view, cohorts)
}

/// The View of `seconds`, at each of which the executables A, B and C used 0.5, 0.25 and 0.125 s
/// of CPU, and the cohorts chosen over all of them that draw the Apps `cohort` names on lines of
/// their own on the CPU chart and the chart of resident memory. Of the seconds of `seconds()`,
/// only 102 follows the one before it.
pub fn sliding(seconds: &[MachineSecond], cohort: &[&str]) -> (View, Cohorts) {
    let mut view = view(1, &[], seconds);
    view.apps_loaded = true;
    let used = [("A", 500_000_000), ("B", 250_000_000), ("C", 125_000_000)];
    for (apps, second) in view.apps.iter_mut().zip(seconds) {
        *apps = Arc::new(AppsSecond {
            consecutive: second.wall_ns / NANOSECONDS_PER_SECOND == 102,
            ..apps_at(second, &used)
        });
    }
    let cpu: Vec<_> = cohort.iter().map(|name| ranked(name)).collect();
    let cohorts = Cohorts {
        seconds: seconds.len(),
        resident: cpu.clone(),
        cpu,
        ..Cohorts::default()
    };

    (view, cohorts)
}

/// The executable `name`, as a chart's cohort names it.
pub fn ranked(name: &str) -> Arc<RankedApp> {
    Arc::new(RankedApp::named(
        KeyKind::Executable,
        name.as_bytes().to_vec(),
        name.into(),
    ))
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
        metadata: viewer::store::AppMetadata::default(),
        processes: Err(viewer::store::Missing::Unread),
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
        apps: rows.try_into().unwrap(),
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
    let ranked = Arc::new(RankedApp::named(
        KeyKind::Executable,
        app.key.bytes.clone(),
        app.display_name.clone(),
    ));

    AppRow::new(&app, ranked)
}

/// The newest second of `hour()`.
pub const HOUR_NEWEST: u64 = 10_000;

/// The View of a store that holds the loaded hour up to `HOUR_NEWEST`, and its cohorts, as
/// `hour_until` gives them.
pub fn hour() -> (View, Cohorts) {
    hour_until(HOUR_NEWEST)
}

/// The View of a store that holds the loaded hour up to `newest`, a multiple of ten: the first
/// seconds of the two stretches before its live span, the 981 seconds up to `newest` in it, and
/// the Apps of each closed stretch. At each second the executables A, B and C hold as many bytes
/// resident as they use nanoseconds of CPU: B 0.25 s and C 0.125 s, and A 0.5 s and a further
/// 10 ms for each second past the start of its stretch, but in stretch 900, where A uses 0.07 s
/// at its first second and 0.27 s at each of the other nine, 0.25 s a second over the stretch.
/// The cohorts draw A and B on lines of their own on the CPU chart and the chart of resident
/// memory.
pub fn hour_until(newest: u64) -> (View, Cohorts) {
    assert_eq!(newest % 10, 0);
    let apps = |seconds: std::ops::Range<u64>| -> Vec<Arc<AppsSecond>> {
        seconds
            .map(|second| Arc::new(apps_at(&reading_at(second, 0), &hour_until_used(second))))
            .collect()
    };

    let live_first = newest - 980;
    let earlier = [
        reading_at(live_first - 20, 0),
        reading_at(live_first - 10, 0),
    ];
    let live: Vec<MachineSecond> = (live_first..=newest)
        .map(|second| reading_at(second, 0))
        .collect();
    let mut view = view(1, &earlier, &live);
    view.apps_loaded = true;
    view.apps = apps(live_first..newest + 1);
    let older = [
        apps(live_first - 20..live_first - 10),
        apps(live_first - 10..live_first),
    ];
    view.stretches = older
        .iter()
        .map(Vec::as_slice)
        .chain(view.apps[..view.apps.len() - 1].chunks(10))
        .zip((live_first - 20) / 10..)
        .map(|(seconds, stretch)| Arc::new(AppsStretch::of_seconds(stretch, seconds)))
        .collect();
    let cohorts = Cohorts {
        seconds: 960,
        cpu: vec![ranked("A"), ranked("B")],
        resident: vec![ranked("A"), ranked("B")],
        ..Cohorts::default()
    };

    (view, cohorts)
}

/// What A, B and C use at `second` of `hour_until`'s View.
fn hour_until_used(second: u64) -> [(&'static str, u64); 3] {
    let a = match second {
        9_000 => 70_000_000,
        9_001..9_010 => 270_000_000,
        _ => 500_000_000 + second % 10 * 10_000_000,
    };

    [("A", a), ("B", 250_000_000), ("C", 125_000_000)]
}

/// The View of `hour()`, in whose two stretches before its live span an executable D also uses
/// 2 s of CPU and holds 2 GB resident at each second, so D leads the charts by App over the hour
/// and is absent from its newest fifteen minutes. Its cohorts draw on lines of their own on the
/// CPU chart A, B and C, which led over the newest 960 seconds.
pub fn hour_led_from_its_older_part() -> View {
    let (mut view, _) = hour();
    for stretch in &mut view.stretches[..2] {
        let first = stretch.stretch * 10;
        let seconds: Vec<_> = (first..first + 10)
            .map(|second| {
                let [a, b, c] = hour_until_used(second);
                let used = [a, b, c, ("D", 2_000_000_000)];
                Arc::new(apps_at(&reading_at(second, 0), &used))
            })
            .collect();
        *stretch = Arc::new(AppsStretch::of_seconds(stretch.stretch, &seconds));
    }
    view.cohorts = Cohorts {
        seconds: 960,
        cpu: vec![ranked("A"), ranked("B"), ranked("C")],
        ..Cohorts::default()
    };

    view
}

/// `panel` with where it draws each reading given as a wall millisecond rather than as a share
/// of its span, and without its help, which says what it chose its leaders over, so charts of
/// the same readings drawn across different spans, from the store's View and by the worker,
/// compare equal.
pub fn on_the_clock(panel: Option<Rc<Panel>>) -> Option<(Panel, Vec<u64>)> {
    let mut panel = Rc::unwrap_or_clone(panel?);
    panel.help.take()?;
    let window = panel.window.take()?;
    let positions = panel.positions.take()?;
    let first = f64::from(u32::try_from(window.first).unwrap());
    let span = f64::from(u32::try_from(window.span).unwrap());
    let milliseconds = positions
        .iter()
        .map(|position| {
            let second = first + f64::from(*position) * span;
            (second * 1_000.0).round().to_u64().unwrap()
        })
        .collect();

    Some((panel, milliseconds))
}
