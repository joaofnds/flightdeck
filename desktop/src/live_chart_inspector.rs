use std::rc::Rc;

use gpui::{Context, FocusHandle, SharedString, Window, div, prelude::*, px, rgb};
use num_traits::ToPrimitive;

use crate::charts::{GRID, MUTED, PANEL};
use crate::dashboard::control;
use crate::data::Panel;
use crate::live_dashboard::LiveDashboard;
use crate::live_presentation::clock;

pub(crate) struct ChartInspector {
    panel_id: String,
    summaries: Vec<SampleSummary>,
    focus: FocusHandle,
}

struct SampleSummary {
    mean: Option<f32>,
    maximum: Option<f32>,
    available: u32,
    total: usize,
}

impl LiveDashboard {
    pub(crate) fn open_chart_inspector(
        &mut self,
        panel: &Rc<Panel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.chart_inspector = Some(ChartInspector {
            panel_id: panel.id.clone(),
            summaries: chart_summaries(panel),
            focus,
        });
        cx.notify();
    }

    pub(crate) fn refresh_chart_inspector(&mut self) {
        let Some(inspector) = &mut self.chart_inspector else {
            return;
        };

        if let Some(panel) = self
            .panels
            .iter()
            .take(16)
            .find(|panel| panel.id == inspector.panel_id)
        {
            inspector.summaries = chart_summaries(panel);
        } else {
            self.chart_inspector = None;
        }
    }

    pub(crate) fn chart_inspector_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let inspector = self.chart_inspector.as_ref()?;
        let panel = self
            .panels
            .iter()
            .take(16)
            .find(|panel| panel.id == inspector.panel_id)?;
        let dialog = self.chart_inspector_dialog(panel, inspector, cx);

        Some(
            div()
                .id("chart-data-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.0))
                .bg(gpui::rgba(0x00_00_00_88))
                .child(dialog),
        )
    }

