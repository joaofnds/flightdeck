use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;
use std::rc::Rc;

use crate::charts::{GRID, MUTED, PANEL};
use crate::live_dashboard::{LiveDashboard, Row};
use crate::live_diagnostics::SelfPart;
use crate::live_sections::Section;

#[path = "live_chrome.rs"]
pub(crate) mod chrome;
#[path = "live_computer_view.rs"]
mod computer_view;
#[path = "live_cpu_top_view.rs"]
mod cpu_top_view;
#[path = "live_summary.rs"]
pub(crate) mod summary;
#[path = "live_system_context.rs"]
mod system_context;
#[path = "live_utilization.rs"]
mod utilization;
pub(crate) use summary::matching_summary;

impl LiveDashboard {
    pub fn row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        if let Row::Spacer(height) = &self.rows[index] {
            return div().h(*height).into_any_element();
        }

        let content = self.row_content(index, cx);
        let (padding, padding_bottom) = Self::row_padding(&self.rows[index]);
        let above = self.rows[..index]
            .iter()
            .rev()
            .find(|row| !matches!(row, Row::Spacer(_)));
        let padding = above.map_or(padding, |above| {
            Self::spacing_above(above, &self.rows[index])
                .map_or(padding, |gap| gap - Self::row_padding(above).1)
        });
        let framed = self.details_frame(index);
        let key = self.rows[index].layout_key();
        let measured = Rc::clone(&self.row_heights);
        let measured_key = key.clone();
        let height = self.reading_row_height(&self.rows[index]);
        let scroll_reserved = matches!(&self.rows[index], Row::CPUTop(_) | Row::CPUActivity);
        div()
            .id(gpui::SharedString::from(key))
            .relative()
            .when_some(height, |row, height| {
                row.h(height).when(
                    scroll_reserved,
                    gpui::StatefulInteractiveElement::overflow_y_scroll,
                )
            })
            .px(px(if matches!(&self.rows[index], Row::Footer) {
                0.0
            } else {
                24.0
            }))
            .pt(px(if framed.is_some_and(|f| !f.top) {
                0.0
            } else {
                padding
            }))
            .pb(px(if framed.is_some_and(|f| !f.bottom) {
                0.0
            } else {
                padding_bottom
            }))
            .child(match framed {
                Some(frame) => details_frame(frame, content),
                None => content,
            })
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

    /// Where this row sits in a details panel, which the prototype draws as one bordered panel
    /// around the summary and what it folds. The rows are separate list items, so each draws its
    /// own slice of the panel.
    /// The room above and below a row. Two rows are as far apart as the lower padding of the
    /// first and the upper padding of the second, unless `spacing_above` names the distance.
    fn row_padding(row: &Row) -> (f32, f32) {
        match row {
            Row::Summary => (8.0, 12.0),
            Row::Charts { .. }
            | Row::ContextCharts { .. }
            | Row::EnergyCooling { .. }
            | Row::OverviewStatus
            | Row::BatteryState(_)
            | Row::Temperature(_, _, _)
            | Row::GpuApps(_)
            | Row::NetworkApps(_)
            | Row::CPUTop(_) => (4.0, 4.0),
            Row::OverviewRankings(_) => (6.0, 6.0),
            Row::Heading(_) => (16.0, 8.0),
            Row::SelfPart(SelfPart::Overview) => (8.0, 0.0),
            Row::SelfPart(_)
            | Row::App(_)
            | Row::Executable(_)
            | Row::AppColumns("executables")
            | Row::Disclosure(Section::AppTotals)
            | Row::ProcessGroupingAbout => (0.0, 0.0),
            Row::ProcessGroupBar => (0.0, 12.0),
            _ => (8.0, 8.0),
        }
    }

