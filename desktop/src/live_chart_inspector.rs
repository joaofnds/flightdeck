use std::rc::Rc;

use gpui::{
    Context, FocusHandle, SharedString, Window, anchored, deferred, div, prelude::*, px, rgb,
};
use num_traits::ToPrimitive;

use crate::charts::{GRID, MUTED, PANEL};
use crate::dashboard::control;
use crate::data::{LineExtremes, MachinePlace, Panel};
use crate::live_dashboard::LiveDashboard;
use crate::live_presentation::clock;

pub(crate) struct ChartInspector {
    panel_id: String,
    summaries: Vec<SampleSummary>,
    /// What the whole of the chart's stack reached, where one of its lines names that.
    stack_total: Option<SampleSummary>,
    focus: FocusHandle,
    /// The focus of the control that holds each row's maximum, the stack's total last.
    maximum_focus: Vec<FocusHandle>,
}

struct SampleSummary {
    mean: Option<f32>,
    maximum: Option<f32>,
    /// The reading that read the maximum, where the chart names it.
    maximum_pick: Option<MaximumPick>,
    available: u32,
    total: usize,
}

/// The reading a row's maximum names, which its control holds.
#[derive(Clone, Copy, Debug, PartialEq)]
enum MaximumPick {
    /// The reading itself, where the panel names its readings. Two runs can read the same wall
    /// second.
    Reading(MachinePlace),
    /// The second it was read in, where the panel names none.
    Second(u64),
}

impl MaximumPick {
    fn second(self) -> u64 {
        match self {
            Self::Reading(place) => place.second(),
            Self::Second(second) => second,
        }
    }
}

