use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};
use num_traits::ToPrimitive;
use serde_json::Value;

use crate::charts::{BLUE, GREEN, GRID, MUTED, ORANGE, PANEL, PURPLE, YELLOW};
use crate::dashboard::control;
use crate::live_dashboard::{LiveDashboard, Picker, toggle};
use crate::live_source::AppKey;
use crate::live_views::{summary, text};

/// The prototype's network cyan.
const CYAN: u32 = 0x00_ff_ff;
const APPS_MAX: usize = 512;
const SELECTED_APP_INDEX: usize = APPS_MAX;
const JSON_BYTES_MAX: usize = 32 * 1024;
/// The most metric columns a table row has, the process group table's eleven.
const METRICS_MAX: usize = 11;
/// The prototype's column widths, in points: names wide enough for a helper's full name.
const NAME_WIDTH: f32 = 200.0;
const COUNT_WIDTH: f32 = 65.0;
const METRIC_WIDTH: f32 = 76.0;
const _: () = {
    assert!(crate::live_executables::COLUMNS.len() <= METRICS_MAX);
    assert!(JSON_BYTES_MAX > 2 * 4095);
    assert!(APPS_MAX == 512);
};

struct SummaryTile {
    label: &'static str,
    reading: String,
    field: &'static str,
    note: &'static str,
}

