use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{BLUE, GRID, MUTED, PANEL};
use crate::dashboard::control;
use crate::live_dashboard::LiveDashboard;
use crate::live_source::AppKey;
use crate::live_views::text;

const COLUMNS_MAX: usize = 32;
const JSON_BYTES_MAX: usize = 32 * 1024;
const PAGE_ROWS: u64 = 512;
const _: () = {
    assert!(COLUMNS_MAX >= 4);
    assert!(JSON_BYTES_MAX >= 2 * 4095);
    assert!(PAGE_ROWS == 512);
};

impl LiveDashboard {
    pub fn table_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut picker = div().mt(px(12.0)).flex().flex_wrap().gap(px(5.0));
        let tables = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.catalog["tables"].as_array());
        if let Some(tables) = tables {
            for table in tables.iter().take(32) {
                if let Some(name) = table.as_str() {
                    let name = name.to_owned();
                    let label = if self.request.table == name {
                        format!("● {name}")
                    } else {
                        name.clone()
                    };
                    picker = picker.child(control(&label).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.request.table.clone_from(&name);
                            this.request.offset = 0;
                            this.expanded = None;
                            this.submit();
                            this.rebuild();
                            cx.notify();
                        },
                    )));
                }
            }
        }
        picker
    }

    pub fn table_heading(&self, name: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.table_page(name);
        let mut heading = div().flex().flex_col().gap(px(8.0));
        let mut title = div()
            .flex()
            .justify_between()
            .items_center()
            .child(format!("{name} · {} recorded rows", text(&page["total"])));
        if self.request.route == "raw" {
            title = title.child(self.raw_paging(page, cx));
        } else {
            if self.request.route == "cpu" {
                if name == "processes" {
                    title = title.child(self.raw_paging(page, cx));
                }
            }
        }
        heading = heading.child(title);
        if let Some(columns) = page["columns"].as_array() {
            heading = heading.child(
                div().flex().gap(px(10.0)).text_color(rgb(MUTED)).children(
                    self.table_indices(name)
                        .iter()
                        .filter_map(|index| columns.get(*index))
                        .map(|column| div().flex_1().min_w_0().child(text(column))),
                ),
            );
        }
        heading.child(
            div()
                .text_size(px(10.0))
                .text_color(rgb(MUTED))
                .child("Select a row to inspect every recorded column. NULL means unavailable."),
        )
    }

    pub fn table_row(&self, name: &str, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let page = self.table_page(name);
        let Some(row) = page["rows"].get(index).and_then(Value::as_array) else {
            return unavailable("Recorded row is unavailable");
        };
        let expanded = self
            .expanded
            .as_ref()
            .is_some_and(|(table, row)| table == name && *row == index);
        let table = name.to_owned();
        let mut content = div()
            .id(SharedString::from(format!("raw-{name}-{index}")))
            .p(px(10.0))
            .rounded(px(5.0))
            .bg(rgb(PANEL))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.expanded = if expanded {
                    None
                } else {
                    Some((table.clone(), index))
                };
                this.rebuild();
                cx.notify();
            }))
            .child(
                div().flex().gap(px(10.0)).children(
                    self.table_indices(name)
                        .iter()
                        .filter_map(|index| row.get(*index))
                        .map(|value| div().flex_1().min_w_0().child(raw_text(value))),
                ),
            );
        if expanded {
            content = content.child(table_columns(page, row));
        }
        content.into_any_element()
    }

    pub fn app_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(app) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.apps.as_ref())
            .and_then(|page| page["apps"].get(index))
        else {
            return unavailable("App row is unavailable");
        };
        let key = match app_key(app) {
            Ok(key) => key,
            Err(error) => return unavailable(&error),
        };
        let selected = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.detail.as_ref())
            .is_some_and(|detail| detail["app"]["key"] == app["key"]);
        let mut content = div()
            .id(SharedString::from(format!("app-{index}")))
            .p(px(12.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(if selected { 0x1c_2b_40 } else { PANEL }))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.request.app = Some(key.clone());
                this.executable = None;
                this.request.members_offset = 0;
                this.submit();
                this.rebuild();
                cx.notify();
            }))
            .child(app_row_heading(app));
        for (label, field, unit) in [
            ("CPU time", "cpu_ns", "ns"),
            ("Energy", "energy_nj", "nJ"),
            ("Resident", "resident_bytes", "B"),
            ("Footprint", "footprint_bytes", "B"),
        ] {
            content = content.child(div().mt(px(4.0)).text_size(px(11.0)).child(measure(
                label,
                &app[field],
                unit,
            )));
        }
        if selected {
            content = content.child(exact_json(app));
        }
        content.into_any_element()
    }

    pub fn process_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(process) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.detail.as_ref())
            .and_then(|detail| detail["processes"].get(index))
        else {
            return unavailable("Process instance is unavailable");
        };
        let expanded = self
            .expanded
            .as_ref()
            .is_some_and(|(table, row)| table == "app_processes" && *row == index);
        let mut content = div()
            .id(SharedString::from(format!("process-{index}")))
            .p(px(12.0))
            .rounded(px(6.0))
            .bg(rgb(PANEL))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.expanded = if expanded {
                    None
                } else {
                    Some(("app_processes".to_owned(), index))
                };
                this.rebuild();
                cx.notify();
            }))
            .child(process_row_heading(process));
        if expanded {
            content = content.child(exact_json(process));
        }
        content.into_any_element()
    }

    pub fn app_paging(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let apps = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.apps.as_ref());
        let detail = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.detail.as_ref());
        div()
            .mt(px(10.0))
            .flex()
            .flex_wrap()
            .gap(px(10.0))
            .child(self.app_page_controls(apps, false, cx))
            .child(self.app_page_controls(detail, true, cx))
            .child(
                control("All process groups").on_click(cx.listener(|this, _, _, cx| {
                    this.executable = None;
                    this.rebuild();
                    cx.notify();
                })),
            )
            .children(self.executable.as_ref().map(|key| {
                div().text_color(rgb(MUTED)).child(
                    if self.executables.iter().any(|group| group.key == *key) {
                        "Executable filter active · app charts remain app totals"
                    } else {
                        "Selected executable is not in this process page"
                    },
                )
            }))
    }

    fn table_page(&self, name: &str) -> &Value {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.pages.get(name))
            .map_or(&Value::Null, |page| page)
    }

    fn table_indices(&self, name: &str) -> Vec<usize> {
        if self.request.route == "cpu" {
            if name == "processes" {
                let Some(columns) = self.table_page(name)["columns"].as_array() else {
                    return Vec::new();
                };
                return ["pid", "awake_ns_at_start", "user_ns", "resident_bytes"]
                    .iter()
                    .filter_map(|field| columns.iter().position(|name| name == field))
                    .collect();
            }
        }
        (0..4).collect()
    }

    fn raw_paging(&self, page: &Value, cx: &mut Context<Self>) -> impl IntoElement {
        let offset = self.request.offset;
        let total = page["total"].as_u64();
        let mut controls = div()
            .flex()
            .gap(px(6.0))
            .items_center()
            .child(format!("Offset {offset}"));
        if offset >= PAGE_ROWS {
            controls = controls.child(control("Previous 512").on_click(cx.listener(
                |this, _, _, cx| {
                    this.request.offset = this.request.offset.saturating_sub(PAGE_ROWS);
                    this.expanded = None;
                    this.submit();
                    this.rebuild();
                    cx.notify();
                },
            )));
        }
        if total.is_some_and(|total| offset.saturating_add(PAGE_ROWS) < total) {
            controls =
                controls.child(control("Next 512").on_click(cx.listener(|this, _, _, cx| {
                    if let Some(offset) = this.request.offset.checked_add(PAGE_ROWS) {
                        this.request.offset = offset;
                        this.expanded = None;
                        this.submit();
                        this.rebuild();
                        cx.notify();
                    }
                })));
        }
        controls
    }

    fn app_page_controls(
        &self,
        page: Option<&Value>,
        members: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let offset = if members {
            self.request.members_offset
        } else {
            self.request.apps_offset
        };
        let label = if members { "Processes" } else { "Apps" };
        let next = match page.and_then(|page| page["page"]["next_offset"].as_u64()) {
            Some(offset) => match u32::try_from(offset) {
                Ok(offset) => Some(offset),
                Err(error) => return unavailable(&format!("Invalid page offset: {error}")),
            },
            None => None,
        };
        let mut controls = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(format!("{label} · offset {offset}"));
        if offset >= 512 {
            let previous_label = format!("Previous 512 {label}");
            controls = controls.child(control(&previous_label).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.app_change_offset(offset.saturating_sub(512), members, cx);
                },
            )));
        }
        if let Some(next) = next {
            let next_label = format!("Next 512 {label}");
            controls = controls.child(control(&next_label).on_click(
                cx.listener(move |this, _, _, cx| this.app_change_offset(next, members, cx)),
            ));
        }
        controls.into_any_element()
    }

    fn app_change_offset(&mut self, offset: u32, members: bool, cx: &mut Context<Self>) {
        if members {
            self.request.members_offset = offset;
        } else {
            self.request.apps_offset = offset;
        }
        self.expanded = None;
        self.submit();
        self.rebuild();
        cx.notify();
    }
}