/// The most rows the table can give: a line's for each of its sixteen, then the stack's total.
const CHART_DATA_ROWS_MAX: usize = 17;
/// The column of a row that says when its maximum was read, and holds that reading.
const CHART_DATA_READ_AT_COLUMN: usize = 4;

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
            stack_total: chart_stack_total(panel),
            focus,
            maximum_focus: (0..CHART_DATA_ROWS_MAX)
                .map(|_| cx.focus_handle())
                .collect(),
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
            inspector.stack_total = chart_stack_total(panel);
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
            .or_else(|| self.shown_second());
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
                    this.chart_inspector_tab(event.keystroke.modifiers.shift, window, cx);
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
            .child(self.chart_inspector_table(panel, inspector, cx))
            .child(div().text_color(rgb(MUTED)).child(
                "Recorded samples in the visible chart. A sample that covers several seconds \
                counts toward the mean once, and toward the maximum by the highest reading of \
                those seconds. They do not recover peaks between readings. Picking when a maximum \
                was read holds that reading on every page.",
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

    /// Moves the focus to the dialog's next control, or its previous one when `backward`, staying
    /// within the dialog.
    fn chart_inspector_tab(&self, backward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let stops = self.chart_inspector_stops();
        if stops.is_empty() {
            return;
        }
        let count = stops.len();
        let at = stops
            .iter()
            .position(|stop| stop.is_focused(window))
            .unwrap_or(0);
        let next = if backward {
            (at + count - 1) % count
        } else {
            (at + 1) % count
        };

        window.focus(&stops[next], cx);
    }

    /// The controls of the dialog in the order the keyboard reaches them.
    fn chart_inspector_stops(&self) -> Vec<FocusHandle> {
        let Some(inspector) = &self.chart_inspector else {
            return Vec::new();
        };
        let Some(panel) = self
            .panels
            .iter()
            .take(16)
            .find(|panel| panel.id == inspector.panel_id)
        else {
            return Vec::new();
        };

        let mut stops = vec![inspector.focus.clone()];
        for (line_index, summary) in inspector.summaries.iter().enumerate() {
            if summary.maximum_pick.is_some() && self.chart_series_visible(panel, line_index) {
                stops.push(inspector.maximum_focus[line_index].clone());
            }
        }
        if chart_stack_total_row(inspector).is_some() {
            stops.push(inspector.maximum_focus[inspector.summaries.len()].clone());
        }
        stops
    }

    /// Lets go of the dialog and holds the reading `pick` names on every page.
    fn chart_inspector_hold(
        &mut self,
        pick: MaximumPick,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_chart_inspector(window, cx);
        self.pointer = crate::live_hover::controller::PointerPlace::AtHeldPick;
        match pick {
            MaximumPick::Reading(place) => {
                self.pin_target(crate::live_hover::Target::of_place(&place), cx);
            }
            MaximumPick::Second(second) => self.pin_second(second, cx),
        }
    }

    /// The control of the row `name` that says when its maximum was read and holds that reading.
    fn chart_inspector_maximum(
        panel: &Panel,
        name: &str,
        pick: MaximumPick,
        focus: &FocusHandle,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let read_at = crate::chart_axes::clock(pick.second(), true);
        let selector = format!("chart-maximum:{}:{name}", panel.id);

        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(|| selector)
            .role(gpui::Role::Button)
            .aria_label(format!(
                "Hold the highest reading of {name}, read at {read_at}"
            ))
            .flex()
            .items_center()
            .min_h(px(24.0))
            .w_full()
            .px(px(8.0))
            .rounded(px(5.0))
            .bg(rgb(0x22_25_2b))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus(crate::charts::focus_ring)
            .hover(|style| style.bg(rgb(0x2c_2f_35)))
            .track_focus(focus)
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if ["enter", "space"].contains(&event.keystroke.key.as_str()) {
                        this.chart_inspector_hold(pick, window, cx);
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.chart_inspector_hold(pick, window, cx);
            }))
            .child(read_at)
    }

    fn chart_inspector_table(
        &self,
        panel: &Panel,
        inspector: &ChartInspector,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut table = div().min_w(px(720.0)).child(
            chart_data_row(
                [
                    "Series".into(),
                    "Selected".into(),
                    "Sample mean".into(),
                    "Sample maximum".into(),
                    "Maximum read at".into(),
                    "Available samples".into(),
                ],
                None,
            )
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
            let maximum = summary.maximum_pick.map(|pick| {
                let focus = &inspector.maximum_focus[line_index];
                Self::chart_inspector_maximum(panel, &line.name, pick, focus, cx)
            });
            table = table.child(chart_data_row(
                chart_data_cells(
                    panel,
                    line,
                    self.chart_line_reading(panel, line_index, index),
                    summary,
                ),
                maximum,
            ));
        }
        if visible_count == 0 {
            table = table.child("No visible series. Enable one in the chart legend.");
        }
        if let Some((total, pick)) = chart_stack_total_row(inspector) {
            let focus = &inspector.maximum_focus[inspector.summaries.len()];
            let maximum = Self::chart_inspector_maximum(panel, CHART_DATA_TOTAL, pick, focus, cx);
            table = table.child(chart_data_row(
                chart_data_total_cells(panel, total),
                Some(maximum),
            ));
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
            .contains(&self.chart_legend_key(panel, line_index))
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
        let indices: Vec<_> = (0..panel.lines.len().min(16))
            .filter(|line| self.chart_series_visible(panel, *line))
            .collect();
        let mut tooltip = div()
            .debug_selector(|| format!("chart-tooltip:{}", panel.id))
            .w(px(252.0))
            .p(px(10.0))
            .rounded(px(3.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(0x11_12_17))
            .text_size(px(12.0))
            .child(div().mb(px(4.0)).child(clock(second, true)));
        for (rank, line_index) in indices.into_iter().enumerate() {
            tooltip = tooltip.child(self.chart_tooltip_row(panel, line_index, index, rank));
        }

        // The padding keeps the pointer clear of the tooltip on whichever side it flips to.
        let mut overlay = anchored().child(div().p(px(TOOLTIP_POINTER_GAP)).child(tooltip));
        if let Some(pointer) = self.chart_pointer {
            overlay = overlay.position(pointer);
        }

        Some(
            div()
                .absolute()
                .top(crate::charts::PLOT_INSETS.top)
                .left(gpui::relative(fraction))
                .child(deferred(overlay)),
        )
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
        tooltip_row(crate::charts::color(&line.color), &line.name, &reading).debug_selector(|| {
            format!(
                "chart-tooltip-row:{}:{rank}:{}:{reading}",
                panel.id, line.name
            )
        })
    }
}

/// One series in a tooltip: its swatch, name and value on a line, and the coverage that
/// qualifies the value, if any, muted on a line under the name.
fn tooltip_row(color: u32, name: &str, reading: &str) -> gpui::Div {
    let (value, detail) = split_reading(reading);
    let headline = div()
        .flex()
        .items_center()
        .gap(px(TOOLTIP_GAP))
        .child(
            div()
                .w(px(TOOLTIP_SWATCH))
                .h(px(3.0))
                .flex_shrink_0()
                .bg(rgb(color)),
        )
        .child(
            div()
                .debug_selector(|| "tooltip-row-name".into())
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(name.to_owned()),
        )
        .child(
            div()
                .flex_shrink_0()
                .whitespace_nowrap()
                .child(value.to_owned()),
        );

    div()
        .py(px(2.0))
        .child(headline)
        .children(detail.map(|detail| {
            div()
                .debug_selector(|| "tooltip-row-detail".into())
                .pl(px(TOOLTIP_SWATCH + TOOLTIP_GAP))
                .text_size(px(11.0))
                .line_height(px(15.0))
                .text_color(rgb(MUTED))
                .child(detail.to_owned())
        }))
}

const TOOLTIP_GAP: f32 = 8.0;
const TOOLTIP_POINTER_GAP: f32 = 12.0;
const TOOLTIP_SWATCH: f32 = 12.0;

/// Splits a reading into its value and the coverage that qualifies it, so a long qualifier
/// sits on a line of its own under the series name and never squeezes the name out.
fn split_reading(reading: &str) -> (&str, Option<&str>) {
    let separators = [" · ", " known subtotal"];
    let Some(at) = separators
        .iter()
        .filter_map(|separator| reading.find(separator))
        .min()
    else {
        return (reading, None);
    };
    let (value, rest) = reading.split_at(at);

    (value, Some(rest.trim_start_matches(" · ").trim()))
}

/// What the Data table's row of `line` says, beside what its selected second reads.
fn chart_data_cells(
    panel: &Panel,
    line: &crate::data::Line,
    selected: String,
    summary: &SampleSummary,
) -> [String; 6] {
    let unit = line.unit.as_deref().unwrap_or(&panel.unit);

    [
        line.name.clone(),
        selected,
        super::line_reading(panel, summary.mean, unit),
        super::line_reading(panel, summary.maximum, unit),
        chart_data_read_at(summary),
        format!("{} / {}", summary.available, summary.total),
    ]
}

/// What the Data table's row of a stack's whole says: how far it reached and when, since the rows
/// of its lines say what each read and a mean of the whole is nothing the chart draws.
fn chart_data_total_cells(panel: &Panel, total: &SampleSummary) -> [String; 6] {
    [
        CHART_DATA_TOTAL.into(),
        String::new(),
        String::new(),
        super::line_reading(panel, total.maximum, &panel.unit),
        chart_data_read_at(total),
        String::new(),
    ]
}

fn chart_data_read_at(summary: &SampleSummary) -> String {
    summary.maximum_pick.map_or_else(String::new, |pick| {
        crate::chart_axes::clock(pick.second(), true)
    })
}

/// A row of the table, whose column of when its maximum was read is `maximum` where it has one.
fn chart_data_row(values: [String; 6], maximum: Option<gpui::Stateful<gpui::Div>>) -> gpui::Div {
    let mut row = div()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(GRID))
        .py(px(9.0));
    let mut maximum = maximum;
    for (column, value) in values.into_iter().enumerate() {
        let cell = div()
            .flex_1()
            .min_w_0()
            .px(px(8.0))
            .debug_selector(|| value.clone());
        row = row.child(
            match maximum.take_if(|_| column == CHART_DATA_READ_AT_COLUMN) {
                Some(maximum) => cell.child(maximum),
                None => cell.child(value),
            },
        );
    }
    row
}