    fn chart_inspector_dialog(
        &self,
        panel: &Panel,
        inspector: &ChartInspector,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let second = self
            .reading_target()
            .map(|target| target.second)
            .or_else(|| self.snapshot.as_ref()?.selection["second"].as_u64());
        let selected = second.map_or_else(|| "Unavailable".into(), |second| clock(second, true));
        div()
            .id("chart-data-dialog")
            .role(gpui::Role::Dialog)
            .aria_label(format!("{} chart data", panel.title))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_chart_inspector(window, cx);
                    window.prevent_default();
                }
                if event.keystroke.key == "tab" {
                    window.prevent_default();
                }
                cx.stop_propagation();
            }))
            .w_full()
            .max_w(px(880.0))
            .max_h(px((f32::from(self.viewport_size.height) - 48.0).max(120.0)))
            .overflow_y_scroll()
            .p(px(20.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(Self::chart_inspector_heading(panel, &inspector.focus, cx))
            .child(div().mt(px(12.0)).child(format!("Selected {selected}")))
            .children(panel.help.as_ref().map(|help| {
                div()
                    .mt(px(10.0))
                    .text_color(rgb(MUTED))
                    .child(help.clone())
            }))
            .child(self.chart_inspector_table(panel, inspector))
            .child(div().text_color(rgb(MUTED)).child(
                "Recorded samples in the visible chart. Mean and maximum summarize available \
                samples; they do not recover between-sample peaks.",
            ))
    }

    fn chart_inspector_heading(
        panel: &Panel,
        focus: &FocusHandle,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(18.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(panel.title.clone()),
            )
            .child(
                control("Close")
                    .debug_selector(|| "chart-data-close".into())
                    .aria_label("Close chart data")
                    .track_focus(focus)
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                        if ["enter", "space"].contains(&event.keystroke.key.as_str()) {
                            this.close_chart_inspector(window, cx);
                            window.prevent_default();
                            cx.stop_propagation();
                        }
                    }))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_chart_inspector(window, cx);
                    })),
            )
    }

    fn close_chart_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chart_inspector = None;
        self.focus_inspection(window, cx);
        cx.notify();
    }

    fn chart_inspector_table(&self, panel: &Panel, inspector: &ChartInspector) -> impl IntoElement {
        let mut table = div().min_w(px(720.0)).child(
            chart_data_row([
                "Series".into(),
                "Selected".into(),
                "Sample mean".into(),
                "Sample maximum".into(),
                "Available samples".into(),
            ])
            .font_weight(gpui::FontWeight::SEMIBOLD),
        );
        let index = self.chart_inspection_index(panel);
        let mut visible_count = 0;
        for (line_index, line) in panel.lines.iter().take(16).enumerate() {
            if !self.chart_series_visible(panel, line_index) {
                continue;
            }

            visible_count += 1;
            let summary = &inspector.summaries[line_index];
            let unit = line.unit.as_deref().unwrap_or(&panel.unit);
            table = table.child(chart_data_row([
                line.name.clone(),
                self.chart_line_reading(panel, line_index, index),
                super::line_reading(panel, summary.mean, unit),
                super::line_reading(panel, summary.maximum, unit),
                format!("{} / {}", summary.available, summary.total),
            ]));
        }
        if visible_count == 0 {
            table = table.child("No visible series. Enable one in the chart legend.");
        }

        div()
            .id("chart-data-table")
            .my(px(16.0))
            .overflow_x_scroll()
            .child(table)
    }

    fn chart_series_visible(&self, panel: &Panel, line_index: usize) -> bool {
        !self
            .hidden
            .contains(&format!("{}\t{}", panel.id, panel.lines[line_index].name))
    }

    pub(crate) fn chart_tooltip(
        &self,
        panel: &Panel,
        index: Option<usize>,
        fraction: Option<f32>,
    ) -> Option<gpui::Div> {
        if self.reading_target_source().map(SharedString::as_ref) != Some(panel.id.as_str()) {
            return None;
        }

        let second = self.reading_target()?.second;
        let fraction = fraction?;
        let mut indices: Vec<_> = (0..panel.lines.len().min(16))
            .filter(|line| self.chart_series_visible(panel, *line))
            .collect();
        indices.sort_by(|left, right| self.chart_tooltip_order(panel, index, *left, *right));
        let mut tooltip = div()
            .debug_selector(|| format!("chart-tooltip:{}", panel.id))
            .absolute()
            .top(px(8.0))
            .w(px(252.0))
            .max_w_full()
            .p(px(10.0))
            .rounded(px(3.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(0x11_12_17))
            .text_size(px(12.0))
            .child(div().mb(px(4.0)).child(clock(second, true)));
        if fraction > 0.5 {
            tooltip = tooltip.right(gpui::relative((1.0 - fraction).max(0.02)));
        } else {
            tooltip = tooltip.left(gpui::relative(fraction.max(0.02)));
        }
        for (rank, line_index) in indices.into_iter().take(16).enumerate() {
            tooltip = tooltip.child(self.chart_tooltip_row(panel, line_index, index, rank));
        }

        Some(tooltip)
    }

    fn chart_tooltip_order(
        &self,
        panel: &Panel,
        index: Option<usize>,
        left: usize,
        right: usize,
    ) -> std::cmp::Ordering {
        if let Some(history) = self.chart_attribution_history(panel) {
            let left = history.amount(left, index);
            let right = history.amount(right, index);
            return match (left, right) {
                (Some(left), Some(right)) => right.total_cmp(&left),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            };
        }
        if panel.id == crate::live_resident_history::PANEL_ID {
            let history = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.resident_history.as_ref());
            let left = history.and_then(|history| history.amount(left, index));
            let right = history.and_then(|history| history.amount(right, index));

            return right.cmp(&left);
        }
        let left = super::chart_line_value(panel, left, index);
        let right = super::chart_line_value(panel, right, index);

        match (left, right) {
            (Some(left), Some(right)) => right.total_cmp(&left),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    }

    fn chart_tooltip_row(
        &self,
        panel: &Panel,
        line_index: usize,
        index: Option<usize>,
        rank: usize,
    ) -> gpui::Div {
        let line = &panel.lines[line_index];
        let reading = self.chart_line_reading(panel, line_index, index);
        div()
            .debug_selector(|| {
                format!(
                    "chart-tooltip-row:{}:{rank}:{}:{reading}",
                    panel.id, line.name
                )
            })
            .flex()
            .items_center()
            .gap(px(8.0))
            .py(px(2.0))
            .child(
                div()
                    .w(px(12.0))
                    .h(px(3.0))
                    .flex_shrink_0()
                    .bg(rgb(crate::charts::color(&line.color))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .child(line.name.clone()),
            )
            .child(div().child(reading))
    }
}

fn chart_data_row(values: [String; 5]) -> gpui::Div {
    let mut row = div()
        .flex()
        .border_b_1()
        .border_color(rgb(GRID))
        .py(px(9.0));
    for value in values {
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .px(px(8.0))
                .debug_selector(|| value.clone())
                .child(value),
        );
    }
    row
}

fn chart_summaries(panel: &Panel) -> Vec<SampleSummary> {
    panel
        .lines
        .iter()
        .take(16)
        .map(|line| {
            assert!(line.values.len() <= crate::data::LIVE_SAMPLE_LIMIT);
            let mut sum = 0.0_f64;
            let mut maximum: Option<f32> = None;
            let mut available = 0_u32;
            for value in line
                .values
                .iter()
                .take(crate::data::LIVE_SAMPLE_LIMIT)
                .flatten()
            {
                if value.is_finite() {
                    sum += f64::from(*value);
                    maximum = Some(maximum.map_or(*value, |maximum| maximum.max(*value)));
                    available += 1;
                }
            }
            let mean = if available == 0 {
                None
            } else {
                (sum / f64::from(available)).to_f32()
            };
            SampleSummary {
                mean,
                maximum,
                available,
                total: line.values.len(),
            }
        })
        .collect()
}
