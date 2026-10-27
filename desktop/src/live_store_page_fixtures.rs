//! The worker's readings of seconds the store holds and what the page shows of them, which the
//! tests of the page drawn from the store's View compare it with.

use std::collections::BTreeMap;

use collector::machine::Pressure;
use serde_json::{Value, json};
use viewer::store::{BatterySecond, MachineSecond, MemorySecond};

use crate::hardware::Readings;
use crate::live_source::Snapshot;
use crate::live_store_fixtures::{reading_at, selection, trend_point};
use crate::live_views::summary;

/// The worker's readings of `trends`, following the newest of them.
pub fn readings(trends: &[MachineSecond]) -> Box<Snapshot> {
    let mut selection = selection(&[], trends);
    selection["summary"] = json!({"memory_total_bytes": 34_359_738_368_u64});

    Box::new(Snapshot {
        generation: 0,
        catalog: json!({"first_second": 9_000, "last_second": 10_000}),
        selection: crate::live_source::Presentation::Legacy(selection),
        pages: (BTreeMap::from([(
            "ticks".into(),
            json!({"offset":0,"total":1,"rows":[["1"]],"columns":["publication"]}),
        )]))
        .into_iter()
        .map(|(key, value)| (key, crate::live_source::Presentation::Legacy(value)))
        .collect(),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        cpu_history: None,
        resident_history: None,
        app_attribution_history: None,
        overview_app_histories: None,
        histories_pending: false,
        process_history: None,
        rankings: None,
        app_ranks: None,
        app_parts: None,
        read_ms: 0.0,
    })
}

/// The second at `wall_second` whose hardware figure shows CPU and GPU dies at 61.5 and 52.25 °C,
/// fans at 1,200 and 1,500 rpm, the CPU 25% and the GPU 12.5% busy, 12 of 32 GiB in use, the
/// battery 75% charged and giving 6.5 W, and moderate thermal pressure.
pub fn figure_second(wall_second: u64) -> MachineSecond {
    const GIBIBYTE: u64 = 1_073_741_824;

    MachineSecond {
        cpu_celsius: Some(61.5),
        gpu_celsius: Some(52.25),
        fan_rpm: [Some(1_200.0), Some(1_500.0)],
        cpu_percent: Some(25.0),
        gpu_active_percent: Some(12.5),
        memory: Some(MemorySecond {
            total_bytes: 32 * GIBIBYTE,
            app_bytes: Some(8 * GIBIBYTE),
            wired_bytes: 2 * GIBIBYTE,
            compressed_bytes: 2 * GIBIBYTE,
            pressure: Pressure::Normal,
            swap_used_bytes: 0,
            swap_total_bytes: 0,
        }),
        battery: Some(BatterySecond {
            charge_percent: Some(75.0),
            power_watts: Some(-6.5),
        }),
        thermal_pressure: Some(1),
        ..reading_at(wall_second, 0)
    }
}

/// The hardware figure's readings of `figure_second`.
pub fn figure_readings() -> Readings {
    Readings {
        temperatures: [Some(61.5), Some(52.25), None],
        speeds: [Some(1_200.0), Some(1_500.0)],
        utilization: [Some(25.0), Some(12.5)],
        memory: [Some(12.0), Some(32.0)],
        battery: [Some(75.0), Some(-6.5)],
        thermal: "Moderate".into(),
    }
}

/// What each of `cards` shows, in order.
pub fn texts(cards: &[summary::Card]) -> Vec<(String, String, String, u32)> {
    cards
        .iter()
        .map(|card| {
            let label = card.label.clone();
            (label, card.reading.clone(), card.note.clone(), card.color)
        })
        .collect()
}

/// The selection of `second` alone, which the worker's readings of that second summarize.
pub fn summary_selection(second: &MachineSecond) -> Value {
    let mut selection = selection(&[], std::slice::from_ref(second));
    selection["summary"] = trend_point(second);
    selection
}