/// What the table's row of a stack's whole names.
const CHART_DATA_TOTAL: &str = "Total";

/// The summary of the stack's whole and the reading its maximum names, where the table has a row
/// for it.
fn chart_stack_total_row(inspector: &ChartInspector) -> Option<(&SampleSummary, MaximumPick)> {
    let total = inspector.stack_total.as_ref()?;

    Some((total, total.maximum_pick?))
}

fn chart_summaries(panel: &Panel) -> Vec<SampleSummary> {
    let lower = super::chart_inspection(panel, None).lower;

    panel
        .lines
        .iter()
        .take(16)
        .map(|line| {
            assert!(line.values.len() <= crate::data::LIVE_SAMPLE_LIMIT);
            let mut sum = 0.0_f64;
            let mut maximum: Option<(f32, Option<MaximumPick>)> = None;
            let mut available = 0_u32;
            let values = line.values.iter().take(crate::data::LIVE_SAMPLE_LIMIT);
            for (index, value) in values.enumerate().skip(lower) {
                let value = value.filter(|value| value.is_finite());
                if let Some(value) = value {
                    sum += f64::from(value);
                    available += 1;
                }
                let Some(reading) = chart_summaries_reading(panel, line, index, value) else {
                    continue;
                };
                if maximum.is_none_or(|(highest, _)| reading.0 > highest) {
                    maximum = Some(reading);
                }
            }
            let mean = if available == 0 {
                None
            } else {
                (sum / f64::from(available)).to_f32()
            };
            SampleSummary {
                mean,
                maximum: maximum.map(|(value, _)| value),
                maximum_pick: maximum.and_then(|(_, pick)| pick),
                available,
                total: line.values.len().saturating_sub(lower),
            }
        })
        .collect()
}