impl LiveDashboard {
    pub fn app_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut selector = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(self.app_selector_controls(cx));
        if self.app_paged() {
            selector = selector.child(self.app_paging(cx));
        }
        selector
    }

    fn app_selector_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = match self.current_app() {
            Some(app) => text(&app["display_name"]),
            None => {
                if self.request.app.is_some() {
                    "Selected App is not on this page".into()
                } else {
                    "Select an App".into()
                }
            }
        };
        let context = self.app_selector_context();
        let selector = format!("app-context:{context}");

        div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child("App")
            .child(
                control(&format!(
                    "{}  {}",
                    label,
                    if self.picker == Some(Picker::App) {
                        "▴"
                    } else {
                        "▾"
                    }
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.picker = toggle(this.picker, Picker::App);
                    this.rebuild();
                    cx.notify();
                })),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .debug_selector(move || selector.clone())
                    .child(context),
            )
    }

    /// The prototype's line beside the App picker: how many process groups the selected
    /// second shows, and that second.
    fn app_selector_context(&self) -> String {
        let Some(snapshot) = self.reading_snapshot() else {
            return "Waiting for readings".into();
        };
        let Some(second) = snapshot.selection["second"].as_u64() else {
            return "Waiting for readings".into();
        };

        let groups = self.reading_executables().len();
        if groups == 0 {
            return "App history retained; process groups not captured at this instant.".into();
        }
        format!(
            "{groups} process groups at {}",
            crate::live_presentation::clock(second, true)
        )
    }

    /// The prototype's control under the process group table, whose note says how processes
    /// group and how much of the App this second's readings cover.
    pub fn process_grouping_about(&self) -> impl IntoElement {
        let help = self.process_grouping_help();
        let label = "About process grouping and snapshot coverage";

        div()
            .id("process-grouping-about")
            .debug_selector(|| label.into())
            .role(gpui::Role::Button)
            .aria_label(label)
            .focusable()
            .tab_stop(true)
            .px(px(8.0))
            .py(px(6.0))
            .text_size(px(11.0))
            .text_color(rgb(MUTED))
            .cursor_pointer()
            .child(label)
            .tooltip(move |_, cx| {
                cx.new(|_| crate::live_chart_view::ChartHelp(help.clone()))
                    .into()
            })
    }

    fn process_grouping_help(&self) -> String {
        let mut parts = Vec::with_capacity(3);
        parts.push(
            "Process groups gather the selected App's processes by executable name, so a row \
            may stand for several processes and is not a PID. * marks a reading some of the \
            group's processes lack."
                .to_owned(),
        );
        let snapshot = self.reading_snapshot();
        if let Some(app) = self.current_app() {
            let loaded = snapshot
                .and_then(|snapshot| snapshot.detail.as_ref())
                .and_then(|detail| detail["processes"].as_array())
                .map_or(0, Vec::len);
            parts.push(format!(
                "At this second the App has {} live of {} members, and its membership is {}. \
                The table covers the {loaded} processes loaded.",
                text(&app["live_members"]),
                text(&app["members"]),
                if app["membership_complete"].as_bool() == Some(true) {
                    "complete"
                } else {
                    "uncertain"
                },
            ));
        }
        if let Some(page) = snapshot.and_then(|snapshot| snapshot.apps.as_ref()) {
            if page["ungrouped_members"]
                .as_u64()
                .is_some_and(|count| count > 0)
            {
                parts.push(format!(
                    "{} processes have unavailable App identity.",
                    text(&page["ungrouped_members"])
                ));
            }
        }
        parts.join(" ")
    }

    pub fn app_choice_indices(&self) -> Vec<usize> {
        let Some(snapshot) = self.reading_snapshot() else {
            return Vec::new();
        };
        let apps = snapshot
            .apps
            .as_ref()
            .and_then(|page| page["apps"].as_array());
        let mut choices = Vec::with_capacity(APPS_MAX + 1);
        if let Some(apps) = apps {
            for (index, app) in apps.iter().take(APPS_MAX).enumerate() {
                choices.push((text(&app["display_name"]).to_lowercase(), index));
            }
        }
        if let Some(app) = self.current_app() {
            let listed = apps.is_some_and(|apps| {
                apps.iter()
                    .take(APPS_MAX)
                    .any(|listed| listed["key"] == app["key"])
            });
            if !listed {
                choices.push((
                    text(&app["display_name"]).to_lowercase(),
                    SELECTED_APP_INDEX,
                ));
            }
        }
        choices.sort_by(|first, second| first.0.cmp(&second.0).then(first.1.cmp(&second.1)));
        assert!(choices.len() <= APPS_MAX + 1);
        choices.into_iter().map(|(_, index)| index).collect()
    }

    pub fn app_summary(&self) -> impl IntoElement {
        let Some(app) = self.current_app() else {
            return div().text_color(rgb(MUTED)).child(
                "Selected App unavailable in this observation. Its identity remains selected.",
            );
        };
        let loaded = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.detail.as_ref())
            .and_then(|detail| detail["processes"].as_array())
            .map_or(0, Vec::len);
        let expanded = self
            .reading_expanded()
            .is_some_and(|(kind, _)| kind == "app_summary");
        let membership = format!(
            "{} live of {} members · membership {} · {} this interval",
            text(&app["live_members"]),
            text(&app["members"]),
            if app["membership_complete"].as_bool() == Some(true) {
                "complete"
            } else {
                "uncertain"
            },
            value_rate(&app["energy_joules"], "J")
        );
        div()
            .child(div().text_size(px(20.0)).child(text(&app["display_name"])))
            .child(
                div()
                    .mt(px(3.0))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(text(&app["key"]["display"])),
            )
            .child(div().mt(px(10.0)).child(app_summary_tiles(app)))
            .child(
                div()
                    .mt(px(8.0))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .debug_selector(|| membership.clone())
                    .child(membership),
            )
            .child(
                div()
                    .mt(px(4.0))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "{loaded} of {} processes loaded · group subtotals cover this page. \
                    * marks partial coverage; exec-crossing CPU, disk and energy are unavailable.",
                        text(&app["members"])
                    )),
            )
            .children(expanded.then(|| exact_json(app)))
    }

    pub fn app_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(app) = self.app_at(index) else {
            return unavailable("App row unavailable");
        };
        let key = match app_key(app) {
            Ok(key) => key,
            Err(error) => return unavailable(&error),
        };
        let selected = self.request.app.as_ref() == Some(&key);
        let metrics = app_metrics(app);
        let disk_selector = format!("app-disk:{}:{}:{}", key.kind, key.bytes_hex, metrics[5]);

        div()
            .id(SharedString::from(format!("app-choice-{index}")))
            .accessibility_id(format!("flightdeck.app.{}.{}", key.kind, key.bytes_hex))
            .role(gpui::Role::Button)
            .aria_label(format!(
                "{}; {} members; CPU {}",
                text(&app["display_name"]),
                text(&app["members"]),
                value_rate(&app["cpu_percent_one_core"], "%")
            ))
            .aria_selected(selected)
            .min_w_0()
            .max_w_full()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.app_table_scroll)
            .border_b_1()
            .border_color(rgb(GRID))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus(|style| style.bg(rgb(0x32_46_61)))
            .bg(rgb(if selected { 0x20_30_41 } else { 0x14_19_24 }))
            .hover(|style| style.bg(rgb(0x20_29_36)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.request.app = Some(key.clone());
                this.request.executable = None;
                this.request.members_offset = 0;
                this.picker = None;
                this.expanded = None;
                this.submit();
                this.rebuild();
                cx.notify();
            }))
            .child(app_cells(
                text(&app["display_name"]),
                text(&app["members"]),
                metrics,
                reading_selector(5, &disk_selector),
            ))
            .into_any_element()
    }

    pub fn process_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(process) = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.detail.as_ref())
            .and_then(|detail| detail["processes"].get(index))
        else {
            return unavailable("Process instance unavailable");
        };
        let expanded = self
            .reading_expanded()
            .is_some_and(|(kind, row)| kind == "app_processes" && *row == index);
        let metrics = process_metrics(process);
        let disk_selector = format!("process-disk:{}:{}", text(&process["pid"]), metrics[5]);

        let mut row = div()
            .id(SharedString::from(format!("process-{index}")))
            .debug_selector(|| format!("process-{index}"))
            .accessibility_id(format!(
                "flightdeck.process.{}.{}",
                text(&process["pid"]),
                text(&process["creation"]["unique_id"])
            ))
            .role(gpui::Role::Button)
            .aria_label(format!(
                "{}; PID {}; CPU {}",
                text(&process["executable_name"]["display"]),
                text(&process["pid"]),
                value_rate(&process["cpu_percent_one_core"], "%")
            ))
            .aria_expanded(expanded)
            .min_w_0()
            .max_w_full()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.app_table_scroll)
            .border_b_1()
            .border_color(rgb(GRID))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus(|style| style.bg(rgb(0x32_46_61)))
            .hover(|style| style.bg(rgb(0x20_29_36)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.expanded = if expanded {
                    None
                } else {
                    Some(("app_processes".into(), index))
                };
                this.rebuild();
                cx.notify();
            }))
            .child(app_cells(
                text(&process["executable_name"]["display"]),
                text(&process["pid"]),
                metrics,
                reading_selector(5, &disk_selector),
            ));
        if expanded {
            row = row.child(exact_json(process));
        }
        row.into_any_element()
    }

    pub(crate) fn current_app(&self) -> Option<&Value> {
        let snapshot = self.reading_snapshot()?;
        Some(&selected_detail(snapshot.detail.as_ref(), self.request.app.as_ref())?["app"])
    }

    fn app_at(&self, index: usize) -> Option<&Value> {
        if index == SELECTED_APP_INDEX {
            return self.current_app();
        }
        self.reading_snapshot()?.apps.as_ref()?["apps"].get(index)
    }
}

