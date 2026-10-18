use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{BLUE, GRID, MUTED, PANEL, TEXT};
use crate::dashboard::control;
use crate::live_dashboard::{LiveDashboard, ROUTES, Row};
use crate::live_projection::quantity;

impl LiveDashboard {
    pub fn navigation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sidebar = div()
            .flex()
            .flex_col()
            .w(px(170.0))
            .flex_shrink_0()
            .h_full()
            .p(px(12.0))
            .gap(px(3.0))
            .border_r_1()
            .border_color(rgb(GRID))
            .child(
                div()
                    .py(px(12.0))
                    .px(px(10.0))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child("SYSTEM MONITOR"),
            );
        for (route, title) in ROUTES {
            sidebar = sidebar.child(
                div()
                    .id(route)
                    .accessibility_id(format!("flightdeck.navigation.{route}"))
                    .role(gpui::Role::Button)
                    .aria_label(title)
                    .aria_selected(self.request.route == route)
                    .px(px(12.0))
                    .py(px(11.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .focus(|style| style.bg(rgb(0x32_46_61)))
                    .bg(rgb(if self.request.route == route {
                        0x1c_2b_40
                    } else {
                        0x11_12_17
                    }))
                    .text_color(rgb(if self.request.route == route {
                        BLUE
                    } else {
                        TEXT
                    }))
                    .hover(|style| style.bg(rgb(0x2c_2f_35)))
                    .on_click(cx.listener(move |this, _, _, cx| this.navigate(route, cx)))
                    .child(title),
            );
        }
        sidebar.child(div().flex_1()).child(
            div()
                .p(px(10.0))
                .text_size(px(10.0))
                .text_color(rgb(MUTED))
                .child("Live system readings"),
        )
    }

    pub fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = ROUTES
            .iter()
            .find(|(route, _)| *route == self.request.route)
            .map_or("Flightdeck", |(_, title)| *title);
        let selected = self.snapshot.as_ref().map_or_else(
            || "Waiting for readings".into(),
            |snapshot| crate::live_presentation::selected_clock(&snapshot.selection),
        );
        let mut header = div()
            .px(px(24.0))
            .py(px(16.0))
            .border_b_1()
            .border_color(rgb(GRID))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(23.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(self.header_controls(cx)),
            )
            .child(
                div()
                    .mt(px(8.0))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(format!("{selected} · {}", self.status())),
            );
        header = header.child(self.missing_reading());
        if self.request.route == "raw" {
            header = self.header_alternatives(header, cx);
        }
        if let Some(snapshot) = &self.snapshot {
            header = header.children(
                crate::live_notices::losses(&snapshot.catalog, &snapshot.selection)
                    .into_iter()
                    .map(|notice| {
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(crate::charts::ORANGE))
                            .child(notice)
                    }),
            );
        }
        if self.request.route == "raw" {
            header = header.child(self.table_picker(cx));
        }
        if self.request.route == "apps" {
            header = header.child(self.app_paging(cx));
        }
        header
    }

    fn header_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut controls = self.native_header_controls(cx);
        if self.request.route == "computer" {
            controls =
                controls.child(control("Refresh").on_click(cx.listener(|this, _, _, cx| {
                    this.inventory = crate::live_inventory::rows();
                    cx.notify();
                })));
        } else {
            controls = controls.children(self.native.input.clone());
            controls = controls.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(MUTED))
                    .mr(px(10.0))
                    .child("Last 15 minutes"),
            );
            if self.request.second.is_some() {
                for (label, delta) in [("‹", -1_i64), ("›", 1)] {
                    controls = controls.child(control(label).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.chart_step(delta, cx);
                        },
                    )));
                }
            }
            controls = controls.child(
                control(if self.request.second.is_some() {
                    "Live"
                } else {
                    "Pause live"
                })
                .on_click(cx.listener(|this, _, _, cx| this.pause(cx))),
            );
            controls = controls.child(control("↻").on_click(cx.listener(|this, _, _, cx| {
                this.submit();
                cx.notify();
            })));
        }
        controls
    }

    fn header_alternatives(&self, header: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let Some(snapshot) = &self.snapshot else {
            return header;
        };
        let Some(alternatives) = snapshot.selection["alternatives"].as_array() else {
            return header;
        };
        if alternatives.len() < 2 {
            return header;
        }
        let mut choices = div().flex().flex_wrap().gap(px(5.0)).mt(px(6.0));
        for identity in alternatives.iter().take(64) {
            let identity = identity.clone();
            let label = format!("Publication {}", text(&identity["publication"]));
            choices =
                choices.child(control(&label).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(second) = this
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.selection["second"].as_u64())
                    {
                        this.request.second = Some(second);
                        this.request.identity = Some(identity.clone());
                        this.request.offset = 0;
                        this.request.apps_offset = 0;
                        this.request.members_offset = 0;
                        this.submit();
                        cx.notify();
                    }
                })));
        }
        header.child(choices)
    }

    pub fn row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let compact = matches!(
            &self.rows[index],
            Row::Table(_, _) | Row::App(_) | Row::Process(_) | Row::Executable(_)
        );
        let content = match &self.rows[index] {
            Row::Summary => self.summary().into_any_element(),
            Row::CPUActivity => self.cpu_activity().into_any_element(),
            Row::OverviewStatus => self.overview_status(cx).into_any_element(),
            Row::AppSelector => self.app_selector(cx).into_any_element(),
            Row::AppSummary => self.app_summary().into_any_element(),
            Row::AppColumns(kind) => {
                crate::live_app_views::app_table_columns(kind).into_any_element()
            }
            Row::Charts(panels) => div()
                .flex()
                .gap(px(16.0))
                .children(
                    panels
                        .iter()
                        .map(|panel| self.live_chart(panel, cx).into_any_element()),
                )
                .into_any_element(),
            Row::Heading(title) => div()
                .text_size(px(17.0))
                .child(title.clone())
                .into_any_element(),
            Row::Hardware => self.live_hardware(cx).into_any_element(),
            Row::TableHeading(name) => self.table_heading(name, cx).into_any_element(),
            Row::Table(name, index) => self.table_row(name, *index, cx),
            Row::App(index) => self.app_row(*index, cx),
            Row::Process(index) => self.process_row(*index, cx),
            Row::Executable(index) => self.executable_row(*index, cx),
            Row::Inventory => self.computer().into_any_element(),
        };
        div()
            .px(px(24.0))
            .py(px(if compact { 0.0 } else { 8.0 }))
            .child(content)
            .into_any_element()
    }

    fn summary(&self) -> impl IntoElement {
        let summary = self
            .snapshot
            .as_ref()
            .map_or(&Value::Null, |snapshot| &snapshot.selection["summary"]);
        let mut tiles = div().flex().flex_wrap().gap(px(10.0));
        for &(label, key, unit, divisor) in summary_tiles(&self.request.route) {
            tiles = tiles.child(
                div()
                    .flex_1()
                    .min_w(px(120.0))
                    .p(px(18.0))
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(rgb(GRID))
                    .bg(rgb(PANEL))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(MUTED))
                            .child(label),
                    )
                    .child(
                        div()
                            .mt(px(6.0))
                            .text_size(px(25.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(quantity(summary, key, unit, divisor)),
                    ),
            );
        }
        div()
            .child(tiles)
            .child(div().mt(px(8.0)).text_color(rgb(MUTED)).child(format!(
                "Memory pressure: {}",
                pressure(&summary["pressure"]),
            )))
    }

    fn computer(&self) -> impl IntoElement {
        let mut content = div()
            .p(px(18.0))
            .bg(rgb(PANEL))
            .rounded(px(10.0))
            .child("Current computer · independent of the selected recording");
        for (name, value) in &self.inventory {
            content = content.child(
                div()
                    .flex()
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(rgb(GRID))
                    .child(
                        div()
                            .w(px(190.0))
                            .text_color(rgb(MUTED))
                            .child(name.clone()),
                    )
                    .child(value.clone()),
            );
        }
        content
    }
}

