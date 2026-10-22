use serde_json::Value;

use crate::charts::{BLUE, GREEN, MUTED, ORANGE, PURPLE, RED, TEXT, YELLOW};

pub struct Card {
    pub label: String,
    pub reading: String,
    pub note: String,
    pub color: u32,
}

#[derive(Clone, Copy)]
enum Reading {
    /// The busy share of every core, noted with the count of cores macOS reports.
    AllCores,
    Quantity(&'static str, &'static str, f32),
    /// A quantity of one resource's readings, such as the battery's.
    Resource(&'static str, &'static str, &'static str, f32),
    Charging,
    /// The temperature of one die, noted with the share of its time spent busy.
    DieTemperature(&'static str, &'static str),
    Health,
    /// The busy share of one performance level, by its index fastest first.
    Level(usize),
    /// The one-minute load average of the selected second.
    LoadAverage,
    MemoryInUse,
    Pressure,
    /// macOS's thermal pressure level at the selected second.
    Thermal,
    Uncaptured,
}

type Specification = (&'static str, Reading, &'static str, u32);

const CPU: Specification = ("CPU", Reading::AllCores, "All captured cores", BLUE);
const GPU: Specification = (
    "GPU",
    Reading::Quantity("gpu_active_percent", "%", 1.0),
    "Active time",
    PURPLE,
);
const OVERVIEW_CPU: Specification = (
    "CPU",
    Reading::DieTemperature("cpu_celsius", "cpu_percent"),
    "",
    BLUE,
);
const OVERVIEW_GPU: Specification = (
    "GPU",
    Reading::DieTemperature("gpu_celsius", "gpu_active_percent"),
    "",
    PURPLE,
);
const PRESSURE: Specification = ("Memory pressure", Reading::Pressure, "macOS verdict", GREEN);
const POWER: Specification = (
    "System power",
    Reading::Quantity("power_watts", "W", 1.0),
    "Whole-machine sensor",
    ORANGE,
);
const DISK_READ: Specification = (
    "Disk read",
    Reading::Resource("storage", "read_bps", "MB/s", 1_000_000.0),
    "Machine device I/O",
    YELLOW,
);
const RECEIVE: Specification = (
    "Receive",
    Reading::Quantity("network_rx_bps", "MB/s", 1_000_000.0),
    "External interfaces",
    BLUE,
);
const SEND: Specification = (
    "Send",
    Reading::Quantity("network_tx_bps", "MB/s", 1_000_000.0),
    "External interfaces",
    GREEN,
);
const PERFORMANCE: Specification = (
    "Core level 1",
    Reading::Level(1),
    "Core levels unreadable",
    GREEN,
);
const SUPER: Specification = (
    "Core level 0",
    Reading::Level(0),
    "Core levels unreadable",
    PURPLE,
);
const LOAD: Specification = (
    "Load average",
    Reading::LoadAverage,
    "1 minute · running or waiting",
    TEXT,
);
const GPU_FREQUENCY: Specification = (
    "Active frequency",
    Reading::Uncaptured,
    "Cycles / active time",
    YELLOW,
);
const GPU_MEMORY: Specification = (
    "Memory in use",
    Reading::Quantity("gpu_memory_in_use_bytes", "GiB", 1_073_741_824.0),
    "Unified-memory allocation",
    PURPLE,
);
const GPU_POWER: Specification = (
    "GPU power",
    Reading::Quantity("gpu_watts", "W", 1.0),
    "Recorded energy / second",
    PURPLE,
);
const GPU_PAGE_POWER: Specification = (
    "GPU power",
    Reading::Quantity("gpu_watts", "W", 1.0),
    "Recorded energy / second",
    ORANGE,
);
const IN_USE: Specification = ("Memory in use", Reading::MemoryInUse, "", GREEN);
const COMPRESSED: Specification = (
    "Compressed",
    Reading::Quantity("memory_compressed_bytes", "GiB", 1_073_741_824.0),
    "Included in memory in use",
    PURPLE,
);
const SWAP: Specification = (
    "Swap used",
    Reading::Quantity("swap_used_bytes", "GiB", 1_073_741_824.0),
    "On the startup disk",
    TEXT,
);
const PROCESS_POWER: Specification = (
    "Process power",
    Reading::Uncaptured,
    "Whole-process total not captured",
    YELLOW,
);
const THERMAL: Specification = ("Thermal state", Reading::Thermal, "macOS verdict", MUTED);
const RECEIVED_PACKETS: Specification = (
    "Received packets",
    Reading::Quantity("network_rx_packets_per_second", "pps", 1.0),
    "Packet count / second",
    BLUE,
);
const SENT_PACKETS: Specification = (
    "Sent packets",
    Reading::Quantity("network_tx_packets_per_second", "pps", 1.0),
    "Packet count / second",
    GREEN,
);

const DISK_WRITE: Specification = (
    "Disk write",
    Reading::Resource("storage", "write_bps", "MB/s", 1_000_000.0),
    "Machine device I/O",
    ORANGE,
);
const READ_OPERATIONS: Specification = (
    "Read operations",
    Reading::Resource("storage", "read_ops_per_second", "ops/s", 1.0),
    "Completed requests",
    YELLOW,
);
const WRITE_OPERATIONS: Specification = (
    "Write operations",
    Reading::Resource("storage", "write_ops_per_second", "ops/s", 1.0),
    "Completed requests",
    ORANGE,
);
const CHARGE: Specification = (
    "Charge",
    Reading::Resource("battery", "charge_percent", "%", 1.0),
    "Reported full capacity",
    GREEN,
);
const CHARGING: Specification = (
    "Charging",
    Reading::Charging,
    "Battery-controller state",
    TEXT,
);
const FLOW: Specification = (
    "Battery flow",
    Reading::Resource("battery", "power_watts", "W", 1.0),
    "Positive in · negative out",
    ORANGE,
);
const HEALTH: Specification = ("Health", Reading::Health, "", GREEN);

pub fn cards(route: &str, selection: &Value) -> Vec<Card> {
    let summary = matching_summary(selection);
    cards_definitions(route)
        .iter()
        .map(|definition| card(summary, definition))
        .collect()
}

/// The cards of `route` while its first reading is still being read. Each says so in place of
/// its reading, and a note that would carry a reading keeps only its own words.
pub fn loading_cards(route: &str) -> Vec<Card> {
    cards_definitions(route)
        .iter()
        .map(|definition| {
            let (_, source, note, _) = *definition;
            let mut card = card(&Value::Null, definition);
            let reads = matches!(
                source,
                Reading::DieTemperature(..) | Reading::Health | Reading::MemoryInUse
            );
            if reads {
                note.clone_into(&mut card.note);
            }
            loading(&mut card);
            card
        })
        .collect()
}

/// Makes `card` say its reading is still being read.
pub fn loading(card: &mut Card) {
    crate::live_dashboard::LOADING.clone_into(&mut card.reading);
    card.color = MUTED;
}

fn cards_definitions(route: &str) -> &'static [Specification] {
    let definitions = match route {
        "overview" => &[
            OVERVIEW_CPU,
            OVERVIEW_GPU,
            PRESSURE,
            POWER,
            DISK_READ,
            RECEIVE,
        ][..],
        "cpu" => &[CPU, PERFORMANCE, SUPER, LOAD][..],
        "gpu" => &[GPU, GPU_FREQUENCY, GPU_MEMORY, GPU_PAGE_POWER][..],
        "memory" => &[PRESSURE, IN_USE, COMPRESSED, SWAP][..],
        "storage" => &[DISK_READ, DISK_WRITE, READ_OPERATIONS, WRITE_OPERATIONS][..],
        "battery" => &[CHARGE, CHARGING, FLOW, HEALTH][..],
        "energy" => &[POWER, GPU_POWER, PROCESS_POWER, THERMAL][..],
        "network" => &[RECEIVE, SEND, RECEIVED_PACKETS, SENT_PACKETS][..],
        _ => &[],
    };
    assert!(definitions.len() <= 6);
    definitions
}

