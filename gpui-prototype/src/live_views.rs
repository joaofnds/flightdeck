use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, Bounds, Context, IntoElement, MouseButton, SharedString, div, prelude::*, px, rgb,
};
use serde_json::Value;

use crate::charts::{self, BLUE, GRID, Inspection, MUTED, PANEL, TEXT};
use crate::dashboard::control;
use crate::data::{Panel, reading};
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
                    .px(px(12.0))
                    .py(px(11.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
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
                .child("Shared recording backend")
                .child(div().child("250 ms · bounded worker")),
        )
    }

    pub fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = ROUTES
            .iter()
            .find(|(route, _)| *route == self.request.route)
            .map_or("Flightdeck", |(_, title)| *title);
        let selected = self.snapshot.as_ref().map_or_else(
            || "Waiting for readings".into(),
            |snapshot| identity_label(&snapshot.selection),
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
                    .child(div().text_size(px(22.0)).child(title))
                    .child(self.header_controls(cx)),
            )
            .child(
                div()
                    .mt(px(8.0))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "{} · {selected} · 15 minute context",
                        self.status()
                    )),
            );
        header = self.header_alternatives(header, cx);
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
        let mut controls = div().flex().gap(px(6.0)).items_center();
        if self.request.route == "computer" {
            controls =
                controls.child(control("Refresh").on_click(cx.listener(|this, _, _, cx| {
                    this.inventory = crate::live_inventory::rows();
                    cx.notify();
                })));
        } else {
            for (label, delta) in [("−15s", -15_i64), ("‹", -1), ("›", 1), ("+15s", 15)] {
                controls =
                    controls.child(control(label).on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(second) = this
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.selection["second"].as_u64())
                        {
                            this.inspect(second.saturating_add_signed(delta), cx);
                        }
                    })));
            }
            controls = controls.child(
                control(if self.request.second.is_some() {
                    "Resume live"
                } else {
                    "Pause"
                })
                .on_click(cx.listener(|this, _, _, cx| this.pause(cx))),
            );
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
        let content = match &self.rows[index] {
            Row::Summary => self.summary().into_any_element(),
            Row::Charts(panels) => div()
                .flex()
                .gap(px(12.0))
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
            .py(px(7.0))
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
                    .p(px(12.0))
                    .rounded(px(8.0))
                    .bg(rgb(PANEL))
                    .child(label)
                    .child(
                        div()
                            .mt(px(6.0))
                            .text_size(px(20.0))
                            .text_color(rgb(BLUE))
                            .child(quantity(summary, key, unit, divisor)),
                    ),
            );
        }
        div()
            .child(tiles)
            .child(div().mt(px(8.0)).text_color(rgb(MUTED)).child(format!(
                "Memory pressure: {} · Installed: {}",
                pressure(&summary["pressure"]),
                quantity(summary, "memory_total_bytes", "GiB", 1_073_741_824.0)
            )))
    }

    fn live_chart(&self, panel: &Rc<Panel>, cx: &mut Context<Self>) -> impl IntoElement {
        let upper = panel
            .lines
            .first()
            .map_or(0, |line| line.values.len().saturating_sub(1));
        let inspection = Inspection {
            lower: 0,
            selected: panel.selected_index.unwrap_or(upper),
            upper,
        };
        let hidden = Rc::new(
            panel
                .lines
                .iter()
                .filter(|line| {
                    self.hidden
                        .contains(&format!("{}\t{}", panel.id, line.name))
                })
                .map(|line| line.name.clone())
                .collect::<Vec<_>>(),
        );
        let measured = Rc::new(Cell::new(Bounds::default()));
        let bounds = Rc::clone(&measured);
        let (minimum, maximum) = charts::domain(panel, inspection, &hidden);
        let plot = div()
            .h(px(220.0))
            .child(charts::plot(Rc::clone(panel), inspection, hidden, measured))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    if let Some(snapshot) = &this.snapshot {
                        let rect = charts::plot_bounds(bounds.get());
                        let fraction = f32::from(event.position.x - rect.origin.x)
                            / f32::from(rect.size.width);
                        if let Some(second) =
                            crate::live_projection::sample_second(&snapshot.selection, fraction)
                        {
                            this.inspect(second, cx);
                        }
                    }
                }),
            );
        div()
            .flex_1()
            .min_w_0()
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .p(px(14.0))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(panel.title.clone())
                    .child(format!("{minimum:.0} … {maximum:.0} {}", panel.unit)),
            )
            .child(plot)
            .child(Self::live_chart_legends(panel, cx))
            .child(
                div()
                    .mt(px(5.0))
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child("15 minutes · click to pause and inspect a second"),
            )
    }

    fn live_chart_legends(panel: &Panel, cx: &mut Context<Self>) -> impl IntoElement {
        let mut legends = div().flex().flex_wrap().gap(px(12.0)).text_size(px(11.0));
        for line in &panel.lines {
            let key = format!("{}\t{}", panel.id, line.name);
            legends = legends.child(
                div()
                    .id(SharedString::from(key.clone()))
                    .cursor_pointer()
                    .text_color(rgb(charts::color(&line.color)))
                    .child(format!(
                        "{}  {}",
                        line.name,
                        reading(
                            panel.selected_index.and_then(|index| line
                                .values
                                .get(index)
                                .copied()
                                .flatten()),
                            &panel.unit
                        )
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(index) = this.hidden.iter().position(|value| *value == key) {
                            this.hidden.remove(index);
                        } else {
                            this.hidden.push(key.clone());
                        }
                        cx.notify();
                    })),
            );
        }
        legends
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

fn identity_label(selection: &Value) -> String {
    let identity = &selection["selected"];
    if identity.is_null() {
        return "No reading in selected second".into();
    }
    format!(
        "second {} · publication {} · run {}",
        text(&selection["second"]),
        text(&identity["publication"]),
        text(&identity["run"])
    )
}