pub fn selected_detail<'a>(
    detail: Option<&'a Value>,
    requested: Option<&AppKey>,
) -> Option<&'a Value> {
    let detail = detail?;
    let app = &detail["app"];
    if app.is_null() {
        return None;
    }
    if let Some(key) = requested {
        if app["key"]["kind"].as_str() != Some(key.kind.as_str()) {
            return None;
        }
        if app["key"]["bytes_hex"].as_str() != Some(key.bytes_hex.as_str()) {
            return None;
        }
    }
    Some(detail)
}

/// The prototype's six readings of the selected App at the selected second. Network has no
/// per-App source, and GPU time exists only for the seconds the app sampled while it ran.
pub(crate) fn app_cards(app: Option<&Value>, selection: &Value) -> Vec<summary::Card> {
    let reading = |read: fn(&Value) -> String| app.map_or_else(|| "Unavailable".into(), read);
    let card = |label, reading, note: &str, color, route| summary::Card {
        label,
        reading,
        note: note.into(),
        color,
        route,
    };
    vec![
        card(
            "CPU",
            reading(|app| value_rate(&app["cpu_percent_one_core"], "%")),
            "100% = one core",
            BLUE,
            "cpu",
        ),
        card(
            "Resident memory",
            reading(|app| measure_memory(&app["resident_bytes"])),
            "Resident; includes shared pages",
            GREEN,
            "memory",
        ),
        card(
            "Network",
            "Not captured".into(),
            "No per-app network source",
            CYAN,
            "network",
        ),
        card(
            "Disk I/O",
            reading(measure_disk_rate),
            "Selected app",
            YELLOW,
            "storage",
        ),
        card(
            "GPU time",
            app.map_or_else(|| "Unavailable".into(), |app| app_gpu_time(app, selection)),
            "Selected app",
            PURPLE,
            "gpu",
        ),
        card(
            "Process power",
            reading(|app| value_rate(&app["energy_watts"], "W")),
            "Selected app",
            ORANGE,
            "energy",
        ),
    ]
}

