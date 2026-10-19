use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;
use std::rc::Rc;

use crate::charts::{BLUE, GRID, MUTED, PANEL, TEXT};
use crate::dashboard::control;
use crate::live_dashboard::{LiveDashboard, ROUTES, Row};

#[path = "live_computer_view.rs"]
mod computer_view;
#[path = "live_cpu_top_view.rs"]
mod cpu_top_view;
#[path = "live_recording_context_view.rs"]
mod recording_context_view;
#[path = "live_summary.rs"]
mod summary;

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
                    .debug_selector(move || format!("navigation:{route}"))
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
                .debug_selector(|| format!("source-sidebar:{}", self.native.source.sidebar()))
                .child(self.native.source.sidebar()),
        )
    }

    pub fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = ROUTES
            .iter()
            .find(|(route, _)| *route == self.request.route)
            .map_or("Flightdeck", |(_, title)| *title);
        let selected = self.header_selected();
        let mut header = div()
            .px(px(24.0))
            .py(px(16.0))
            .border_b_1()
            .border_color(rgb(GRID))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(12.0))
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .debug_selector(|| "reading-title".into())
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
                    .child(selected),
            );
        if !matches!(self.request.route.as_str(), "computer" | "self") {
            header = header.child(self.missing_reading());
            header = self.header_recording_context(header, cx);
            header = self.header_alternatives(header, cx);
        }
        if self.request.route == "raw" {
            header = header.child(self.table_picker(cx));
        }
        if self.request.route == "apps" {
            header = header.child(self.app_paging(cx));
        }
        self.header_geometry(header)
    }

    fn header_selected(&self) -> gpui::Div {
        if self.request.route == "self" {
            return div().child("Reader diagnostics · independent of chart inspection");
        }

        let selected = self.reading_snapshot().map_or_else(
            || {
                self.request.second.map_or_else(
                    || "Waiting for readings".into(),
                    |second| format!("Requested Unix second {second}"),
                )
            },
            |snapshot| crate::live_presentation::selected_clock(&snapshot.selection),
        );

        self.reading_header(&selected)
    }

    fn header_geometry(&self, header: gpui::Div) -> impl IntoElement {
        let height = self
            .preview
            .as_ref()
            .and_then(|preview| preview.header_height);
        let measured = Rc::clone(&self.header_height);
        header
            .id("reading-header")
            .relative()
            .when_some(height, |header, height| {
                header.h(height).overflow_y_scroll()
            })
            .child(
                gpui::canvas(
                    // Percent canvas bounds omit this header's one-pixel bottom border.
                    move |bounds, _, _| measured.set(bounds.size.height + px(1.0)),
                    |_, (), _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }

    fn header_controls(&self, cx: &mut Context<Self>) -> gpui::Div {
        let controls = self
            .native_header_controls(cx)
            .debug_selector(|| "reading-controls".into())
            .flex_wrap()
            .min_w_0()
            .max_w_full();

        match self.request.route.as_str() {
            "self" => controls.child(control("Refresh").on_click(cx.listener(|this, _, _, cx| {
                this.submit();
                cx.notify();
            }))),
            "computer" => {
                controls.child(control("Refresh").on_click(cx.listener(|this, _, _, cx| {
                    this.inventory = crate::live_inventory::rows();
                    cx.notify();
                })))
            }
            _ => self.header_history_controls(controls, cx),
        }
    }

    fn header_history_controls(
        &self,
        mut controls: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        controls = controls.children(self.native.input.clone()).child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(MUTED))
                .mr(px(10.0))
                .child("Last 15 minutes"),
        );
        if self.request.second.is_some() {
            for (label, delta) in [("‹", -1_i64), ("›", 1)] {
                controls = controls.child(
                    control(label)
                        .on_click(cx.listener(move |this, _, _, cx| this.chart_step(delta, cx))),
                );
            }
        }
        let label = self
            .native
            .source
            .following_label(self.request.second.is_some());

        controls
            .child(
                control(label)
                    .debug_selector(|| format!("source-follow:{label}"))
                    .on_click(cx.listener(|this, _, _, cx| this.pause(cx))),
            )
            .child(control("↻").on_click(cx.listener(|this, _, _, cx| {
                this.submit();
                cx.notify();
            })))
    }

    pub fn row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        if let Row::Spacer(height) = &self.rows[index] {
            return div().h(*height).into_any_element();
        }

        let compact = matches!(
            &self.rows[index],
            Row::Table(_, _) | Row::App(_) | Row::Process(_) | Row::Executable(_)
        );
        let content = self.row_content(index, cx);
        let key = self.rows[index].layout_key();
        let measured = Rc::clone(&self.row_heights);
        let measured_key = key.clone();
        let height = self.reading_row_height(&self.rows[index]);
        let scroll_reserved = matches!(
            &self.rows[index],
            Row::AppSummary
                | Row::CPUTop(_)
                | Row::CPUActivity
                | Row::ResourceSummary
                | Row::StorageDevice(_, _)
        );
        div()
            .id(gpui::SharedString::from(key))
            .relative()
            .when_some(height, |row, height| {
                row.h(height).when(
                    scroll_reserved,
                    gpui::StatefulInteractiveElement::overflow_y_scroll,
                )
            })
            .px(px(24.0))
            .py(px(if compact { 0.0 } else { 8.0 }))
            .child(content)
            .child(
                gpui::canvas(
                    move |bounds, _, _| {
                        let mut cache = measured.borrow_mut();
                        if cache.len() < 4096 {
                            cache.insert(measured_key, bounds.size.height);
                        }
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .into_any_element()
    }

    fn row_content(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        match &self.rows[index] {
            Row::Summary => self.summary(cx).into_any_element(),
            Row::ResourceSummary => self.resource_summary(),
            Row::StorageDevice(index, _) => self.resource_device(*index),
            Row::Diagnostics => self.self_monitoring().into_any_element(),
            Row::CPUTop(panel) => self.cpu_top(panel.as_ref(), cx).into_any_element(),
            Row::CPUActivity => self.cpu_activity().into_any_element(),
            Row::CPUHistory => self.cpu_history_panel(cx).into_any_element(),
            Row::OverviewStatus => self.overview_status(cx).into_any_element(),
            Row::Rankings(metrics) => self.app_rankings(metrics, cx),
            Row::MemoryAttribution(panel) => self.memory_attribution(panel.as_ref(), cx),
            Row::AppSelector => self.app_selector(cx).into_any_element(),
            Row::AppSummary => self.app_summary().into_any_element(),
            Row::AppColumns(kind) => {
                crate::live_app_views::app_table_columns(kind, &self.app_table_scroll)
                    .into_any_element()
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
            Row::Spacer(height) => div().h(*height).into_any_element(),
        }
    }

    fn summary(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let mut tiles = div().flex().flex_wrap().gap(px(10.0));
        for card in summary::cards(&self.request.route, selection) {
            tiles = tiles.child(Self::summary_card(&card, cx));
        }

        div()
            .id("resource-summary")
            .role(gpui::Role::Group)
            .aria_label("Resource readings at the selected time")
            .child(tiles)
            .children((self.request.route == "memory").then(|| self.memory_quantities()))
    }

    fn summary_card(card: &summary::Card, cx: &mut Context<Self>) -> impl IntoElement {
        let route = card.route;
        let selector = format!("summary-card:{}:{}", card.label, card.reading);
        div()
            .id(gpui::SharedString::from(format!("summary-{}", card.label)))
            .debug_selector(move || selector.clone())
            .accessibility_id(format!("flightdeck.summary.{}", card.label))
            .role(gpui::Role::Button)
            .aria_label(format!(
                "{}: {}; {}. Open {}",
                card.label, card.reading, card.note, route
            ))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus(|style| style.bg(rgb(0x32_46_61)))
            .hover(|style| style.bg(rgb(0x20_29_36)))
            .flex_1()
            .min_w(px(145.0))
            .p(px(16.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .on_click(cx.listener(move |this, _, _, cx| this.summary_navigate(route, cx)))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.summary_navigate(route, cx);
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(MUTED))
                    .child(card.label),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .text_size(px(25.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(card.color))
                    .child(card.reading.clone()),
            )
            .child(
                div()
                    .mt(px(4.0))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(card.note),
            )
    }

    fn summary_navigate(&mut self, route: &str, cx: &mut Context<Self>) {
        if let Some(target) = self.reading_target().cloned() {
            self.request.second = Some(target.second);
            self.request.identity = target.identity;
        }
        self.navigate(route, cx);
    }

    fn memory_quantities(&self) -> impl IntoElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let mut values = div().flex().flex_wrap().gap(px(12.0));

        for row in crate::live_memory::quantities(selection) {
            let selector = format!("memory-quantity:{}:{}", row.name, row.reading);
            values = values.child(
                div()
                    .debug_selector(|| selector.clone())
                    .min_w(px(220.0))
                    .flex_1()
                    .flex()
                    .justify_between()
                    .gap(px(12.0))
                    .child(div().text_color(rgb(MUTED)).child(row.name))
                    .child(row.reading),
            );
        }

        div()
            .mt(px(16.0))
            .p(px(18.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(div().text_size(px(15.0)).child("All memory quantities"))
            .child(div().mt(px(12.0)).text_size(px(12.0)).child(values))
            .child(
                div().mt(px(12.0)).text_size(px(11.0)).text_color(rgb(MUTED)).child(
                    "Independent VM categories overlap and are not an additive used-memory total. \
                    Speculative is included in Free.",
                ),
            )
    }

    fn computer(&self) -> impl IntoElement {
        computer_view::view(&self.inventory)
    }
}

pub fn text(value: &Value) -> String {
    match value {
        Value::Null => "Unavailable".into(),
        Value::String(text) => text.clone(),
        value => value.to_string(),
    }
}

pub(crate) fn pressure(value: &Value) -> &'static str {
    match value.as_u64() {
        Some(0) => "Normal",
        Some(1) => "Warning",
        Some(2) => "Critical",
        _ => "Unavailable",
    }
}
