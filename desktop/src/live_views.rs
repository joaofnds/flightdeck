use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;
use std::rc::Rc;

use crate::charts::{GRID, MUTED, PANEL};
use crate::live_dashboard::{LiveDashboard, Row};

#[path = "live_chrome.rs"]
pub(crate) mod chrome;
#[path = "live_computer_view.rs"]
mod computer_view;
#[path = "live_cpu_top_view.rs"]
mod cpu_top_view;
#[path = "live_recording_context_view.rs"]
mod recording_context_view;
#[path = "live_summary.rs"]
mod summary;
#[cfg(feature = "automation")]
pub(crate) use summary::cards;
pub(crate) use summary::matching_summary;

impl LiveDashboard {
    pub fn row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        if let Row::Spacer(height) = &self.rows[index] {
            return div().h(*height).into_any_element();
        }

        let compact = matches!(
            &self.rows[index],
            Row::Table(_, _) | Row::App(_) | Row::Process(_) | Row::Executable(_)
        );
        let content = self.row_content(index, cx);
        let padding = match &self.rows[index] {
            Row::Charts { .. }
            | Row::OverviewStatus
            | Row::BatteryState(_)
            | Row::OverviewRankings(_) => 4.0,
            _ => 8.0,
        };
        let key = self.rows[index].layout_key();
        let measured = Rc::clone(&self.row_heights);
        let measured_key = key.clone();
        let height = self.reading_row_height(&self.rows[index]);
        let scroll_reserved = matches!(
            &self.rows[index],
            Row::AppSummary | Row::CPUTop(_) | Row::CPUActivity | Row::StorageDevice(_, _)
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
            .py(px(if compact { 0.0 } else { padding }))
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
            Row::StorageDevice(index, _) => self.resource_device(*index),
            Row::Diagnostics => self.self_monitoring().into_any_element(),
            Row::CPUTop(panel) => self.cpu_top(panel.as_ref(), cx).into_any_element(),
            Row::CPUActivity => self.cpu_activity().into_any_element(),
            Row::CPUHistory => self.cpu_history_panel(cx).into_any_element(),
            Row::OverviewStatus => self.overview_status(cx).into_any_element(),
            Row::BatteryState(panel) => self.battery_state(panel.as_ref(), cx),
            Row::OverviewRankings(slots) => {
                self.overview_rankings(slots, self.reading_chart_columns(), cx)
            }
            Row::OverviewAppHistories(slots) => self.overview_app_histories(slots, cx),
            Row::MemoryAttribution(panel) => self.memory_attribution(panel.as_ref(), cx),
            Row::AppAttribution(metric, panel) => self.app_attribution(*metric, panel.as_ref(), cx),
            Row::AppSelector => self.app_selector(cx).into_any_element(),
            Row::AppSummary => self.app_summary().into_any_element(),
            Row::AppColumns(kind) => {
                crate::live_app_views::app_table_columns(kind, &self.app_table_scroll)
                    .into_any_element()
            }
            Row::Charts { panels, columns } => self.row_content_charts(panels, *columns, cx),
            Row::Heading(title) => heading(title).into_any_element(),
            Row::Disclosure(section) => self.disclosure(*section, cx).into_any_element(),
            Row::Hardware => self.live_hardware(cx).into_any_element(),
            Row::TableHeading(name) => self.table_heading(name, cx).into_any_element(),
            Row::Table(name, index) => self.table_row(name, *index, cx),
            Row::App(index) => self.app_row(*index, cx),
            Row::Process(index) => self.process_row(*index, cx),
            Row::Executable(index) => self.executable_row(*index, cx),
            Row::Inventory => self.computer(cx).into_any_element(),
            Row::Spacer(height) => div().h(*height).into_any_element(),
        }
    }

    fn row_content_charts(
        &self,
        panels: &[Rc<crate::data::Panel>],
        columns: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        assert!((1..=3).contains(&columns));
        assert!(!panels.is_empty());
        assert!(panels.len() <= columns);

        div()
            .flex()
            .gap(px(8.0))
            .children(panels.iter().map(|panel| {
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .child(self.live_chart(panel, cx))
            }))
            .children((panels.len()..columns).map(|_| div().flex_1().min_w_0()))
            .into_any_element()
    }

    fn summary(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let mut cards = summary::cards(&self.request.route, selection);
        if let Some(card) = cards.iter_mut().find(|card| card.label == "Process power") {
            if let Some(history) = self.snapshot.as_ref().and_then(|snapshot| {
                snapshot.attribution_history(crate::live_app_attribution_history::Metric::Power)
            }) {
                card.reading = history.headline(history.index(self.reading_target()));
                card.note = "Attributed Apps; separate scope".into();
            }
        }
        let mut tiles = div()
            .grid()
            .grid_cols(summary_columns(self.viewport_size.width, cards.len()))
            .gap(px(8.0));
        for card in cards {
            tiles = tiles.child(Self::summary_card(&card, cx));
        }

        div()
            .id("resource-summary")
            .role(gpui::Role::Group)
            .aria_label("Resource readings at the selected time")
            .child(tiles)
            .children(
                self.resource_notice()
                    .map(|notice| div().pt(px(8.0)).child(notice)),
            )
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
            .flex()
            .flex_col()
            .gap(px(3.0))
            .min_w_0()
            .p(px(12.0))
            .rounded(px(8.0))
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
                    .debug_selector(|| format!("summary-label:{}", card.label))
                    .text_size(px(12.0))
                    .text_color(rgb(MUTED))
                    .child(card.label),
            )
            .child(
                div()
                    .debug_selector(|| format!("summary-value:{}", card.label))
                    .text_size(px(22.0))
                    .line_height(px(30.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(card.color))
                    .child(card.reading.clone()),
            )
            .child(
                div()
                    .debug_selector(|| format!("summary-note:{}", card.label))
                    .text_size(px(11.0))
                    .line_height(px(16.0))
                    .text_color(rgb(MUTED))
                    .child(card.note.clone()),
            )
    }

    fn summary_navigate(&mut self, route: &str, cx: &mut Context<Self>) {
        if let Some(target) = self.reading_target().cloned() {
            self.request.second = Some(target.second);
            self.request.identity = target.identity;
        }
        self.navigate(route, cx);
    }

    pub(crate) fn memory_quantities(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

        let note = div()
            .mt(px(12.0))
            .text_size(px(11.0))
            .text_color(rgb(MUTED))
            .child(
                "Independent VM categories overlap and are not an additive used-memory total. \
                 Speculative is included in Free.",
            );

        div()
            .mt(px(16.0))
            .p(px(18.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(self.disclosure(crate::live_sections::Section::MemoryQuantities, cx))
            .when(
                self.sections
                    .expanded(crate::live_sections::Section::MemoryQuantities),
                |panel| {
                    panel
                        .child(div().mt(px(12.0)).text_size(px(12.0)).child(values))
                        .child(note)
                },
            )
    }

    fn computer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        computer_view::view(self, cx)
    }
}

fn summary_columns(width: gpui::Pixels, count: usize) -> u16 {
    assert!((1..=6).contains(&count));
    if width < px(640.0) {
        return 2;
    }

    if width < px(1100.0) {
        return 4;
    }

    match u16::try_from(count) {
        Ok(count) => count,
        Err(_) => unreachable!("Summary cards are bounded to six"),
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

/// A section title as the prototype sets it, with its note on the right where it has one.
fn heading(title: &str) -> gpui::Div {
    let note = match title {
        "Top apps at the selected time" => "Select an app to inspect its processes",
        _ => "",
    };
    let selector = title.to_owned();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .pt(px(12.0))
        .child(
            div()
                .debug_selector(move || selector.clone())
                .text_size(px(14.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(crate::charts::MUTED))
                .child(note),
        )
}