fn app_row_heading(app: &Value) -> impl IntoElement {
    div()
        .child(
            div()
                .flex()
                .justify_between()
                .child(text(&app["display_name"]))
                .child(format!(
                    "CPU {}% · Power {} W",
                    text(&app["cpu_percent_one_core"]),
                    text(&app["energy_watts"])
                )),
        )
        .child(div().mt(px(5.0)).text_color(rgb(MUTED)).child(format!(
            "{} members · {} live · membership complete: {}",
            text(&app["members"]),
            text(&app["live_members"]),
            text(&app["membership_complete"])
        )))
}

fn process_row_heading(process: &Value) -> impl IntoElement {
    div()
        .child(
            div()
                .flex()
                .justify_between()
                .child(format!(
                    "{} · pid {} · {}",
                    text(&process["executable_name"]["display"]),
                    text(&process["pid"]),
                    text(&process["state"])
                ))
                .child(format!(
                    "CPU {}% · Power {} W",
                    text(&process["cpu_percent_one_core"]),
                    text(&process["energy_watts"])
                )),
        )
        .child(
            div()
                .mt(px(5.0))
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(format!(
                    "Process identity {} · pid version {}",
                    text(&process["creation"]["unique_id"]),
                    text(&process["creation"]["pid_version"])
                )),
        )
        .child(div().mt(px(5.0)).child(format!(
            "Resident {} · Footprint {}",
            amount(&process["resident_bytes"]),
            amount(&process["footprint_bytes"])
        )))
}

