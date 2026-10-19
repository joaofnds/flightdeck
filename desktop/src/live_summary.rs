use serde_json::Value;

use crate::charts::{BLUE, GREEN, MUTED, ORANGE, PURPLE, RED, TEXT, YELLOW};

pub struct Card {
    pub label: &'static str,
    pub reading: String,
    pub note: String,
    pub color: u32,
    pub route: &'static str,
}

#[derive(Clone, Copy)]
enum Reading {
    /// The busy share of every core, noted with the count of cores macOS reports.
    AllCores,
    Quantity(&'static str, &'static str, f32),
    /// A quantity of one resource's readings, such as the battery's.
    Resource(&'static str, &'static str, &'static str, f32),
    Charging,
    Health,
    /// The busy share of one performance level, by its index fastest first.
    Level(usize),
    /// The one-minute load average the app sampled in the selected second.
    LoadAverage,
    MemoryInUse,
    Pressure,
    /// macOS's thermal pressure level the app sampled in the selected second.
    Thermal,
    Uncaptured,
}

type Specification = (&'static str, Reading, &'static str, u32, &'static str);

const CPU: Specification = ("CPU", Reading::AllCores, "All captured cores", BLUE, "cpu");
const GPU: Specification = (
    "GPU",
    Reading::Quantity("gpu_active_percent", "%", 1.0),
    "Active time",
    PURPLE,
    "gpu",
);
const PRESSURE: Specification = (
    "Memory pressure",
    Reading::Pressure,
    "macOS verdict",
    GREEN,
    "memory",
);
const POWER: Specification = (
    "System power",
    Reading::Quantity("power_watts", "W", 1.0),
    "Whole-machine sensor",
    ORANGE,
    "energy",
);
const DISK_READ: Specification = (
    "Disk read",
    Reading::Resource("storage", "read_bps", "MB/s", 1_000_000.0),
    "Machine device I/O",
    YELLOW,
    "storage",
);
const RECEIVE: Specification = (
    "Receive",
    Reading::Quantity("network_rx_bps", "MB/s", 1_000_000.0),
    "External interfaces",
    BLUE,
    "network",
);
const SEND: Specification = (
    "Send",
    Reading::Quantity("network_tx_bps", "MB/s", 1_000_000.0),
    "External interfaces",
    GREEN,
    "network",
);
const PERFORMANCE: Specification = (
    "Core level 1",
    Reading::Level(1),
    "Core levels unreadable",
    GREEN,
    "cpu",
);
const SUPER: Specification = (
    "Core level 0",
    Reading::Level(0),
    "Core levels unreadable",
    PURPLE,
    "cpu",
);
const LOAD: Specification = (
    "Load average",
    Reading::LoadAverage,
    "1 minute · running or waiting",
    TEXT,
    "cpu",
);
const GPU_FREQUENCY: Specification = (
    "Active frequency",
    Reading::Uncaptured,
    "Cycles / active time",
    YELLOW,
    "gpu",
);
const GPU_MEMORY: Specification = (
    "Memory in use",
    Reading::Quantity("gpu_memory_in_use_bytes", "GiB", 1_073_741_824.0),
    "Unified-memory allocation",
    PURPLE,
    "gpu",
);
const GPU_POWER: Specification = (
    "GPU power",
    Reading::Quantity("gpu_watts", "W", 1.0),
    "Recorded energy / second",
    PURPLE,
    "gpu",
);
const GPU_PAGE_POWER: Specification = (
    "GPU power",
    Reading::Quantity("gpu_watts", "W", 1.0),
    "Recorded energy / second",
    ORANGE,
    "gpu",
);
const IN_USE: Specification = ("Memory in use", Reading::MemoryInUse, "", GREEN, "memory");
const COMPRESSED: Specification = (
    "Compressed",
    Reading::Quantity("memory_compressed_bytes", "GiB", 1_073_741_824.0),
    "Included in memory in use",
    PURPLE,
    "memory",
);
const SWAP: Specification = (
    "Swap used",
    Reading::Quantity("swap_used_bytes", "GiB", 1_073_741_824.0),
    "On the startup disk",
    TEXT,
    "memory",
);
const PROCESS_POWER: Specification = (
    "Process power",
    Reading::Uncaptured,
    "Whole-process total not captured",
    YELLOW,
    "energy",
);
const THERMAL: Specification = (
    "Thermal state",
    Reading::Thermal,
    "macOS verdict",
    MUTED,
    "energy",
);
const RECEIVED_PACKETS: Specification = (
    "Received packets",
    Reading::Quantity("network_rx_packets_per_second", "pps", 1.0),
    "Packet count / second",
    BLUE,
    "network",
);
const SENT_PACKETS: Specification = (
    "Sent packets",
    Reading::Quantity("network_tx_packets_per_second", "pps", 1.0),
    "Packet count / second",
    GREEN,
    "network",
);

const DISK_WRITE: Specification = (
    "Disk write",
    Reading::Resource("storage", "write_bps", "MB/s", 1_000_000.0),
    "Machine device I/O",
    ORANGE,
    "storage",
);
const READ_OPERATIONS: Specification = (
    "Read operations",
    Reading::Resource("storage", "read_ops_per_second", "ops/s", 1.0),
    "Completed requests",
    YELLOW,
    "storage",
);
const WRITE_OPERATIONS: Specification = (
    "Write operations",
    Reading::Resource("storage", "write_ops_per_second", "ops/s", 1.0),
    "Completed requests",
    ORANGE,
    "storage",
);
const CHARGE: Specification = (
    "Charge",
    Reading::Resource("battery", "charge_percent", "%", 1.0),
    "Reported full capacity",
    GREEN,
    "battery",
);
const CHARGING: Specification = (
    "Charging",
    Reading::Charging,
    "Battery-controller state",
    TEXT,
    "battery",
);
const FLOW: Specification = (
    "Battery flow",
    Reading::Resource("battery", "power_watts", "W", 1.0),
    "Positive in · negative out",
    ORANGE,
    "battery",
);
const HEALTH: Specification = ("Health", Reading::Health, "", GREEN, "battery");

pub fn cards(route: &str, selection: &Value) -> Vec<Card> {
    let summary = matching_summary(selection);
    let definitions = match route {
        "overview" => &[CPU, GPU, PRESSURE, POWER, DISK_READ, RECEIVE][..],
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
        .iter()
        .map(|definition| card(summary, definition))
        .collect()
}

fn card(summary: &Value, definition: &Specification) -> Card {
    let (mut label, source, note, color, route) = *definition;
    let mut color = color;
    let mut note = note.to_owned();
    let reading = match source {
        Reading::AllCores => {
            if let Some(levels) = crate::live_inventory::performance_levels() {
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
        Reading::Health => {
            note = match summary["battery"]["cycle_count"].as_u64() {
                Some(cycles) => format!("{cycles} cycles"),
                None => "Cycles unavailable".into(),
            };
            crate::live_projection::quantity(&summary["battery"], "health_percent", "%", 1.0)
        }
        Reading::Level(index) => match crate::live_inventory::performance_levels() {
            Some(levels) => {
                let level = &levels[index];
                label = level.cores.as_str();
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
            color = match summary["pressure"].as_u64() {
                Some(0) => GREEN,
                Some(1) => YELLOW,
                Some(2) => RED,
                _ => MUTED,
            };
            crate::live_views::pressure(&summary["pressure"]).into()
        }
        Reading::LoadAverage => {
            let load = serde_json::json!({ "load": summary["load_average"][0] });
            crate::live_projection::quantity(&load, "load", "work items", 1.0)
        }
        Reading::Thermal => {
            color = thermal_color(summary);
            crate::live_system::thermal_pressure_text(summary).into()
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
        route,
    }
}

fn card_charging(summary: &Value) -> &'static str {
    match summary["battery"]["charging"].as_bool() {
        Some(true) => "Yes",
        Some(false) => "No",
        None => "Unavailable",
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