/// The selected App's share of GPU time at the selected second. An App the sampler saw no
/// GPU time for used none, and a second the app did not sample has no reading.
fn app_gpu_time(app: &Value, selection: &Value) -> String {
    let Some(apps) = selection["summary"]["gpu_apps"].as_array() else {
        return "Not sampled".into();
    };
    let key = &app["key"];
    let share = apps.iter().find(|entry| {
        if entry["kind"] != key["kind"] {
            return false;
        }
        entry["bytes_hex"] == key["bytes_hex"]
    });
    match share {
        Some(entry) => value_rate(&entry["percent"], "%"),
        None => rate(Some(0.0), "%"),
    }
}

fn app_summary_tiles(app: &Value) -> gpui::Div {
    let mut tiles = div().flex().flex_wrap().gap(px(10.0));
    for tile in [
        SummaryTile {
            label: "CPU",
            field: "cpu_ns",
            note: "100% = one core",
            reading: measure_rate(app, "cpu_percent_one_core", "cpu_ns", "%"),
        },
        SummaryTile {
            label: "Resident",
            field: "resident_bytes",
            note: "Shared pages count per process",
            reading: measure_memory(&app["resident_bytes"]),
        },
        SummaryTile {
            label: "Footprint",
            field: "footprint_bytes",
            note: "Separate process memory gauge",
            reading: measure_memory(&app["footprint_bytes"]),
        },
        SummaryTile {
            label: "Average power",
            field: "energy_nj",
            note: "Selected observation interval",
            reading: measure_rate(app, "energy_watts", "energy_nj", "W"),
        },
        SummaryTile {
            label: "Disk I/O",
            field: "disk_io_bytes",
            note: "Selected observation interval · process attribution. \
                Separate from device throughput.",
            reading: measure_disk_rate(app),
        },
    ] {
        tiles = tiles.child(app_summary_tile(app, tile));
    }
    tiles
}

fn app_summary_tile(app: &Value, tile: SummaryTile) -> impl IntoElement {
    let selector = format!("app-summary:{}:{}", tile.field, tile.reading);

    div()
        .flex_1()
        .flex_basis(px(180.0))
        .min_w(px(160.0))
        .max_w_full()
        .debug_selector(move || selector.clone())
        .p(px(12.0))
        .rounded(px(8.0))
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(GRID))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(tile.label),
        )
        .child(div().mt(px(5.0)).text_size(px(23.0)).child(tile.reading))
        .child(
            div()
                .mt(px(4.0))
                .text_size(px(10.0))
                .text_color(rgb(MUTED))
                .child(tile.note),
        )
        .child(
            div()
                .mt(px(4.0))
                .text_size(px(10.0))
                .text_color(rgb(MUTED))
                .child(coverage(&app[tile.field])),
        )
}

