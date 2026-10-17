use gpui::{Context, IntoElement, div, prelude::*, px, rgb};

use crate::charts::{BLUE, GREEN, GRID, MUTED, ORANGE, PANEL, PURPLE};
use crate::dashboard::Dashboard;
use crate::data::{reading, state};

type Metric = (
    &'static str,
    &'static str,
    f32,
    &'static str,
    u32,
    &'static str,
);

const GIB: f32 = 1.0 / 1_073_741_824.0;
const CPU: Metric = ("CPU", "cpu0", 1.0, "%", BLUE, "cpu");
const GPU: Metric = ("GPU", "gpu0", 1.0, "%", PURPLE, "gpu");
const MEMORY: Metric = ("Memory in use", "memoryUsed0", GIB, "GiB", GREEN, "memory");
const PRESSURE: Metric = ("Memory pressure", "pressure0", 1.0, "", GREEN, "memory");
const POWER: Metric = ("System power", "power0", 1.0, "W", ORANGE, "energy");
const READ: Metric = ("Disk read", "disk0", 1e-6, "MB/s", ORANGE, "storage");
const WRITE: Metric = ("Disk write", "disk1", 1e-6, "MB/s", ORANGE, "storage");
const RECEIVE: Metric = ("Receive", "network0", 1e-6, "MB/s", BLUE, "network");
const SEND: Metric = ("Send", "network1", 1e-6, "MB/s", GREEN, "network");
const GPU_POWER: Metric = ("GPU power", "gpuPower1", 1.0, "W", PURPLE, "gpu");

impl Dashboard {
    pub fn summary(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = div().flex().gap(px(8.0)).pt(px(16.0)).pb(px(8.0));
        for metric in summary_metrics(&self.route) {
            row = row.child(self.summary_card(metric, cx));
        }
        row
    }

    fn summary_card(&self, metric: &Metric, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, _, _, _, tint, route) = *metric;
        div()
            .id(title)
            .flex_1()
            .min_w_0()
            .p(px(12.0))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(GRID))
            .rounded(px(8.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.route = route.into();
                this.rebuild_rows();
                cx.notify();
            }))
            .child(
                div()
                    .text_color(rgb(MUTED))
                    .text_size(px(12.0))
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(22.0))
                    .text_color(rgb(tint))
                    .child(self.summary_value(metric)),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(summary_note(metric.1)),
            )
    }

    fn summary_value(&self, metric: &Metric) -> String {
        let (_, key, scale, unit, _, _) = *metric;
        let selected = self.inspection.selected;
        match key {
            "pressure0" => state(self.capture.value(key, selected), false).into(),
            "thermal0" => state(self.capture.value(key, selected), true).into(),
            "memCompressed" => self.capture.quantity(key, selected),
            _ => {
                let value = if key.starts_with("app") {
                    self.capture
                        .series
                        .get(key)
                        .and_then(|rows| {
                            rows.iter()
                                .find(|row| row.app.as_deref() == Some(&self.app))
                        })
                        .and_then(|row| row.values[selected])
                } else {
                    self.capture.value(key, selected)
                };
                reading(value.map(|value| value * scale), unit)
            }
        }
    }
}

const OVERVIEW_METRICS: &[Metric] = &[CPU, GPU, PRESSURE, POWER, READ, RECEIVE];

const CPU_METRICS: &[Metric] = &[
    CPU,
    ("Performance cores", "cpuUsage1", 1.0, "%", GREEN, "cpu"),
    ("Super cores", "cpuUsage2", 1.0, "%", PURPLE, "cpu"),
    ("Load average", "load0", 1.0, "work items", MUTED, "cpu"),
];

const GPU_METRICS: &[Metric] = &[
    GPU,
    ("Active frequency", "gpuDetail1", 0.001, "GHz", BLUE, "gpu"),
    ("Memory in use", "gpuMemory0", GIB, "GiB", PURPLE, "gpu"),
    GPU_POWER,
];

const MEMORY_METRICS: &[Metric] = &[
    PRESSURE,
    MEMORY,
    ("Compressed", "memCompressed", 1.0, "GiB", PURPLE, "memory"),
    ("Swap used", "swapDetail0", GIB, "GiB", MUTED, "memory"),
];

const STORAGE_METRICS: &[Metric] = &[
    READ,
    WRITE,
    ("Read operations", "diskOps0", 1.0, "ops/s", BLUE, "storage"),
    (
        "Write operations",
        "diskOps1",
        1.0,
        "ops/s",
        ORANGE,
        "storage",
    ),
];

const NETWORK_METRICS: &[Metric] = &[
    RECEIVE,
    SEND,
    ("Received packets", "packets0", 1.0, "pps", BLUE, "network"),
    ("Sent packets", "packets1", 1.0, "pps", GREEN, "network"),
];

const ENERGY_METRICS: &[Metric] = &[
    POWER,
    GPU_POWER,
    (
        "SoC temperature",
        "temperature0",
        1.0,
        "°C",
        ORANGE,
        "energy",
    ),
    ("Thermal state", "thermal0", 1.0, "", GREEN, "energy"),
];

const BATTERY_METRICS: &[Metric] = &[
    ("Charge", "battery0", 1.0, "%", GREEN, "battery"),
    ("Battery flow", "batteryPower0", 1.0, "W", ORANGE, "battery"),
    ("Health", "batteryHealth0", 1.0, "%", GREEN, "battery"),
    ("Cycles", "batteryHealth1", 1.0, "cycles", MUTED, "battery"),
];

const APP_METRICS: &[Metric] = &[
    ("CPU", "appCpu", 1.0, "% of one core", BLUE, "apps"),
    ("Resident memory", "appMemory", GIB, "GiB", GREEN, "apps"),
    ("Network", "appNetwork", 1e-6, "MB/s", BLUE, "apps"),
    ("Disk I/O", "appDisk", 1e-6, "MB/s", ORANGE, "apps"),
    ("GPU time", "appGpu", 1.0, "% of interval", PURPLE, "apps"),
    ("Process power", "appEnergy", 1.0, "W", ORANGE, "apps"),
];

fn summary_metrics(route: &str) -> &'static [Metric] {
    match route {
        "overview" => OVERVIEW_METRICS,
        "cpu" => CPU_METRICS,
        "gpu" => GPU_METRICS,
        "memory" => MEMORY_METRICS,
        "storage" => STORAGE_METRICS,
        "network" => NETWORK_METRICS,
        "energy" => ENERGY_METRICS,
        "battery" => BATTERY_METRICS,
        _ => APP_METRICS,
    }
}

fn summary_note(key: &str) -> &'static str {
    match key {
        "cpu0" => "All 18 cores",
        "gpu0" => "Active time",
        "pressure0" | "thermal0" => "macOS verdict",
        "power0" => "Whole-machine sensor",
        "disk0" | "disk1" => "Machine device I/O",
        "network0" | "network1" => "External interfaces",
        "memoryUsed0" => "Of 48 GiB installed",
        "cpuUsage1" => "12 cores · cluster capacity",
        "cpuUsage2" => "6 cores · cluster capacity",
        _ => "Captured sample",
    }
}