fn card(summary: &Value, definition: &Specification) -> Card {
    let (label, source, note, color) = *definition;
    let mut label = label.to_owned();
    let mut color = color;
    let mut note = note.to_owned();
    let reading = match source {
        Reading::AllCores => {
            if let Some(levels) = crate::live_inventory::performance_levels(summary) {
                let cores = levels[0].logical_cpus + levels[1].logical_cpus;
                note = format!("All {cores} cores");
            }
            crate::live_projection::quantity(summary, "cpu_percent", "%", 1.0)
        }
        Reading::Quantity(key, unit, divisor) => {
            crate::live_projection::quantity(summary, key, unit, divisor)
        }
        Reading::Resource(resource, key, unit, divisor) => {
            crate::live_projection::quantity(&summary[resource], key, unit, divisor)
        }
        Reading::Charging => card_charging(summary).into(),
        Reading::DieTemperature(temperature, utilization) => {
            note = card_utilization(summary, utilization);
            crate::live_projection::hardware_reading(summary, temperature, " °C", 1)
        }
        Reading::Health => {
            note = match summary["battery"]["cycle_count"].as_u64() {
                Some(cycles) => format!("{cycles} cycles"),
                None => "Cycles unavailable".into(),
            };
            crate::live_projection::quantity(&summary["battery"], "health_percent", "%", 1.0)
        }
        Reading::Level(index) => match crate::live_inventory::performance_levels(summary) {
            Some(levels) => {
                let level = &levels[index];
                label.clone_from(&level.cores);
                note = format!("{} cores · cluster capacity", level.logical_cpus);
                card_level(summary, index)
            }
            None => "Unavailable".into(),
        },
        Reading::MemoryInUse => {
            note = card_memory_installed(summary);
            card_memory_in_use(summary)
        }
        Reading::Pressure => {
            color = pressure_color(summary);
            crate::live_views::pressure(&summary["pressure"]).into()
        }
        Reading::LoadAverage => {
            let load = serde_json::json!({ "load": summary["load_average"][0] });
            crate::live_projection::quantity(&load, "load", "work items", 1.0)
        }
        Reading::Thermal => {
            color = thermal_color(summary);
            crate::live_thermal_pressure::thermal_pressure_text(summary).into()
        }
        Reading::Uncaptured => "Unavailable".into(),
    };
    if reading == "Unavailable" {
        color = MUTED;
    }

    Card {
        label,
        reading,
        note,
        color,
    }
}