pub fn app_table_columns(kind: &str, scroll: &gpui::ScrollHandle) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("app-columns-{kind}")))
        .min_w_0()
        .max_w_full()
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .track_scroll(scroll)
        .child(app_table_headings(kind))
        .bg(rgb(0x1a_22_2f))
        .text_color(rgb(MUTED))
}

fn app_table_headings(kind: &str) -> gpui::Div {
    let selector = move |_: usize, text: &str| format!("app-column:{kind}:{text}");
    match kind {
        "apps" => app_cells(
            "App".into(),
            "Members".into(),
            [
                "CPU %".into(),
                "Resident".into(),
                "Footprint".into(),
                "Power".into(),
                "Energy".into(),
                "Disk I/O".into(),
            ],
            selector,
        ),
        "executables" => app_cells(
            "Process group".into(),
            "Instances".into(),
            crate::live_executables::COLUMNS.map(String::from),
            selector,
        ),
        _ => app_cells(
            "Executable".into(),
            "PID".into(),
            [
                "CPU %".into(),
                "Resident".into(),
                "Footprint".into(),
                "Power".into(),
                "Energy".into(),
                "Disk I/O".into(),
                "Idle wakeups/s".into(),
                "Page-ins/s".into(),
                "Threads".into(),
            ],
            selector,
        ),
    }
}