/// How far the whole of `panel`'s stack reached in view, where a line of it names that: the highest
/// of what that line names and of what its lines add up to at each reading. No row of a line can
/// say it, since each keeps to its own readings.
fn chart_stack_total(panel: &Panel) -> Option<SampleSummary> {
    let lines: Vec<&crate::data::Line> = panel
        .lines
        .iter()
        .take(16)
        .filter(|line| crate::charts::stacked(panel, line))
        .collect();
    let extremes = lines.iter().find_map(|line| line.extremes.as_ref())?;
    let lower = super::chart_inspection(panel, None).lower;
    let count = lines.iter().map(|line| line.values.len()).max()?;

    let mut maximum: Option<(f32, Option<MaximumPick>)> = None;
    for index in lower..count {
        let added = chart_stack_total_reading(&lines, index);
        let Some(reading) = chart_summaries_highest(panel, Some(extremes), index, added) else {
            continue;
        };
        if maximum.is_none_or(|(highest, _)| reading.0 > highest) {
            maximum = Some(reading);
        }
    }

    Some(SampleSummary {
        mean: None,
        maximum: maximum.map(|(value, _)| value),
        maximum_pick: maximum.and_then(|(_, pick)| pick),
        available: 0,
        total: 0,
    })
}

/// What `lines` add up to at `index`, or `None` where any of them read nothing there, since a sum
/// that left one out would read less than the stack did.
fn chart_stack_total_reading(lines: &[&crate::data::Line], index: usize) -> Option<f32> {
    let mut added = 0.0_f32;
    for line in lines {
        added += line
            .values
            .get(index)
            .copied()
            .flatten()
            .filter(|value| value.is_finite())?;
    }
    Some(added)
}

/// The highest reading the sample `value` at `index` of `line` stands for, and the reading the
/// chart names for it. A stacked line's reach is the stack's total and not its own.
fn chart_summaries_reading(
    panel: &Panel,
    line: &crate::data::Line,
    index: usize,
    value: Option<f32>,
) -> Option<(f32, Option<MaximumPick>)> {
    let extremes = line.extremes.as_ref().filter(|_| panel.stack != Some(true));

    chart_summaries_highest(panel, extremes, index, value)
}

