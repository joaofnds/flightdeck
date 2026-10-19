use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{GRID, MUTED};
use crate::dashboard::control;
use crate::live_dashboard::{LiveDashboard, Picker, toggle};
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
            .reading_snapshot()
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
        }
        let mut heading = div()
            .debug_selector(|| format!("raw-heading:{name}"))
            .child(title);
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
                        columns
                            .iter()
                            .take(4)
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
            .reading_expanded()
            .is_some_and(|(table, row)| table == name && *row == index);
        let table = name.to_owned();
        let mut content = div()
            .id(SharedString::from(format!("raw-{name}-{index}")))
            .accessibility_id(format!("flightdeck.recorded-row.{name}.{index}"))
            .role(gpui::Role::Button)
            .aria_label(format!(
                "{name}, row {}; {}",
                index + 1,
                row.iter()
                    .take(COLUMNS_MAX)
                    .map(raw_text)
                    .collect::<Vec<_>>()
                    .join(", ")
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
                    .children(row.iter().take(4).map(|value| {
                        div()
                            .debug_selector(|| {
                                format!("raw-value:{name}:{index}:{}", raw_text(value))
                            })
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(raw_text(value))
                    })),
            );
        if expanded {
            content = content.child(table_columns(page, row));
        }
        content.into_any_element()
    }

    pub fn app_paging(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (apps, detail) = self.app_pages();
        div()
            .flex()
            .flex_wrap()
            .gap(px(10.0))
            .text_size(px(11.0))
            .child(self.app_page_controls(apps, false, cx))
            .child(self.app_page_controls(detail, true, cx))
    }

    /// Whether the Apps or the selected App's processes span more than one page, so the
    /// page controls have something to move between.
    pub fn app_paged(&self) -> bool {
        let (apps, detail) = self.app_pages();
        for page in [apps, detail].into_iter().flatten() {
            if page["page"]["offset"]
                .as_u64()
                .is_some_and(|offset| offset > 0)
            {
                return true;
            }
            if !page["page"]["next_offset"].is_null() {
                return true;
            }
        }
        false
    }

    fn app_pages(&self) -> (Option<&Value>, Option<&Value>) {
        let apps = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.apps.as_ref());
        let detail = self.reading_snapshot().and_then(|snapshot| {
            crate::live_app_views::selected_detail(
                snapshot.detail.as_ref(),
                self.request.app.as_ref(),
            )
        });
        (apps, detail)
    }

    /// The prototype's Process group picker above the process histories, with the histories'
    /// state beside it and the readings no source records per process below.
    pub fn process_group_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.process_group_selected();
        let selector = format!("process-group-picker:{selected}");
        let arrow = if self.picker == Some(Picker::ProcessGroup) {
            "▴"
        } else {
            "▾"
        };
        let state = self
            .process_history_state()
            .unwrap_or_else(|| "Process histories · grouped by name, not PID".into());
        let picker = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.0))
            .child("Process group")
            .child(
                div().debug_selector(move || selector.clone()).child(
                    control(&format!("{selected}  {arrow}"))
                        .debug_selector(|| "process-group-picker".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.picker = toggle(this.picker, Picker::ProcessGroup);
                            this.rebuild();
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .debug_selector({
                        let state = state.clone();
                        move || format!("process-history-state:{state}")
                    })
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(state),
            );

        let mut bar = div().flex().flex_col().gap(px(6.0)).child(picker);
        if self.picker == Some(Picker::ProcessGroup) {
            bar = bar.child(self.process_group_options(cx));
        }
        let uncaptured = "Per-process network, GPU time, open files and sockets are not captured.";
        bar.child(
            div()
                .debug_selector(|| uncaptured.into())
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(uncaptured),
        )
    }

    fn process_group_selected(&self) -> String {
        let Some(key) = self.request.executable.as_ref() else {
            return "All process groups".into();
        };

        if let Some(group) = self
            .reading_executables()
            .iter()
            .find(|group| group.key == *key)
        {
            return group.name.clone();
        }
        // A historical second may not list the group, whose history still names it.
        let history = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.process_history.as_deref())
            .and_then(crate::live_process_history::ProcessHistory::payload);
        match history {
            Some(payload) => crate::live_views::text(&payload["executable_name"]["display"]),
            None => "Selected group is not on this process page".into(),
        }
    }

    fn process_group_options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut options = div().flex().flex_col().gap(px(2.0)).max_w(px(360.0)).child(
            control("All process groups")
                .debug_selector(|| "all-process-groups".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.picker = None;
                    this.select_executable(None);
                    this.rebuild();
                    cx.notify();
                })),
        );
        for group in self.reading_executables().iter().take(512) {
            let key = group.key.clone();
            let selector = format!("process-group-option:{}", group.name);
            options = options.child(
                control(&group.name)
                    .debug_selector(move || selector.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.picker = None;
                        this.select_executable(Some(key.clone()));
                        this.rebuild();
                        cx.notify();
                    })),
            );
        }
        options
    }

    /// Why the process histories show nothing yet, or `None` once they show the selection.
    fn process_history_state(&self) -> Option<String> {
        use crate::live_process_history::ProcessHistory;

        let filter = self.request.executable.as_deref();
        if let Some(reason) = ProcessHistory::filter_reason(filter) {
            return Some(reason.into());
        }
        let history = self.reading_snapshot()?.process_history.as_deref();
        match history {
            Some(ProcessHistory::Ready { .. }) => {
                if filter.is_some() {
                    None
                } else {
                    Some("Loading captured history".into())
                }
            }
            Some(ProcessHistory::All { .. }) => {
                if filter.is_none() {
                    None
                } else {
                    Some("Loading captured history".into())
                }
            }
            Some(ProcessHistory::Unavailable(reason)) => Some(format!("Unavailable: {reason}")),
            None => Some("Loading captured history".into()),
        }
    }

    fn table_page(&self, name: &str) -> &Value {
        self.reading_snapshot()
            .and_then(|snapshot| snapshot.pages.get(name))
            .map_or(&Value::Null, |page| page)
    }

    fn raw_paging(&self, page: &Value, cx: &mut Context<Self>) -> impl IntoElement {
        if page.is_null() {
            return div().child("Recorded page unavailable");
        }

        let offset = self.reading_request().offset;
        let total = page["total"].as_u64();
        let mut controls = div()
            .flex()
            .gap(px(6.0))
            .items_center()
            .text_size(px(11.0))
            .child(format!("Offset {offset}"));
        if offset >= PAGE_ROWS {
            controls = controls.child(control("Previous 512").on_click(cx.listener(
                move |this, _, _, cx| {
                    this.request.offset = offset.saturating_sub(PAGE_ROWS);
                    this.expanded = None;
                    this.submit();
                    this.rebuild();
                    cx.notify();
                },
            )));
        }
        if total.is_some_and(|total| offset.saturating_add(PAGE_ROWS) < total) {
            controls = controls.child(control("Next 512").on_click(cx.listener(
                move |this, _, _, cx| {
                    if let Some(offset) = offset.checked_add(PAGE_ROWS) {
                        this.request.offset = offset;
                        this.expanded = None;
                        this.submit();
                        this.rebuild();
                        cx.notify();
                    } else {
                        this.error = Some("Recorded page offset overflow".into());
                    }
                },
            )));
        }
        controls
    }

    fn app_page_controls(
        &self,
        page: Option<&Value>,
        members: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if members { "Processes" } else { "Apps" };
        let Some(page) = page else {
            return unavailable(&format!("{label} page unavailable"));
        };

        let offset = if members {
            self.reading_request().members_offset
        } else {
            self.reading_request().apps_offset
        };
        let next = match page["page"]["next_offset"].as_u64() {
            Some(offset) => match u32::try_from(offset) {
                Ok(offset) => Some(offset),
                Err(error) => return unavailable(&format!("Invalid page offset: {error}")),
            },
            None => None,
        };
        let total = text(&page["page"]["total"]);
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
