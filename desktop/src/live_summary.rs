use serde_json::Value;

use crate::charts::{BLUE, GREEN, MUTED, ORANGE, PURPLE, RED, YELLOW};

pub struct Card {
    pub label: &'static str,
    pub reading: String,
    pub note: &'static str,
    pub color: u32,
    pub route: &'static str,
}

#[derive(Clone, Copy)]
enum Reading {
    Quantity(&'static str, &'static str, f32),
    Pressure,
    Uncaptured,
}

type Specification = (&'static str, Reading, &'static str, u32, &'static str);

const CPU: Specification = (
    "CPU",
    Reading::Quantity("cpu_percent", "%", 1.0),
    "All captured cores",
    BLUE,
    "cpu",
);
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
    Reading::Quantity("read_bps", "MB/s", 1_000_000.0),
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
    "Performance cores",
    Reading::Uncaptured,
    "Cluster activity not captured",
    GREEN,
    "cpu",
);
const SUPER: Specification = (
    "Super cores",
    Reading::Uncaptured,
    "Cluster activity not captured",
    PURPLE,
    "cpu",
);
const LOAD: Specification = (
    "Load average",
    Reading::Uncaptured,
    "Scheduling demand not captured",
    YELLOW,
    "cpu",
);
const GPU_FREQUENCY: Specification = (
    "Active frequency",
    Reading::Uncaptured,
    "Frequency not captured",
    YELLOW,
    "gpu",
);
const GPU_MEMORY: Specification = (
    "Memory in use",
    Reading::Uncaptured,
    "GPU allocation not captured",
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
const ACTIVE: Specification = (
    "Active memory",
    Reading::Quantity("memory_active_bytes", "GiB", 1_073_741_824.0),
    "Active VM pages",
    GREEN,
    "memory",
);
const COMPRESSED: Specification = (
    "Compressed",
    Reading::Quantity("memory_compressed_bytes", "GiB", 1_073_741_824.0),
    "Compressed in RAM",
    PURPLE,
    "memory",
);
const SWAP: Specification = (
    "Swap used",
    Reading::Quantity("swap_used_bytes", "GiB", 1_073_741_824.0),
    "On the startup disk",
    YELLOW,
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
    Reading::Uncaptured,
    "macOS verdict not captured",
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

pub fn cards(route: &str, selection: &Value) -> Vec<Card> {
    let summary = matching_summary(selection);
    let definitions = match route {
        "overview" => &[CPU, GPU, PRESSURE, POWER, DISK_READ, RECEIVE][..],
        "cpu" => &[CPU, PERFORMANCE, SUPER, LOAD][..],
        "gpu" => &[GPU, GPU_FREQUENCY, GPU_MEMORY, GPU_POWER][..],
        "memory" => &[PRESSURE, ACTIVE, COMPRESSED, SWAP][..],
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
    let (label, source, note, color, route) = *definition;
    let mut color = color;
    let reading = match source {
        Reading::Quantity(key, unit, divisor) => {
            let source = if key == "read_bps" {
                &summary["storage"]
            } else {
                summary
            };
            crate::live_projection::quantity(source, key, unit, divisor)
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
        Reading::Uncaptured => "Unavailable".into(),
    };

    Card {
        label,
        reading,
        note,
        color,
        route,
    }
}

fn matching_summary(selection: &Value) -> &Value {
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

    summary
}