/// The higher of `value` and what `extremes` name at `index`, and the reading that read it.
fn chart_summaries_highest(
    panel: &Panel,
    extremes: Option<&LineExtremes>,
    index: usize,
    value: Option<f32>,
) -> Option<(f32, Option<MaximumPick>)> {
    let reach = extremes
        .and_then(|extremes| extremes[index])
        .filter(|reach| value.is_none_or(|value| reach.high >= value));
    if let Some(reach) = reach {
        return Some((reach.high, Some(MaximumPick::Reading(reach.peak))));
    }

    value.map(|value| (value, chart_summaries_pick(panel, index)))
}

/// The reading at `index` of `panel`: the one it names, or else the second its clock puts it in.
fn chart_summaries_pick(panel: &Panel, index: usize) -> Option<MaximumPick> {
    if let Some(places) = &panel.places {
        return Some(MaximumPick::Reading(places[index]));
    }
    let buckets = crate::charts::panel_buckets(panel)?;
    let positions = panel.positions.as_deref()?;

    buckets.second_of(positions[index]).map(MaximumPick::Second)
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, Window, div, prelude::*, px};

    use super::{
        MaximumPick, chart_data_cells, chart_stack_total, chart_summaries, split_reading,
        tooltip_row,
    };

    struct Probe;

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .debug_selector(|| "probe-tooltip".into())
                .w(px(252.0))
                .p(px(10.0))
                .text_size(px(12.0))
                .child(tooltip_row(
                    0x00f2_cc0c,
                    "rustc",
                    "44.4 W known subtotal · 5 measured · 3 unavailable",
                ))
        }
    }

    #[gpui::test]
    fn a_long_reading_keeps_the_series_name_and_stays_inside_the_tooltip(cx: &mut TestAppContext) {
        // Goal: coverage text under a series never squeezes its name out or spills past the
        // tooltip, as it did when the whole reading shared the name's line. Method: lay out a
        // 252 pixel tooltip with a long reading and measure the name, detail and tooltip.
        let (_, cx) = cx.add_window_view(|_, _| Probe);
        let tooltip = cx.debug_bounds("probe-tooltip").unwrap();
        let name = cx.debug_bounds("tooltip-row-name").unwrap();
        let detail = cx.debug_bounds("tooltip-row-detail").unwrap();

        assert!(name.size.width >= px(60.0), "{name:?}");
        assert!(detail.right() <= tooltip.right(), "{detail:?} {tooltip:?}");
        assert!(detail.top() >= name.bottom());
    }

    /// A panel of one line, its second sample said to reach 95 at wall second 9,000.
    fn reaching_panel(stack: bool) -> crate::data::Panel {
        let mut panel: crate::data::Panel = serde_json::from_value(serde_json::json!({
            "id": "cpu", "title": "CPU", "unit": "%", "kind": "line", "stack": stack,
            "lines": [{"name": "All cores", "color": "#5794f2", "values": [10.0, 20.0]}]}))
        .unwrap();
        panel.lines[0].extremes = Some(vec![
            None,
            Some(crate::data::LineReach {
                low: 1.0,
                high: 95.0,
                peak: crate::data::MachinePlace {
                    wall_ns: 9_000_000_000_000,
                    ..crate::data::MachinePlace::default()
                },
            }),
        ]);

        panel
    }

    #[test]
    fn a_data_row_says_what_a_line_reached_and_when() {
        // Goal: the Data table is the route to a peak, so its row reads the maximum and the second
        // that read it.
        // Method: a line whose sample reaches past what it draws, and read its row.
        let panel = reaching_panel(false);
        let summaries = chart_summaries(&panel);

        let cells = chart_data_cells(&panel, &panel.lines[0], String::new(), &summaries[0]);

        assert_eq!(
            cells[3],
            crate::live_chart_view::line_reading(&panel, Some(95.0), &panel.unit)
        );
        assert_eq!(cells[4], crate::chart_axes::clock(9_000, true));
    }

    #[test]
    fn a_samples_maximum_is_the_highest_reading_it_covers_and_when_that_was_read() {
        // Goal: a chart's table of statistics gives the peak its mean hides and when it was read,
        // and leaves the mean a mean.
        // Method: a sample that reaches past its own mean, and read the summary.
        let summaries = chart_summaries(&reaching_panel(false));

        assert_eq!(summaries[0].maximum, Some(95.0));
        assert_eq!(
            summaries[0].maximum_pick.map(MaximumPick::second),
            Some(9_000)
        );
        assert_eq!(summaries[0].mean, Some(15.0));
    }

    #[test]
    fn a_reach_under_a_samples_own_reading_leaves_that_reading_the_maximum() {
        // Goal: a stretch's reach leaves out the reading a chart draws, so the higher of the two
        // is what the line reached.
        // Method: a sample that reads past what its reach names, and read the summary.
        let mut panel = reaching_panel(false);
        panel.lines[0].values[1] = Some(99.0);

        let summaries = chart_summaries(&panel);

        assert_eq!(summaries[0].maximum, Some(99.0));
    }

    #[test]
    fn a_sample_with_no_reading_of_its_own_still_gives_how_far_it_reaches() {
        // Goal: a stretch whose first second has no rate still reached a peak, so the table gives
        // that peak, and the mean keeps to the samples that have a value.
        // Method: the sample that reaches past the others with no value of its own.
        let mut panel = reaching_panel(false);
        panel.lines[0].values[1] = None;

        let summaries = chart_summaries(&panel);

        assert_eq!(summaries[0].maximum, Some(95.0));
        assert_eq!(
            summaries[0].maximum_pick.map(MaximumPick::second),
            Some(9_000)
        );
        assert_eq!(summaries[0].mean, Some(10.0));
    }

    #[test]
    fn a_stacked_lines_maximum_is_its_own_and_not_the_total_of_its_stack() {
        // Goal: a stack's top line names how far the stack reaches, which is no reading of that
        // line, so its row keeps to its own samples.
        // Method: the same line in a stack, and read the summary.
        let summaries = chart_summaries(&reaching_panel(true));

        assert_eq!(summaries[0].maximum, Some(20.0));
        assert_eq!(summaries[0].maximum_pick, None);
    }

    /// A chart of `values` a second apart from 1,000, across the 900 seconds from `first`.
    fn clocked_panel(first: u64, values: &[Option<f32>]) -> crate::data::Panel {
        let mut panel: crate::data::Panel = serde_json::from_value(serde_json::json!({
            "id": "cpu", "title": "CPU", "unit": "%", "kind": "line",
            "lines": [{"name": "All cores", "color": "#5794f2", "values": values}]}))
        .unwrap();
        let window = crate::chart_axes::TimeWindow { first, span: 900 };
        panel.window = Some(window);
        panel.positions = Some(
            (0..values.len())
                .map(|index| {
                    let second = 1000 + u64::try_from(index).unwrap();
                    let offset = f64::from(u32::try_from(second).unwrap())
                        - f64::from(u32::try_from(first).unwrap());
                    num_traits::ToPrimitive::to_f32(&((offset + 0.5) / 900.0)).unwrap()
                })
                .collect(),
        );
        panel
    }

    #[test]
    fn a_peak_of_one_second_is_its_lines_maximum_at_every_offset() {
        // Goal: a spike one second long stays in the table wherever a bucket boundary falls, so no
        // offset or width of the chart loses it.
        // Method: a steady line with one peak at each second of a minute, and read each summary.
        for peak in 0..60_usize {
            let mut values = vec![Some(10.0); 900];
            values[peak] = Some(95.0);

            let summaries = chart_summaries(&clocked_panel(1000, &values));

            assert_eq!(summaries[0].maximum, Some(95.0));
            assert_eq!(
                summaries[0].maximum_pick.map(MaximumPick::second),
                Some(1000 + u64::try_from(peak).unwrap())
            );
        }
    }

    #[test]
    fn a_panel_that_names_no_readings_holds_a_maximum_by_its_second() {
        // Goal: a chart by App names no reading of its own, so its maximum is held by the second it
        // was read in.
        // Method: a chart with a clock and no places, and read the pick.
        let summaries = chart_summaries(&clocked_panel(1000, &[Some(1.0), Some(9.0), Some(2.0)]));

        assert_eq!(summaries[0].maximum_pick, Some(MaximumPick::Second(1001)));
    }

    #[test]
    fn a_reading_before_the_window_counts_in_no_summary() {
        // Goal: the table says it reads the samples in the visible chart, and a chart holds
        // readings before its window for its first point's mean, so a peak there is no maximum
        // to hold.
        // Method: a window that starts two seconds after a higher reading, and read the summary.
        let summaries = chart_summaries(&clocked_panel(1002, &[Some(95.0), Some(5.0), Some(7.0)]));

        assert_eq!(summaries[0].maximum, Some(7.0));
        assert_eq!(summaries[0].maximum_pick, Some(MaximumPick::Second(1002)));
        assert_eq!(summaries[0].mean, Some(6.0));
        assert_eq!(summaries[0].available, 2);
        assert_eq!(summaries[0].total, 2);
    }

    /// A stack of two lines a second apart, the second naming the stack's reach at its second
    /// point.
    fn stacked_panel() -> crate::data::Panel {
        let mut panel = reaching_panel(true);
        let mut top = panel.lines[0].clone();
        top.name = "Top".into();
        top.values = vec![Some(5.0), Some(1.0)];
        panel.lines[0].extremes = None;
        panel.lines.push(top);

        panel
    }

    #[test]
    fn a_stacks_total_is_the_highest_it_reached_at_one_reading() {
        // Goal: a memory or power spike is a spike of the whole stack, which no line's row can
        // give, and it adds up one reading, never separate maxima.
        // Method: a stack whose top line names a reach past what its lines add up to, and read the
        // total.
        let total = chart_stack_total(&stacked_panel()).unwrap();

        assert_eq!(total.maximum, Some(95.0));
        assert_eq!(total.maximum_pick.map(MaximumPick::second), Some(9_000));
    }

    #[test]
    fn a_stacks_total_adds_up_its_lines_where_they_read_past_what_it_names() {
        // Goal: a stretch's reach leaves out the reading a chart draws, so the higher of the two is
        // what the stack reached.
        // Method: a stack whose lines add up past the reach it names, and read the total.
        let mut panel = stacked_panel();
        panel.lines[0].values = vec![Some(100.0), Some(20.0)];

        let total = chart_stack_total(&panel).unwrap();

        assert_eq!(total.maximum, Some(105.0));
    }

    #[test]
    fn a_stacks_total_adds_up_no_reading_a_line_of_it_missed() {
        // Goal: a sum that left a line out would read a total the stack never read.
        // Method: a stack whose higher reading lacks one of its lines, and read the total.
        let mut panel = stacked_panel();
        panel.lines[0].values = vec![None, Some(20.0)];
        panel.lines[1].extremes = Some(vec![None, None]);

        let total = chart_stack_total(&panel).unwrap();

        assert_eq!(total.maximum, Some(21.0));
    }

    #[test]
    fn a_chart_without_a_stack_or_a_named_reach_has_no_total() {
        // Goal: only a stack that says how far it reached has a total to give.
        // Method: a chart of lines on their own, and a stack that names no reach.
        let mut stack = stacked_panel();
        stack.lines[1].extremes = None;

        assert!(chart_stack_total(&reaching_panel(false)).is_none());
        assert!(chart_stack_total(&stack).is_none());
    }

    #[test]
    fn splits_a_reading_from_the_coverage_that_qualifies_it() {
        // Goal: the tooltip puts a long coverage note under the series, not beside it.
        // Method: split each reading form the charts produce.
        assert_eq!(
            split_reading("44.4 W known subtotal · 5 measured · 3 unavailable"),
            (
                "44.4 W",
                Some("known subtotal · 5 measured · 3 unavailable")
            )
        );
        assert_eq!(
            split_reading("2.5 MiB/s · known subtotal · 5 measured · 1 unavailable"),
            (
                "2.5 MiB/s",
                Some("known subtotal · 5 measured · 1 unavailable")
            )
        );
        assert_eq!(
            split_reading("3 W · coverage unavailable"),
            ("3 W", Some("coverage unavailable"))
        );
        assert_eq!(split_reading("Unavailable"), ("Unavailable", None));
        assert_eq!(split_reading("12.5 W"), ("12.5 W", None));
    }
}