    /// The distance the prototype puts between a row and the one before it, where the
    /// paddings alone would not give it.
    fn spacing_above(above: &Row, row: &Row) -> Option<f32> {
        match (above, row) {
            (Row::SelfPart(_), Row::SelfPart(SelfPart::Reports | SelfPart::Minutes)) => Some(12.0),
            (Row::SelfPart(_), Row::SelfPart(_)) => Some(20.0),
            (Row::Heading(_), _) => Some(10.0),
            (Row::ProcessGroupingAbout, Row::ProcessGroupBar)
            | (Row::Charts { .. }, Row::Disclosure(Section::AppTotals)) => Some(14.0),
            (_, Row::ProcessGroupingAbout)
            | (Row::Disclosure(Section::AppTotals), Row::Charts { .. }) => Some(0.0),
            (Row::AppSelector, Row::Heading(_)) | (Row::Summary, _) | (_, Row::Disclosure(_)) => {
                Some(16.0)
            }
            (above, Row::Heading(_)) if above.in_details() => Some(20.0),
            _ => None,
        }
    }

    fn details_frame(&self, index: usize) -> Option<Frame> {
        let member = |row: &Row| row.in_details();
        let row = &self.rows[index];
        if !member(row) {
            return None;
        }

        let next = self.rows.get(index + 1);
        Some(Frame {
            top: matches!(row, Row::Disclosure(_)),
            bottom: next.is_none_or(|next| !member(next) || matches!(next, Row::Disclosure(_))),
        })
    }

