use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{GRID, MUTED};
use crate::dashboard::control;
use crate::live_dashboard::LiveDashboard;
use crate::live_views::text;

const COLUMNS_MAX: usize = 32;
const PAGE_ROWS: u64 = 512;
const _: () = {
    assert!(COLUMNS_MAX >= 4);
    assert!(PAGE_ROWS == 512);
};

impl LiveDashboard {
    pub fn table_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut picker = div().mt(px(8.0)).flex().flex_wrap().gap(px(5.0));
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
        let mut title = div()
            .h(px(30.0))
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
        let mut heading = div().child(title);
        if let Some(columns) = page["columns"].as_array() {
            heading = heading.child(
                div()
                    .h(px(26.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(8.0))
                    .bg(rgb(0x1a_22_2f))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .children(
                        self.table_indices(name)
                            .iter()
                            .filter_map(|index| columns.get(*index))
                            .map(|column| div().flex_1().min_w_0().truncate().child(text(column))),
                    ),
            );
        }
        heading.child(
            div()
                .py(px(4.0))
                .text_size(px(10.0))
                .text_color(rgb(MUTED))
                .child("Select a row for all recorded columns. NULL means unavailable."),
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
            .border_b_1()
            .border_color(rgb(GRID))
            .cursor_pointer()
            .focusable()
            .focus(|style| style.bg(rgb(0x32_46_61)))
            .hover(|style| style.bg(rgb(0x20_29_36)))
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
                div()
                    .h(px(26.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .text_size(px(11.0))
                    .children(
                        self.table_indices(name)
                            .iter()
                            .filter_map(|index| row.get(*index))
                            .map(|value| {
                                div().flex_1().min_w_0().truncate().child(raw_text(value))
                            }),
                    ),
            );
        if expanded {
            content = content.child(table_columns(page, row));
        }
        content.into_any_element()
    }

    pub fn app_paging(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let apps = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.apps.as_ref());
        let detail = self.snapshot.as_ref().and_then(|snapshot| {
            crate::live_app_views::selected_detail(
                snapshot.detail.as_ref(),
                self.request.app.as_ref(),
            )
        });
        div()
            .flex()
            .flex_wrap()
            .gap(px(10.0))
            .text_size(px(11.0))
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
                        "Executable filter active · charts show App totals"
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
            if matches!(name, "cpus" | "interfaces") {
                return self.table_page(name)["columns"]
                    .as_array()
                    .map_or_else(Vec::new, |columns| {
                        (0..columns.len().min(COLUMNS_MAX)).collect()
                    });
            }
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
            .text_size(px(11.0))
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
                    } else {
                        this.error = Some("Recorded page offset overflow".into());
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
        let total = page.map_or_else(|| "Unavailable".into(), |page| text(&page["page"]["total"]));
        let mut controls = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(format!("{label} · offset {offset} of {total}"));
        if offset >= 512 {
            controls = controls.child(control(&format!("Previous {label}")).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.app_change_offset(offset.saturating_sub(512), members, cx);
                },
            )));
        }
        if let Some(next) = next {
            controls = controls.child(control(&format!("Next {label}")).on_click(
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

fn table_columns(page: &Value, row: &[Value]) -> impl IntoElement {
    let mut columns = div().px(px(8.0)).py(px(6.0)).bg(rgb(0x10_15_20));
    if let Some(names) = page["columns"].as_array() {
        for (index, name) in names.iter().take(COLUMNS_MAX).enumerate() {
            let value = row
                .get(index)
                .map_or_else(|| "Missing column".to_owned(), raw_text);
            columns = columns.child(
                div()
                    .py(px(3.0))
                    .flex()
                    .gap(px(12.0))
                    .text_size(px(11.0))
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

fn unavailable(message: &str) -> AnyElement {
    div()
        .text_color(rgb(MUTED))
        .child(message.to_owned())
        .into_any_element()
}
