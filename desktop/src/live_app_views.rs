use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{BLUE, GRID, MUTED, PANEL};
use crate::dashboard::control;
use crate::live_dashboard::LiveDashboard;
use crate::live_source::AppKey;
use crate::live_views::text;

const APPS_MAX: usize = 512;
const SELECTED_APP_INDEX: usize = APPS_MAX;
const JSON_BYTES_MAX: usize = 32 * 1024;
const _: () = {
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
            .child(self.app_selector_controls(cx))
            .child(self.app_paging(cx));
        if let Some(page) = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.apps.as_ref())
        {
            if page["ungrouped_members"]
                .as_u64()
                .is_some_and(|count| count > 0)
            {
                selector = selector.child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(
                    format!(
                        "{} processes have unavailable App identity",
                        text(&page["ungrouped_members"])
                    ),
                ));
            }
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
        div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child("App")
            .child(
                control(&format!(
                    "{}  {}",
                    label,
                    if self.app_picker_open { "▴" } else { "▾" }
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.app_picker_open = !this.app_picker_open;
                    this.rebuild();
                    cx.notify();
                })),
            )
            .child(
                control("App details").on_click(cx.listener(|this, _, _, cx| {
                    let open = this
                        .expanded
                        .as_ref()
                        .is_some_and(|(kind, _)| kind == "app_summary");
                    this.expanded = if open {
                        None
                    } else {
                        Some(("app_summary".into(), 0))
                    };
                    this.rebuild();
                    cx.notify();
                })),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child("CPU, memory and energy from the selected observation"),
            )
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
                    * marks partial coverage; exec-crossing CPU and energy are unavailable.",
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
                this.executable = None;
                this.request.members_offset = 0;
                this.app_picker_open = false;
                this.expanded = None;
                this.submit();
                this.rebuild();
                cx.notify();
            }))
            .child(app_cells(
                text(&app["display_name"]),
                text(&app["members"]),
                [
                    value_rate(&app["cpu_percent_one_core"], "%"),
                    measure_memory(&app["resident_bytes"]),
                    measure_memory(&app["footprint_bytes"]),
                    value_rate(&app["energy_watts"], "W"),
                    value_rate(&app["energy_joules"], "J"),
                ],
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
                [
                    value_rate(&process["cpu_percent_one_core"], "%"),
                    amount_memory(&process["resident_bytes"]),
                    amount_memory(&process["footprint_bytes"]),
                    value_rate(&process["energy_watts"], "W"),
                    value_rate(&process["energy_joules"], "J"),
                ],
            ));
        if expanded {
            row = row.child(exact_json(process));
        }
        row.into_any_element()
    }

    fn current_app(&self) -> Option<&Value> {
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

fn app_summary_tiles(app: &Value) -> gpui::Div {
    let mut tiles = div().flex().gap(px(10.0));
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
    ] {
        tiles = tiles.child(app_summary_tile(app, tile));
    }
    tiles
}

fn app_summary_tile(app: &Value, tile: SummaryTile) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
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

pub fn app_table_columns(kind: &str) -> impl IntoElement {
    let name = if kind == "apps" { "App" } else { "Executable" };
    let count = match kind {
        "apps" => "Members",
        "executables" => "Instances",
        _ => "PID",
    };
    app_cells(
        name.into(),
        count.into(),
        [
            "CPU %".into(),
            "Resident".into(),
            "Footprint".into(),
            "Power".into(),
            "Energy".into(),
        ],
    )
    .bg(rgb(0x1a_22_2f))
    .text_color(rgb(MUTED))
}

pub fn app_cells(name: String, count: String, metrics: [String; 5]) -> gpui::Div {
    let mut row = div()
        .h(px(26.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(px(11.0))
        .child(
            div()
                .debug_selector(|| name.clone())
                .flex_1()
                .min_w_0()
                .truncate()
                .child(name),
        )
        .child(
            div()
                .w(px(65.0))
                .flex_shrink_0()
                .text_right()
                .truncate()
                .child(count),
        );
    for metric in metrics {
        row = row.child(
            div()
                .debug_selector(|| metric.clone())
                .w(px(100.0))
                .flex_shrink_0()
                .text_right()
                .truncate()
                .child(metric),
        );
    }
    row
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