fn table_columns(page: &Value, row: &[Value]) -> impl IntoElement {
    let mut columns = div().mt(px(10.0)).border_t_1().border_color(rgb(GRID));
    if let Some(names) = page["columns"].as_array() {
        for (index, name) in names.iter().take(COLUMNS_MAX).enumerate() {
            let value = row
                .get(index)
                .map_or_else(|| "Missing column".to_owned(), raw_text);
            columns = columns.child(
                div()
                    .py(px(5.0))
                    .flex()
                    .gap(px(12.0))
                    .child(
                        div()
                            .w(px(220.0))
                            .flex_shrink_0()
                            .text_color(rgb(MUTED))
                            .child(text(name)),
                    )
                    .child(div().flex_1().min_w_0().child(value)),
            );
        }
    }
    columns
}

fn raw_text(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::String(value) => value.clone(),
        value => value.to_string(),
    }
}

fn app_key(value: &Value) -> Result<AppKey, String> {
    let kind = value["key"]["kind"]
        .as_str()
        .ok_or_else(|| "App key kind is unavailable".to_owned())?;
    let bytes_hex = value["key"]["bytes_hex"]
        .as_str()
        .ok_or_else(|| "App key bytes are unavailable".to_owned())?;
    Ok(AppKey {
        kind: kind.to_owned(),
        bytes_hex: bytes_hex.to_owned(),
    })
}

fn measure(label: &str, value: &Value, unit: &str) -> String {
    format!(
        "{label}: known {} {unit} · {} measured · {} unavailable · overflow: {}",
        text(&value["known"]),
        text(&value["measured_members"]),
        text(&value["unavailable_members"]),
        text(&value["overflowed"])
    )
}

fn amount(value: &Value) -> String {
    if value["known"].is_null() {
        format!("Unavailable ({})", text(&value["unavailable"]))
    } else {
        format!("{} B", text(&value["known"]))
    }
}

fn exact_json(value: &Value) -> impl IntoElement {
    let json = value.to_string();
    let detail = if json.len() <= JSON_BYTES_MAX {
        json
    } else {
        "Detail exceeds the 32 KiB display limit".to_owned()
    };
    div()
        .mt(px(8.0))
        .text_size(px(10.0))
        .text_color(rgb(BLUE))
        .child(detail)
}

fn unavailable(message: &str) -> AnyElement {
    div()
        .text_color(rgb(MUTED))
        .child(message.to_owned())
        .into_any_element()
}
