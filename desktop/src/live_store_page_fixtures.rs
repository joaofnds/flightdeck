//! The worker's readings of seconds the store holds and what the page shows of them, which the
//! tests of the page drawn from the store's View compare it with.

use collector::machine::Pressure;
use serde_json::{Value, json};
use viewer::store::{BatterySecond, MachineSecond, MemorySecond};

use crate::hardware::Readings;
use crate::live_source::Snapshot;
use crate::live_store_fixtures::{
    reading_at, selection, store_levels_named, trend_point, with_cpu_reading,
};
use crate::live_views::summary;

/// The worker's readings of `trends`, following the newest of them.
pub fn readings(trends: &[MachineSecond]) -> Box<Snapshot> {
    let mut selection = selection(&[], trends);
    selection["summary"] = json!({"memory_total_bytes": 34_359_738_368_u64});

    Box::new(Snapshot {
        generation: 0,
        catalog: json!({"first_second": 9_000, "last_second": 10_000}),
        selection: crate::live_source::Presentation::Legacy(viewer::Selection::from_json(
            selection,
        )),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        cpu_history: None,
        resident_history: None,
        app_attribution_history: None,
        overview_app_histories: None,
        histories_pending: false,
        points_keeping: crate::live_source::PointsKeeping::Kept,
        app_chart_source: crate::live_source::AppChartSource::Reading,
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

/// The seconds of `wall_seconds`, each with a complete reading of its CPU and the logical CPUs of
/// the interval since the second before.
pub fn cpu_seconds(wall_seconds: std::ops::RangeInclusive<u64>) -> Vec<MachineSecond> {
    let first = *wall_seconds.start();
    cpu_run([1, 2], first, wall_seconds)
}

/// The seconds of `wall_seconds` of run `run`, numbered from publication `first_publication`,
/// each with a complete reading of its CPU and the logical CPUs of the interval since the second
/// before.
pub fn cpu_run(
    run: [u64; 2],
    first_publication: u64,
    wall_seconds: std::ops::RangeInclusive<u64>,
) -> Vec<MachineSecond> {
    let levels = store_levels_named("Super", "Performance");

    cpu_run_of_levels(run, first_publication, wall_seconds, levels)
}

/// The logical CPUs of each level of the Mac of `cpu_seconds_past_a_page`.
const PAST_A_PAGE_LOGICAL_CPUS_PER_LEVEL: u32 = 20;

const _: () = {
    assert!(2 * PAST_A_PAGE_LOGICAL_CPUS_PER_LEVEL > crate::live_cpu_history::PAGE_CPUS);
};

/// The seconds of `wall_seconds`, as `cpu_seconds` gives them, of a Mac with more logical CPUs
/// than one page of the heatmap holds.
pub fn cpu_seconds_past_a_page(wall_seconds: std::ops::RangeInclusive<u64>) -> Vec<MachineSecond> {
    let first = *wall_seconds.start();
    let mut levels = store_levels_named("Super", "Performance");
    for level in &mut levels {
        level.logical_cpus = PAST_A_PAGE_LOGICAL_CPUS_PER_LEVEL;
    }

    cpu_run_of_levels([1, 2], first, wall_seconds, levels)
}

/// The seconds of `cpu_run`, of a Mac whose CPUs `levels` count.
fn cpu_run_of_levels(
    run: [u64; 2],
    first_publication: u64,
    wall_seconds: std::ops::RangeInclusive<u64>,
    levels: [collector::system::PerformanceLevel; 2],
) -> Vec<MachineSecond> {
    let first = *wall_seconds.start();
    let at = |wall_second: u64| {
        let mut second = reading_at(wall_second, 0);
        second.run = run;
        second.publication = wall_second + first_publication - first;
        second.run_tick = second.publication;
        second.performance_levels = Some(levels);
        second
    };
    let mut seconds = Vec::new();
    let mut previous = at(first - 1);
    for wall_second in wall_seconds {
        let second = with_logical_cpus(with_cpu_reading(at(wall_second)), &previous);
        previous = second.clone();
        seconds.push(second);
    }

    seconds
}

/// `second`, which `with_cpu_reading` gave a complete reading, with each logical CPU its
/// performance levels name busy over the interval since `previous` for a share that differs from
/// one CPU to the next and from one second to the next.
fn with_logical_cpus(mut second: MachineSecond, previous: &MachineSecond) -> MachineSecond {
    let [fastest, slower] = second.performance_levels.as_ref().unwrap();
    let wall_second = second.wall_ns / crate::live_store_fixtures::NANOSECONDS_PER_SECOND;
    let busy_percent = (0..fastest.logical_cpus + slower.logical_cpus)
        .map(|cpu| {
            Some(f64::from(
                u32::try_from((wall_second + u64::from(cpu)) % 80).unwrap(),
            ))
        })
        .collect();
    let viewer::store::CompleteMachineState::Available(complete) = second.complete else {
        panic!("only a second with a complete reading holds its logical CPUs");
    };
    second.complete = viewer::store::CompleteMachineState::Available(
        viewer::store::CompleteMachine::with_cpu_interval(complete, busy_percent, previous),
    );
    second
}