pub fn text(value: &Value) -> String {
    match value {
        Value::Null => "Unavailable".into(),
        Value::String(text) => text.clone(),
        value => value.to_string(),
    }
}

fn pressure(value: &Value) -> &'static str {
    match value.as_u64() {
        Some(0) => "Normal",
        Some(1) => "Warning",
        Some(2) => "Critical",
        _ => "Unavailable",
    }
}

fn summary_tiles(route: &str) -> &'static [(&'static str, &'static str, &'static str, f32)] {
    if route == "memory" {
        return &[
            ("Active", "memory_active_bytes", "GiB", 1_073_741_824.0),
            ("Wired", "memory_wired_bytes", "GiB", 1_073_741_824.0),
            (
                "Compressed",
                "memory_compressed_bytes",
                "GiB",
                1_073_741_824.0,
            ),
            ("Swap used", "swap_used_bytes", "GiB", 1_073_741_824.0),
            ("Installed", "memory_total_bytes", "GiB", 1_073_741_824.0),
        ];
    }
    &[
        ("CPU", "cpu_percent", "%", 1.0),
        ("GPU", "gpu_active_percent", "%", 1.0),
        (
            "Active memory",
            "memory_active_bytes",
            "GiB",
            1_073_741_824.0,
        ),
        ("System power", "power_watts", "W", 1.0),
        ("SoC", "soc_celsius", "°C", 1.0),
        ("Swap used", "swap_used_bytes", "GiB", 1_073_741_824.0),
    ]
}