/// The note under a temperature card: how busy the same die is.
fn card_utilization(summary: &Value, key: &str) -> String {
    let load = crate::live_projection::quantity(summary, key, "%", 1.0);
    format!("Utilization {load}")
}

fn card_charging(summary: &Value) -> &'static str {
    match summary["battery"]["charging"].as_bool() {
        Some(true) => "Yes",
        Some(false) => "No",
        None => "Unavailable",
    }
}

/// Green at normal memory pressure, yellow at warning and red at critical.
fn pressure_color(summary: &Value) -> u32 {
    match summary["pressure"].as_u64() {
        Some(0) => GREEN,
        Some(1) => YELLOW,
        Some(2) => RED,
        _ => MUTED,
    }
}

/// Green at nominal pressure, then yellow, orange and red as macOS asks for less heat.
fn thermal_color(summary: &Value) -> u32 {
    match summary["thermal_pressure"].as_u64() {
        Some(0) => GREEN,
        Some(1) => YELLOW,
        Some(2) => ORANGE,
        Some(3 | 4) => RED,
        _ => MUTED,
    }
}

/// The busy share of the performance level at `index`, fastest first.
fn card_level(summary: &Value, index: usize) -> String {
    let percent = serde_json::json!({ "percent": summary["cpu_level_percent"][index] });
    crate::live_projection::quantity(&percent, "percent", "%", 1.0)
}

fn card_memory_installed(summary: &Value) -> String {
    let installed =
        crate::live_projection::quantity(summary, "memory_total_bytes", "GiB", 1_073_741_824.0);
    format!("Of {installed} installed")
}

/// Memory in use as the prototype and Activity Monitor count it: app memory, wired and
/// compressed. Any one of them missing leaves the total unavailable.
fn card_memory_in_use(summary: &Value) -> String {
    match memory_in_use_bytes(summary) {
        Some(bytes) => crate::live_projection::quantity(
            &serde_json::json!({ "bytes": bytes }),
            "bytes",
            "GiB",
            1_073_741_824.0,
        ),
        None => "Unavailable".into(),
    }
}

/// App memory, wired and compressed memory together, or none where any one is missing.
pub(crate) fn memory_in_use_bytes(summary: &Value) -> Option<u64> {
    let anonymous = summary["memory_anonymous_bytes"].as_u64();
    let purgeable = summary["memory_purgeable_bytes"].as_u64();
    let app = anonymous
        .zip(purgeable)
        .and_then(|(anonymous, purgeable)| anonymous.checked_sub(purgeable));
    let wired = summary["memory_wired_bytes"].as_u64();
    let compressed = summary["memory_compressed_bytes"].as_u64();
    app.zip(wired)
        .and_then(|(app, wired)| app.checked_add(wired))
        .zip(compressed)
        .and_then(|(used, compressed)| used.checked_add(compressed))
}

/// The summary only where it is the selected observation's own: the same run, boot and
/// publication, and the same wall clock, so a sibling observation never lends its readings.
pub(crate) fn matching_summary(selection: &Value) -> &Value {
    let selected = &selection["selected"];
    let summary = &selection["summary"];
    for field in ["run", "boot", "publication"] {
        if selected[field].is_null() {
            return &Value::Null;
        }
        if selected[field] != summary[field] {
            return &Value::Null;
        }
    }

    if selected["wall_ns"].is_null() {
        let (Some(wall), Some(second)) =
            (summary["wall_ns"].as_u64(), selection["second"].as_u64())
        else {
            return &Value::Null;
        };

        if wall / 1_000_000_000 != second {
            return &Value::Null;
        }
    } else {
        if selected["wall_ns"] != summary["wall_ns"] {
            return &Value::Null;
        }
    }

    summary
}

#[cfg(test)]
#[path = "live_summary_tests.rs"]
mod tests;