/// A table row's cells: the name, the count and the metrics, each found by the selector
/// `selector` gives its column index and text, the name being column 0.
pub fn app_cells<const METRICS: usize>(
    name: String,
    count: String,
    metrics: [String; METRICS],
    selector: impl Fn(usize, &str) -> String,
) -> gpui::Div {
    assert!(METRICS <= METRICS_MAX);

    let count_metrics = u16::try_from(METRICS).unwrap_or_else(|_| unreachable!());
    let width_metrics = f32::from(count_metrics);
    let name_selector = selector(0, &name);
    let count_selector = selector(1, &count);
    let mut row = div()
        .min_w(px(NAME_WIDTH
            + COUNT_WIDTH
            + width_metrics * METRIC_WIDTH
            + (width_metrics + 1.0) * 8.0
            + 16.0))
        .h(px(26.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(px(11.0))
        .child(
            div()
                .debug_selector(move || name_selector.clone())
                .flex_1()
                .min_w(px(NAME_WIDTH))
                .truncate()
                .child(name),
        )
        .child(
            div()
                .debug_selector(move || count_selector.clone())
                .w(px(COUNT_WIDTH))
                .flex_shrink_0()
                .text_right()
                .truncate()
                .child(count),
        );
    for (index, metric) in metrics.into_iter().enumerate() {
        let metric_selector = selector(index + 2, &metric);
        row = row.child(
            div()
                .debug_selector(move || metric_selector.clone())
                .w(px(METRIC_WIDTH))
                .flex_shrink_0()
                .text_right()
                .truncate()
                .child(metric),
        );
    }
    row
}

/// The selector of a reading's cell: its own text, the count's prefixed so a PID reads apart
/// from a reading, or `disk` for the Disk I/O column, so a test can find that column's reading
/// in any row.
pub fn reading_selector(disk_index: usize, disk: &str) -> impl Fn(usize, &str) -> String {
    move |index, text| {
        if index == 1 {
            return format!("count:{text}");
        }
        if index == disk_index + 2 {
            return disk.to_owned();
        }
        text.to_owned()
    }
}

#[cfg(test)]
#[path = "live_app_hover_input_tests.rs"]
mod hover_input_tests;

pub fn memory(value: Option<u64>) -> String {
    let Some(value) = value else {
        return "Unavailable".into();
    };
    let (divisor, unit) = if value >= 1_073_741_824 {
        (1_073_741_824, "GiB")
    } else {
        if value >= 1_048_576 {
            (1_048_576, "MiB")
        } else {
            if value >= 1024 {
                (1024, "KiB")
            } else {
                return format!("{value} B");
            }
        }
    };
    format!(
        "{}.{} {unit}",
        value / divisor,
        (value % divisor) * 10 / divisor
    )
}

pub fn rate(value: Option<f64>, unit: &str) -> String {
    match value {
        Some(value) if value.is_finite() => format!("{value:.1} {unit}"),
        _ => "Unavailable".into(),
    }
}

pub fn exact_json(value: &Value) -> impl IntoElement {
    let detail = value.to_string();
    let detail = if detail.len() <= JSON_BYTES_MAX {
        detail
    } else {
        "Detail exceeds the 32 KiB display limit".into()
    };
    div()
        .debug_selector(|| "expanded-json".into())
        .p(px(10.0))
        .bg(rgb(0x10_15_20))
        .text_size(px(10.0))
        .text_color(rgb(BLUE))
        .child(detail)
}

fn app_key(app: &Value) -> Result<AppKey, String> {
    crate::live_source::app_key_from_json(app)
}

fn value_rate(value: &Value, unit: &str) -> String {
    if value.is_null() {
        return "Unavailable".into();
    }
    match value.as_f64() {
        Some(value) => rate(Some(value), unit),
        None => "Invalid reading".into(),
    }
}

pub fn event_rate(value: Option<f64>, unit: &str) -> String {
    value.and_then(|value| value.to_f32()).map_or_else(
        || "Unavailable".into(),
        |value| crate::live_projection::quantity_text(value, unit),
    )
}

pub fn disk_rate(value: Option<f64>) -> String {
    let Some(value) = value else {
        return "Unavailable".into();
    };

    if !value.is_finite() {
        return "Unavailable".into();
    }

    if value < 0.0 {
        return "Invalid reading".into();
    }

    match (value / 1_000_000.0).to_f32() {
        Some(value) => crate::live_projection::quantity_text(value, "MB/s"),
        None => "Unavailable".into(),
    }
}

fn value_disk_rate(value: &Value) -> String {
    if value.is_null() {
        return "Unavailable".into();
    }

    match value.as_f64() {
        Some(value) => disk_rate(Some(value)),
        None => "Invalid reading".into(),
    }
}

fn measure_disk_rate(app: &Value) -> String {
    let measure = &app["disk_io_bytes"];
    let reading = match measure_value(measure) {
        Ok(Some(_)) => value_disk_rate(&app["disk_io_bps"]),
        Ok(None) => "Unavailable".into(),
        Err(error) => return error,
    };
    let Some(unavailable) = measure["unavailable_members"].as_u64() else {
        return "Missing unavailable coverage".into();
    };

    if unavailable > 0 {
        return format!("{reading} *");
    }

    reading
}

fn amount_disk_rate(process: &Value) -> String {
    let amount = &process["disk_io_bytes"];
    if !amount.is_object() {
        return "Missing disk amount".into();
    }

    match decimal(&amount["known"]) {
        Ok(Some(_)) => {
            if !amount["unavailable"].is_null() {
                return "Invalid disk amount".into();
            }

            value_disk_rate(&process["disk_io_bps"])
        }
        Ok(None) => {
            if amount["unavailable"].as_str().is_none() {
                return "Missing disk availability".into();
            }

            "Unavailable".into()
        }
        Err(error) => error,
    }
}

fn app_metrics(app: &Value) -> [String; 6] {
    [
        value_rate(&app["cpu_percent_one_core"], "%"),
        measure_memory(&app["resident_bytes"]),
        measure_memory(&app["footprint_bytes"]),
        value_rate(&app["energy_watts"], "W"),
        value_rate(&app["energy_joules"], "J"),
        measure_disk_rate(app),
    ]
}

fn process_metrics(process: &Value) -> [String; 9] {
    [
        value_rate(&process["cpu_percent_one_core"], "%"),
        amount_memory(&process["resident_bytes"]),
        amount_memory(&process["footprint_bytes"]),
        value_rate(&process["energy_watts"], "W"),
        value_rate(&process["energy_joules"], "J"),
        amount_disk_rate(process),
        event_rate(process["idle_wakeups_per_second"].as_f64(), "/s"),
        event_rate(process["pageins_per_second"].as_f64(), "pages/s"),
        amount_count(&process["threads"]),
    ]
}

fn amount_count(value: &Value) -> String {
    match decimal(&value["known"]) {
        Ok(Some(value)) => value.to_string(),
        Ok(None) => "Unavailable".into(),
        Err(error) => error,
    }
}

fn measure_rate(app: &Value, field: &str, measure: &str, unit: &str) -> String {
    match measure_value(&app[measure]) {
        Ok(Some(_)) => value_rate(&app[field], unit),
        Ok(None) => "Unavailable".into(),
        Err(error) => error,
    }
}

fn measure_memory(value: &Value) -> String {
    let reading = match measure_value(value) {
        Ok(value) => memory(value),
        Err(error) => error,
    };
    if value["unavailable_members"]
        .as_u64()
        .is_some_and(|count| count > 0)
    {
        format!("{reading} *")
    } else {
        reading
    }
}

fn measure_value(value: &Value) -> Result<Option<u64>, String> {
    let measured = value["measured_members"]
        .as_u64()
        .ok_or("Missing measured coverage")?;
    let overflowed = value["overflowed"]
        .as_bool()
        .ok_or("Missing overflow state")?;
    if overflowed {
        return Ok(None);
    }
    if measured == 0 {
        return Ok(None);
    }
    decimal(&value["known"])
}

fn amount_memory(value: &Value) -> String {
    match decimal(&value["known"]) {
        Ok(value) => memory(value),
        Err(error) => error,
    }
}

fn decimal(value: &Value) -> Result<Option<u64>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => text
            .parse::<u64>()
            .map(Some)
            .map_err(|error| error.to_string()),
        _ => Err("Invalid exact counter".into()),
    }
}