    fn row_content(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        match &self.rows[index] {
            Row::Summary => self.summary(cx).into_any_element(),
            Row::SelfPart(part) => self.self_part(*part, cx),
            Row::CPUTop(panel) => self.cpu_top(panel.as_ref(), cx).into_any_element(),
            Row::CPUActivity => self.cpu_activity().into_any_element(),
            Row::CPUHistory => self.cpu_history_panel(cx).into_any_element(),
            Row::SystemContext(panel) => self.system_context(panel.as_ref(), cx).into_any_element(),
            Row::OverviewStatus => self.overview_status(cx).into_any_element(),
            Row::BatteryState(panel) => self.battery_state(panel.as_ref(), cx),
            Row::Temperature(_, sensor, panel) => self.temperature(sensor, panel.as_ref(), cx),
            Row::OverviewRankings(slots) => {
                self.overview_rankings(slots, self.reading_chart_columns(), cx)
            }
            Row::OverviewAppHistories(slots) => self.overview_app_histories(slots, cx),
            Row::OverviewAnalysis(panel) => self.overview_analysis(panel.as_ref(), cx),
            Row::MemoryAttribution(panel) => self.memory_attribution(panel.as_ref(), cx),
            Row::MemoryQuantities => self.memory_quantities().into_any_element(),
            Row::AppAttribution(metric, panel) => self.app_attribution(*metric, panel.as_ref(), cx),
            Row::GpuApps(panel) => self.gpu_apps(panel.as_ref(), cx),
            Row::NetworkApps(panel) => self.network_apps(panel.as_ref(), cx),
            Row::AppSelector => self.app_selector(cx).into_any_element(),
            Row::ProcessGroupBar => self.process_group_bar(cx).into_any_element(),
            Row::ProcessGroupingAbout => self.process_grouping_about().into_any_element(),
            Row::AppColumns("executables") => {
                crate::live_executables::table_header(&self.app_table_scroll).into_any_element()
            }
            Row::AppColumns(kind) => {
                crate::live_app_views::app_table_columns(kind, &self.app_table_scroll)
                    .into_any_element()
            }
            Row::Charts { panels, columns } | Row::ContextCharts { panels, columns } => {
                self.row_content_charts(panels, *columns, cx)
            }
            Row::EnergyCooling {
                fans,
                thermal,
                maxima,
            } => self.energy_cooling(&[fans, thermal, maxima], cx),
            Row::Heading(title) => heading(title).into_any_element(),
            Row::Disclosure(section) => self.disclosure(*section, cx).into_any_element(),
            Row::Hardware => self.live_hardware(cx).into_any_element(),
            Row::App(index) => self.app_row(*index, cx),
            Row::Executable(index) => self.executable_row(*index, cx),
            Row::Inventory => self.computer(cx).into_any_element(),
            Row::Footer => self.footer().into_any_element(),
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
                let chart = if panel.kind == "timeline" {
                    self.live_timeline(panel, crate::live_chart_view::timeline::BAND_FULL, cx)
                } else {
                    self.live_chart(panel, cx).into_any_element()
                };
                div().flex_1().min_w_0().flex().child(chart)
            }))
            .children((panels.len()..columns).map(|_| div().flex_1().min_w_0()))
            .into_any_element()
    }

    /// The summary cards of the page, as the screen shows them.
    pub(crate) fn summary_cards(&self) -> Vec<summary::Card> {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        if self.request.route == "apps" {
            return crate::live_app_views::app_cards(self.current_app(), selection);
        }
        if self.request.route == "self" {
            return crate::self_monitoring::cards(&self.self_reports);
        }

        let mut cards = summary::cards(&self.request.route, selection);
        if let Some(card) = cards.iter_mut().find(|card| card.label == "Process power") {
            if let Some(history) = self.snapshot.as_ref().and_then(|snapshot| {
                snapshot.attribution_history(crate::live_app_attribution_history::Metric::Power)
            }) {
                card.reading = history.total_rate(history.index(self.reading_target()));
                card.note = "Attributed processes; separate scope".into();
                if card.reading != "Unavailable" {
                    card.color = crate::charts::TEXT;
                }
            }
        }
        cards
    }

    fn summary(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cards = self.summary_cards();
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

    pub(crate) fn memory_quantities(&self) -> impl IntoElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let keys = [
            "memory-app",
            "memory-wired",
            "memory-compressed",
            "memory-cached",
            "memory-free",
            "memory-pressure",
        ];
        let readings = crate::live_memory::quantities(selection)
            .into_iter()
            .zip(keys)
            .map(|(row, key)| (key, row.name, row.reading))
            .collect();

        crate::live_presentation::quantity_panel(
            "memory-quantities",
            "Captured memory quantities",
            "Memory quantities at the selected time. Independent VM categories overlap and are \
             not an additive used-memory total.",
            readings,
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
pub(crate) fn heading(title: &str) -> gpui::Div {
    let note = heading_note(title);
    let selector = title.to_owned();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .child(
            div()
                .debug_selector(move || selector.clone())
                .text_size(px(14.0))
                .line_height(px(21.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .child(
            div()
                .debug_selector(move || note.into())
                .text_size(px(11.0))
                .text_color(rgb(crate::charts::MUTED))
                .child(note),
        )
}

fn heading_note(title: &str) -> &'static str {
    if title == "Top apps at the selected time" {
        return "Select an app to inspect its processes";
    }
    if title.ends_with(" by app") {
        return "At the selected sample · select an app to inspect";
    }
    if title.starts_with("Inside ") {
        return "Grouped by process name, not PID";
    }
    if title == "Where collector CPU goes" {
        return "Ten-second report ending at the inspected run time";
    }
    ""
}

/// A row's slice of a details panel.
#[derive(Clone, Copy)]
struct Frame {
    top: bool,
    bottom: bool,
}

/// The panel's sides, with its corners and padding on the rows that begin and end it. The gap
/// between rows is the prototype's eight pixels, half of it on each side of the seam.
fn details_frame(frame: Frame, content: AnyElement) -> AnyElement {
    div()
        .px(px(12.0))
        .pt(px(if frame.top { 8.0 } else { 4.0 }))
        .pb(px(if frame.bottom { 8.0 } else { 4.0 }))
        .border_l_1()
        .border_r_1()
        .border_color(rgb(crate::charts::LINE))
        .bg(rgb(crate::charts::PANEL))
        .when(frame.top, |panel| panel.border_t_1().rounded_t(px(12.0)))
        .when(frame.bottom, |panel| panel.border_b_1().rounded_b(px(12.0)))
        .child(content)
        .into_any_element()
}