fn coverage(value: &Value) -> String {
    format!(
        "{} measured · {} unavailable{}",
        text(&value["measured_members"]),
        text(&value["unavailable_members"]),
        if value["overflowed"].as_bool() == Some(true) {
            " · overflow"
        } else {
            ""
        }
    )
}

fn unavailable(message: &str) -> AnyElement {
    div()
        .text_size(px(11.0))
        .text_color(rgb(MUTED))
        .child(message.to_owned())
        .into_any_element()
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use serde_json::json;

    crate::live_source::app_key_tests::captured_app_key_tests!(app_key);

    #[test]
    fn changing_apps_hides_previous_details_until_the_requested_app_arrives() {
        // A pending or failed B read must never expose retained A processes and histories as B.
        let previous = json!({"app":{"key":{"kind":"bundle","bytes_hex":"41"}},
            "processes":[{"pid":123}]});
        let requested = AppKey {
            kind: "bundle".into(),
            bytes_hex: "42".into(),
        };
        assert!(selected_detail(Some(&previous), Some(&requested)).is_none());
        assert!(selected_detail(None, Some(&requested)).is_none());

        let replacement = json!({"app":{"key":{"kind":"bundle","bytes_hex":"42"}},
            "processes":[{"pid":456}]});
        let detail = selected_detail(Some(&replacement), Some(&requested)).unwrap();
        assert_eq!(detail["processes"][0]["pid"], 456);
    }

    #[test]
    fn app_identity_includes_the_key_kind() {
        // Equal raw bytes from a different identity domain are a different App.
        let detail = json!({"app":{"key":{"kind":"bundle","bytes_hex":"41"}}});
        let requested = AppKey {
            kind: "executable".into(),
            bytes_hex: "41".into(),
        };
        assert!(selected_detail(Some(&detail), Some(&requested)).is_none());
    }
}
