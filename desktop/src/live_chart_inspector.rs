use std::rc::Rc;

use gpui::{
    Context, FocusHandle, ScrollAnchor, ScrollHandle, SharedString, Window, anchored, deferred,
    div, prelude::*, px, rgb,
};
use num_traits::ToPrimitive;

use crate::chart_axes::TimeWindow;
use crate::charts::{GRID, MUTED, PANEL};
use crate::dashboard::control;
use crate::data::{LineExtremes, MachinePlace, Panel};
use crate::live_dashboard::LiveDashboard;
use crate::live_presentation::clock;

pub(crate) struct ChartInspector {
    panel_id: String,
    summaries: Vec<SampleSummary>,
    /// What the whole of the chart's stack reached, where one of its lines names that.
    stack_total: Option<SampleMaximum>,
    /// What the keyboard scrolls, since GPUI brings no focused element into view itself.
    scroll: ScrollHandle,
    close_stop: ChartDataStop,
    /// The control that holds each row's maximum, the stack's total last.
    maximum_stops: Vec<ChartDataStop>,
}

/// One control of the dialog the keyboard reaches.
#[derive(Clone)]
struct ChartDataStop {
    focus: FocusHandle,
    anchor: ScrollAnchor,
}

struct SampleSummary {
    mean: Option<f32>,
    maximum: Option<SampleMaximum>,
    available: u32,
    total: usize,
}

/// The highest a row reached, and the reading that read it where the chart names its readings.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SampleMaximum {
    value: f32,
    place: Option<MachinePlace>,
}

/// The most lines a chart's table gives a row.
const CHART_LINES_MAX: usize = 16;
/// The most rows the table can give: a line's for each of its lines, then the stack's total.
const CHART_DATA_ROWS_MAX: usize = CHART_LINES_MAX + 1;
/// The column of a row that says when its maximum was read, and holds that reading.
const CHART_DATA_READ_AT_COLUMN: usize = 4;

impl LiveDashboard {
    pub(crate) fn open_chart_inspector(
        &mut self,
        panel: &Rc<Panel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scroll = ScrollHandle::new();
        let close_stop = ChartDataStop {
            focus: cx.focus_handle(),
            anchor: ScrollAnchor::for_handle(scroll.clone()),
        };
        window.focus(&close_stop.focus, cx);
        let view = self.view_window();
        self.chart_inspector = Some(ChartInspector {
            panel_id: panel.id.clone(),
            summaries: chart_summaries(panel, view),
            stack_total: chart_stack_total(panel, view),
            maximum_stops: (0..CHART_DATA_ROWS_MAX)
                .map(|_| ChartDataStop {
                    focus: cx.focus_handle(),
                    anchor: ScrollAnchor::for_handle(scroll.clone()),
                })
                .collect(),
            scroll,
            close_stop,
        });
        cx.notify();
    }

    pub(crate) fn refresh_chart_inspector(&mut self) {
        let view = self.view_window();
        let Some(inspector) = &mut self.chart_inspector else {
            return;
        };

        if let Some(panel) = chart_inspector_panel(&self.panels, &inspector.panel_id) {
            inspector.summaries = chart_summaries(panel, view);
            inspector.stack_total = chart_stack_total(panel, view);
        } else {
            self.chart_inspector = None;
        }
    }

    pub(crate) fn chart_inspector_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let inspector = self.chart_inspector.as_ref()?;
        let panel = chart_inspector_panel(&self.panels, &inspector.panel_id)?;
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
            .overflow_scroll()
            .track_scroll(&inspector.scroll)
            .p(px(20.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(Self::chart_inspector_heading(
                panel,
                &inspector.close_stop,
                cx,
            ))
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
                those seconds where the chart keeps it. A layer of a stack and an App keep only \
                their samples. They do not recover peaks between readings. Picking when a maximum \
                was read holds that reading on every page.",
            ))
    }

    fn chart_inspector_heading(
        panel: &Panel,
        close_stop: &ChartDataStop,
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
                    .track_focus(&close_stop.focus)
                    .anchor_scroll(Some(close_stop.anchor.clone()))
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
    /// within the dialog, and scrolls it to the dialog's top left corner, as far as there is table
    /// to scroll. Only a scroll container's direct children give their place, so a control already
    /// in view moves too.
    fn chart_inspector_tab(&self, backward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let stops = self.chart_inspector_stops();
        if stops.is_empty() {
            return;
        }
        let count = stops.len();
        let focused_index = stops
            .iter()
            .position(|stop| stop.focus.is_focused(window))
            .unwrap_or(0);
        let next_index = if backward {
            (focused_index + count - 1) % count
        } else {
            (focused_index + 1) % count
        };

        window.focus(&stops[next_index].focus, cx);
        stops[next_index].anchor.scroll_to(window, cx);
    }

    /// The controls of the dialog in the order the keyboard reaches them.
    fn chart_inspector_stops(&self) -> Vec<ChartDataStop> {
        let Some(inspector) = &self.chart_inspector else {
            return Vec::new();
        };
        let Some(panel) = chart_inspector_panel(&self.panels, &inspector.panel_id) else {
            return Vec::new();
        };

        let mut stops = vec![inspector.close_stop.clone()];
        for (line_index, summary) in inspector.summaries.iter().enumerate() {
            let held = summary
                .maximum
                .is_some_and(|maximum| maximum.place.is_some());
            if held && self.chart_series_visible(panel, line_index) {
                stops.push(inspector.maximum_stops[line_index].clone());
            }
        }
        let total = self.chart_inspector_total(panel, inspector);
        if total.is_some_and(|total| total.place.is_some()) {
            stops.push(inspector.maximum_stops[inspector.summaries.len()].clone());
        }

        stops
    }

    /// What the whole of `panel`'s stack reached, while every line of the stack is shown, since
    /// with one hidden the chart no longer stacks what the total adds up.
    fn chart_inspector_total(
        &self,
        panel: &Panel,
        inspector: &ChartInspector,
    ) -> Option<SampleMaximum> {
        let every_line_shown = panel
            .lines
            .iter()
            .take(CHART_LINES_MAX)
            .enumerate()
            .filter(|(_, line)| crate::charts::stacked(panel, line))
            .all(|(line_index, _)| self.chart_series_visible(panel, line_index));

        inspector.stack_total.filter(|_| every_line_shown)
    }

    /// Lets go of the dialog and holds the reading at `place` on every page. A hold stays in the
    /// window the charts show, and the table was drawn before this event, so a reading that window
    /// has since left holds nothing.
    pub(crate) fn chart_inspector_hold(
        &mut self,
        place: MachinePlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_chart_inspector(window, cx);
        let in_view = self
            .view_window()
            .is_some_and(|view| view.contains(place.second()));
        if !in_view {
            return;
        }

        self.pointer = crate::live_hover::controller::PointerPlace::AtHeldPick;
        self.pin_target(crate::live_hover::Target::of_place(&place), cx);
    }

    /// The control of the row `name` that says when its maximum was read and holds that reading.
    fn chart_inspector_maximum(
        panel: &Panel,
        name: &str,
        place: MachinePlace,
        stop: &ChartDataStop,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let read_at = crate::chart_axes::clock(place.second(), true);
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
            .track_focus(&stop.focus)
            .anchor_scroll(Some(stop.anchor.clone()))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if ["enter", "space"].contains(&event.keystroke.key.as_str()) {
                        this.chart_inspector_hold(place, window, cx);
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.chart_inspector_hold(place, window, cx);
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
        let lines = panel.lines.iter().take(CHART_LINES_MAX);
        for (line_index, line) in lines.enumerate() {
            if !self.chart_series_visible(panel, line_index) {
                continue;
            }

            visible_count += 1;
            let summary = &inspector.summaries[line_index];
            let place = summary.maximum.and_then(|maximum| maximum.place);
            let maximum = place.map(|place| {
                let stop = &inspector.maximum_stops[line_index];
                Self::chart_inspector_maximum(panel, &line.name, place, stop, cx)
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
        if let Some(total) = self.chart_inspector_total(panel, inspector) {
            let maximum = total.place.map(|place| {
                let stop = &inspector.maximum_stops[inspector.summaries.len()];
                Self::chart_inspector_maximum(panel, CHART_DATA_TOTAL, place, stop, cx)
            });
            table = table.child(chart_data_row(
                chart_data_total_cells(panel, total),
                maximum,
            ));
        }

        table.my(px(16.0))
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
        super::line_reading(panel, summary.maximum.map(|maximum| maximum.value), unit),
        chart_data_read_at(summary.maximum),
        format!("{} / {}", summary.available, summary.total),
    ]
}

/// What the Data table's row of a stack's whole says: how far it reached and when, since the rows
/// of its lines say what each read and a mean of the whole is nothing the chart draws.
fn chart_data_total_cells(panel: &Panel, total: SampleMaximum) -> [String; 6] {
    [
        CHART_DATA_TOTAL.into(),
        String::new(),
        String::new(),
        super::line_reading(panel, Some(total.value), &panel.unit),
        chart_data_read_at(Some(total)),
        String::new(),
    ]
}

fn chart_data_read_at(maximum: Option<SampleMaximum>) -> String {
    maximum
        .and_then(|maximum| maximum.place)
        .map_or_else(String::new, |place| {
            crate::chart_axes::clock(place.second(), true)
        })
}

/// A row of the table, whose column of when its maximum was read is `maximum` where it has one.
fn chart_data_row(
    values: [String; 6],
    mut maximum: Option<gpui::Stateful<gpui::Div>>,
) -> gpui::Div {
    let mut row = div()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(GRID))
        .py(px(9.0));
    for (column, value) in values.into_iter().enumerate() {
        let cell = div()
            .flex_1()
            .min_w_0()
            .px(px(8.0))
            .debug_selector(|| value.clone());
        let control = if column == CHART_DATA_READ_AT_COLUMN {
            maximum.take()
        } else {
            None
        };
        row = row.child(match control {
            Some(control) => cell.child(control),
            None => cell.child(value),
        });
    }

    row
}

/// What the table's row of a stack's whole names.
const CHART_DATA_TOTAL: &str = "Total";

/// The chart of `panels` a table with `panel_id` reads.
fn chart_inspector_panel<'panels>(
    panels: &'panels [Rc<Panel>],
    panel_id: &str,
) -> Option<&'panels Rc<Panel>> {
    panels.iter().take(16).find(|panel| panel.id == panel_id)
}

/// What each line of `panel` read on the chart. A chart keeps readings on either side of its
/// window, for its first point's mean and so that a narrowed range keeps each reading's index. Its
/// mean and its count keep to the readings the plot paints, and its maximum to the readings in
/// `view`, the window the charts show, since that is what a hold can name.
fn chart_summaries(panel: &Panel, view: Option<TimeWindow>) -> Vec<SampleSummary> {
    panel
        .lines
        .iter()
        .take(CHART_LINES_MAX)
        .map(|line| {
            assert!(line.values.len() <= crate::data::LIVE_SAMPLE_LIMIT);
            let mut sum = 0.0_f64;
            let mut maximum = None;
            let mut available = 0_u32;
            let mut total = 0_usize;
            let values = line.values.iter().take(crate::data::LIVE_SAMPLE_LIMIT);
            for (index, value) in values.enumerate() {
                let on_plot = chart_summaries_on_plot(panel, index);
                let value = value.filter(|value| on_plot && value.is_finite());
                if on_plot {
                    total += 1;
                }
                if let Some(value) = value {
                    sum += f64::from(value);
                    available += 1;
                }
                let reading = chart_summaries_reading(panel, line, index, value, view);
                maximum = chart_data_higher(maximum, reading);
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
                total,
            }
        })
        .collect()
}

/// Whether the plot of `panel` paints its reading at `index`. A chart with no clock paints them
/// all.
fn chart_summaries_on_plot(panel: &Panel, index: usize) -> bool {
    match &panel.positions {
        Some(positions) if panel.window.is_some() => positions
            .get(index)
            .is_some_and(|position| crate::time_range::on_plot(*position)),
        _ => true,
    }
}

/// How far the whole of `panel`'s stack reached in `view`, where a line of it names that: the
/// highest of what that line names and of what its lines add up to at each reading. No row of a
/// line can say it, since each keeps to its own readings.
fn chart_stack_total(panel: &Panel, view: Option<TimeWindow>) -> Option<SampleMaximum> {
    let lines: Vec<&crate::data::Line> = panel
        .lines
        .iter()
        .take(CHART_LINES_MAX)
        .filter(|line| crate::charts::stacked(panel, line))
        .collect();
    let extremes = lines.iter().find_map(|line| line.extremes.as_ref())?;
    let count = lines.iter().map(|line| line.values.len()).max()?;

    let mut maximum = None;
    for index in 0..count {
        let added = chart_stack_total_reading(&lines, index);
        let reading = chart_data_highest(panel, Some(extremes), index, added, view);
        maximum = chart_data_higher(maximum, reading);
    }

    maximum
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

/// The highest reading the sample `value` at `index` of `line` stands for. A stacked line's reach
/// is the stack's total and not its own.
fn chart_summaries_reading(
    panel: &Panel,
    line: &crate::data::Line,
    index: usize,
    value: Option<f32>,
    view: Option<TimeWindow>,
) -> Option<SampleMaximum> {
    let extremes = line
        .extremes
        .as_ref()
        .filter(|_| !crate::charts::stacked(panel, line));

    chart_data_highest(panel, extremes, index, value, view)
}

/// The higher of `value` and what `extremes` name at `index`, and the reading that read it, among
/// the readings in `view`. A reach can name a reading past the point that stands for it, so each
/// answers for its own place. A chart that names no readings has only its sample to go by.
fn chart_data_highest(
    panel: &Panel,
    extremes: Option<&LineExtremes>,
    index: usize,
    value: Option<f32>,
    view: Option<TimeWindow>,
) -> Option<SampleMaximum> {
    let in_view = |place: &MachinePlace| view.is_some_and(|view| view.contains(place.second()));
    let place = panel
        .places
        .as_ref()
        .and_then(|places| places.get(index).copied());
    let own = value
        .filter(|_| place.is_none_or(|place| in_view(&place)))
        .map(|value| SampleMaximum { value, place });
    let reach = extremes
        .and_then(|extremes| extremes.get(index).copied().flatten())
        .filter(|reach| in_view(&reach.peak))
        .map(|reach| SampleMaximum {
            value: reach.high,
            place: Some(reach.peak),
        });

    chart_data_higher(reach, own)
}

/// The higher of `highest` and `reading`, the earlier of two that tie.
fn chart_data_higher(
    highest: Option<SampleMaximum>,
    reading: Option<SampleMaximum>,
) -> Option<SampleMaximum> {
    match (highest, reading) {
        (Some(highest), Some(reading)) if reading.value > highest.value => Some(reading),
        (highest, reading) => highest.or(reading),
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, Window, div, prelude::*, px};

    use crate::chart_axes::TimeWindow;
    use crate::data::{LineReach, MachinePlace};

    use super::{
        SampleMaximum, SampleSummary, chart_data_cells, chart_stack_total, chart_summaries,
        split_reading, tooltip_row,
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

    /// A reading of no run, read at `second`.
    fn place_at(second: u64) -> MachinePlace {
        MachinePlace {
            wall_ns: second * 1_000_000_000,
            ..MachinePlace::default()
        }
    }

    /// A window that holds the wall seconds `reaching_panel` names.
    const REACHING_VIEW: Option<TimeWindow> = Some(TimeWindow {
        first: 8_900,
        span: 200,
    });

    fn highest(summary: &SampleSummary) -> Option<f32> {
        summary.maximum.map(|maximum| maximum.value)
    }

    fn read_at(maximum: Option<SampleMaximum>) -> Option<u64> {
        maximum
            .and_then(|maximum| maximum.place)
            .map(|place| place.second())
    }

    /// A panel of one line, its second sample said to reach 95 at wall second 9,000.
    fn reaching_panel(stack: bool) -> crate::data::Panel {
        let mut panel: crate::data::Panel = serde_json::from_value(serde_json::json!({
            "id": "cpu", "title": "CPU", "unit": "%", "kind": "line", "stack": stack,
            "lines": [{"name": "All cores", "color": "#5794f2", "values": [10.0, 20.0]}]}))
        .unwrap();
        panel.lines[0].extremes = Some(vec![
            None,
            Some(LineReach {
                high: 95.0,
                peak: place_at(9_000),
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
        let summaries = chart_summaries(&panel, REACHING_VIEW);

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
        let summaries = chart_summaries(&reaching_panel(false), REACHING_VIEW);

        assert_eq!(highest(&summaries[0]), Some(95.0));
        assert_eq!(read_at(summaries[0].maximum), Some(9_000));
        assert_eq!(summaries[0].mean, Some(15.0));
    }

    #[test]
    fn a_sample_that_ties_its_reach_holds_the_reading_the_reach_names() {
        // Goal: a reach names the very reading that reached it, so where a sample reads the same a
        // hold goes to the reading the reach names.
        // Method: a sample reading what its reach reached, each naming its own second.
        let mut panel = reaching_panel(false);
        panel.lines[0].values[1] = Some(95.0);
        panel.places = Some(vec![place_at(8_950), place_at(8_960)].into());

        let summaries = chart_summaries(&panel, REACHING_VIEW);

        assert_eq!(read_at(summaries[0].maximum), Some(9_000));
    }

    #[test]
    fn a_line_that_reads_its_highest_twice_holds_the_earlier_reading() {
        // Goal: of two readings that tie, the table gives the first it came to, so a table drawn
        // again over the same readings keeps naming the same second.
        // Method: a line reading the same highest at two seconds.
        let panel = clocked_panel(1000, &[Some(9.0), Some(9.0), Some(1.0)]);

        let summaries = chart_summaries(&panel, panel.window);

        assert_eq!(read_at(summaries[0].maximum), Some(1000));
    }

    #[test]
    fn a_chart_with_no_window_names_no_reading_to_hold() {
        // Goal: a hold keeps to the window the charts show, so with none there is nothing a maximum
        // could hold and no reach names one.
        // Method: a line whose reach would pass its samples, and no window.
        let summaries = chart_summaries(&reaching_panel(false), None);

        assert_eq!(highest(&summaries[0]), Some(20.0));
        assert_eq!(read_at(summaries[0].maximum), None);
    }

    #[test]
    fn a_reach_under_a_samples_own_reading_leaves_that_reading_the_maximum() {
        // Goal: a stretch's reach leaves out the reading a chart draws, so the higher of the two
        // is what the line reached, and picking it holds that reading rather than the reach's.
        // Method: a sample that reads past what its reach names, and read the summary.
        let mut panel = reaching_panel(false);
        panel.lines[0].values[1] = Some(99.0);
        panel.places = Some(vec![place_at(8_950), place_at(8_960)].into());

        let summaries = chart_summaries(&panel, REACHING_VIEW);

        assert_eq!(highest(&summaries[0]), Some(99.0));
        assert_eq!(read_at(summaries[0].maximum), Some(8_960));
    }

    #[test]
    fn a_sample_with_no_reading_of_its_own_still_gives_how_far_it_reaches() {
        // Goal: a stretch whose first second has no rate still reached a peak, so the table gives
        // that peak, and the mean keeps to the samples that have a value.
        // Method: the sample that reaches past the others with no value of its own.
        let mut panel = reaching_panel(false);
        panel.lines[0].values[1] = None;

        let summaries = chart_summaries(&panel, REACHING_VIEW);

        assert_eq!(highest(&summaries[0]), Some(95.0));
        assert_eq!(read_at(summaries[0].maximum), Some(9_000));
        assert_eq!(summaries[0].mean, Some(10.0));
    }

    #[test]
    fn a_stacked_lines_maximum_is_its_own_and_not_the_total_of_its_stack() {
        // Goal: a stack's top line names how far the stack reaches, which is no reading of that
        // line, so its row keeps to its own samples.
        // Method: the same line in a stack, and read the summary.
        let summaries = chart_summaries(&reaching_panel(true), REACHING_VIEW);

        assert_eq!(highest(&summaries[0]), Some(20.0));
        assert_eq!(read_at(summaries[0].maximum), None);
    }

    #[test]
    fn a_guide_line_of_a_stacked_chart_keeps_its_own_reach() {
        // Goal: a line drawn over a stack rather than in it, as installed memory is, reads for
        // itself.
        // Method: the same line taken out of the stack, and read the summary.
        let mut panel = reaching_panel(true);
        panel.lines[0].stack = Some(false);

        let summaries = chart_summaries(&panel, REACHING_VIEW);

        assert_eq!(highest(&summaries[0]), Some(95.0));
    }

    /// A chart of `values` a second apart from 1,000, across the 900 seconds from `first`, each
    /// naming its reading.
    fn clocked_panel(first: u64, values: &[Option<f32>]) -> crate::data::Panel {
        let mut panel: crate::data::Panel = serde_json::from_value(serde_json::json!({
            "id": "cpu", "title": "CPU", "unit": "%", "kind": "line",
            "lines": [{"name": "All cores", "color": "#5794f2", "values": values}]}))
        .unwrap();
        let window = TimeWindow { first, span: 900 };
        let seconds = (0..values.len()).map(|index| 1000 + u64::try_from(index).unwrap());
        panel.window = Some(window);
        panel.positions = Some(
            seconds
                .clone()
                .map(|second| {
                    let offset = f64::from(u32::try_from(second).unwrap())
                        - f64::from(u32::try_from(first).unwrap());
                    num_traits::ToPrimitive::to_f32(&((offset + 0.5) / 900.0)).unwrap()
                })
                .collect(),
        );
        panel.places = Some(seconds.map(place_at).collect());
        panel
    }

    #[test]
    fn a_peak_of_one_second_is_its_lines_maximum_at_every_offset() {
        // Goal: a spike one second long is its line's maximum in the table at whatever second of
        // a minute it falls.
        // Method: a steady line with one peak at each second of a minute, and read each summary.
        for peak in 0..60_usize {
            let mut values = vec![Some(10.0); 900];
            values[peak] = Some(95.0);
            let panel = clocked_panel(1000, &values);

            let summaries = chart_summaries(&panel, panel.window);

            assert_eq!(highest(&summaries[0]), Some(95.0), "peak {peak}");
            assert_eq!(
                read_at(summaries[0].maximum),
                Some(1000 + u64::try_from(peak).unwrap()),
                "peak {peak}"
            );
        }
    }

    #[test]
    fn a_panel_that_names_no_readings_gives_a_maximum_and_nothing_to_hold() {
        // Goal: a hold names a reading, so a chart that names none says what it reached and offers
        // no control.
        // Method: a chart with a clock and no places, and read the summary.
        let mut panel = clocked_panel(1000, &[Some(1.0), Some(9.0), Some(2.0)]);
        panel.places = None;

        let summaries = chart_summaries(&panel, panel.window);

        assert_eq!(highest(&summaries[0]), Some(9.0));
        assert_eq!(read_at(summaries[0].maximum), None);
    }

    #[test]
    fn a_reading_before_the_window_counts_in_no_summary() {
        // Goal: the table says it reads the samples in the visible chart, and a chart holds
        // readings before its window for its first point's mean, so a peak there is no maximum
        // to hold.
        // Method: a window that starts two seconds after a higher reading, and read the summary.
        let panel = clocked_panel(1002, &[Some(95.0), Some(5.0), Some(7.0)]);

        let summaries = chart_summaries(&panel, panel.window);

        assert_eq!(highest(&summaries[0]), Some(7.0));
        assert_eq!(read_at(summaries[0].maximum), Some(1002));
        assert_eq!(summaries[0].mean, Some(6.0));
        assert_eq!(summaries[0].available, 2);
        assert_eq!(summaries[0].total, 2);
    }

    #[test]
    fn a_reading_after_the_window_counts_in_no_summary() {
        // Goal: a narrowed range keeps the readings past it, and narrowing is how a second spike
        // is found, so a higher reading past the window must not hide the one inside it.
        // Method: a window whose last second is 1,000, a reading the plot paints a hair beyond it
        // and one further on, and read the summary.
        let panel = clocked_panel(101, &[Some(7.0), Some(95.0), Some(96.0)]);

        let summaries = chart_summaries(&panel, panel.window);

        assert_eq!(highest(&summaries[0]), Some(7.0));
        assert_eq!(read_at(summaries[0].maximum), Some(1000));
        assert_eq!(summaries[0].mean, Some(51.0));
        assert_eq!(summaries[0].available, 2);
        assert_eq!(summaries[0].total, 2);
    }

    #[test]
    fn a_reading_a_hair_before_the_window_is_no_maximum_to_hold() {
        // Goal: the plot paints a reading a hair before its window, and a hold on it would move
        // the charts, which keep the window they showed when the hold began.
        // Method: a higher reading in the second before the window, and read the summary.
        let panel = clocked_panel(1001, &[Some(9.0), Some(5.0)]);

        let summaries = chart_summaries(&panel, panel.window);

        assert_eq!(highest(&summaries[0]), Some(5.0));
        assert_eq!(read_at(summaries[0].maximum), Some(1001));
        assert_eq!(summaries[0].mean, Some(7.0));
    }

    #[test]
    fn a_reach_that_names_a_reading_past_the_window_is_no_maximum() {
        // Goal: a stretch can start inside the window and peak after it, and a hold there would
        // leave the window the charts show.
        // Method: a point inside the window whose reach names a reading past its last second.
        let mut panel = clocked_panel(1000, &[Some(10.0), Some(20.0)]);
        panel.lines[0].extremes = Some(vec![
            Some(LineReach {
                high: 95.0,
                peak: place_at(2_000),
            }),
            None,
        ]);

        let summaries = chart_summaries(&panel, panel.window);

        assert_eq!(highest(&summaries[0]), Some(20.0));
        assert_eq!(read_at(summaries[0].maximum), Some(1001));
    }

    #[test]
    fn a_reach_that_began_before_the_window_gives_its_peak_inside_it() {
        // Goal: a stretch stands where its first second is, so a window that starts inside it
        // leaves its point off the plot and still holds its peak.
        // Method: a point before the window whose reach names a reading inside it.
        let mut panel = clocked_panel(1003, &[Some(10.0), Some(10.0), Some(10.0), Some(12.0)]);
        panel.lines[0].extremes = Some(vec![
            Some(LineReach {
                high: 95.0,
                peak: place_at(1_005),
            }),
            None,
            None,
            None,
        ]);

        let summaries = chart_summaries(&panel, panel.window);

        assert_eq!(highest(&summaries[0]), Some(95.0));
        assert_eq!(read_at(summaries[0].maximum), Some(1_005));
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
        // give.
        // Method: a stack whose top line names a reach past what its lines add up to, and read the
        // total.
        let total = chart_stack_total(&stacked_panel(), REACHING_VIEW);

        assert_eq!(total.map(|total| total.value), Some(95.0));
        assert_eq!(read_at(total), Some(9_000));
    }

    #[test]
    fn a_stacks_total_adds_up_its_lines_where_they_read_past_what_it_names() {
        // Goal: a stretch's reach leaves out the reading a chart draws, so the higher of the two is
        // what the stack reached.
        // Method: a stack whose lines add up past the reach it names, and read the total.
        let mut panel = stacked_panel();
        panel.lines[0].values = vec![Some(100.0), Some(20.0)];

        let total = chart_stack_total(&panel, REACHING_VIEW);

        assert_eq!(total.map(|total| total.value), Some(105.0));
    }

    #[test]
    fn a_stacks_total_adds_up_one_reading_and_never_separate_maxima() {
        // Goal: lines that peak at different moments never stacked that high, so the total is the
        // highest any one reading added up to.
        // Method: two lines that peak at different readings, and read the total.
        let mut panel = stacked_panel();
        panel.lines[0].values = vec![Some(10.0), Some(20.0)];
        panel.lines[1].values = vec![Some(15.0), Some(1.0)];
        panel.lines[1].extremes = Some(vec![None, None]);

        let total = chart_stack_total(&panel, REACHING_VIEW);

        assert_eq!(total.map(|total| total.value), Some(25.0));
    }

    #[test]
    fn a_stacks_total_adds_up_no_reading_a_line_of_it_missed() {
        // Goal: a sum that left a line out would read a total the stack never read.
        // Method: a stack whose larger sum lacks one of its lines, and read the total.
        let mut panel = stacked_panel();
        panel.lines[0].values = vec![None, Some(20.0)];
        panel.lines[1].values = vec![Some(50.0), Some(1.0)];
        panel.lines[1].extremes = Some(vec![None, None]);

        let total = chart_stack_total(&panel, REACHING_VIEW);

        assert_eq!(total.map(|total| total.value), Some(21.0));
    }

    #[test]
    fn a_guide_line_adds_nothing_to_its_stacks_total() {
        // Goal: installed memory is drawn over the stack and is no layer of it, so it is no part of
        // what the stack reached.
        // Method: a stack with a far higher line taken out of it, and read the total.
        let mut panel = stacked_panel();
        let mut guide = panel.lines[0].clone();
        guide.name = "Guide".into();
        guide.stack = Some(false);
        guide.values = vec![Some(1000.0), Some(1000.0)];
        panel.lines.push(guide);

        let total = chart_stack_total(&panel, REACHING_VIEW);

        assert_eq!(total.map(|total| total.value), Some(95.0));
    }

    #[test]
    fn a_stacks_total_keeps_to_the_window() {
        // Goal: the total says what the chart in view reached, as each line's row does.
        // Method: readings off the plot, a hair before the window and in it, the first two adding
        // up to more, and read the total.
        let mut panel = clocked_panel(1002, &[Some(90.0), Some(2.0), Some(3.0)]);
        let mut top = panel.lines[0].clone();
        top.name = "Top".into();
        top.values = vec![Some(90.0), Some(50.0), Some(1.0)];
        top.extremes = Some(vec![None, None, None]);
        panel.lines.push(top);
        panel.stack = Some(true);

        let total = chart_stack_total(&panel, panel.window);

        assert_eq!(total.map(|total| total.value), Some(4.0));
        assert_eq!(read_at(total), Some(1002));
    }

    #[test]
    fn a_chart_without_a_stack_or_a_named_reach_has_no_total() {
        // Goal: only a stack that says how far it reached has a total to give.
        // Method: a chart of lines on their own, and a stack that names no reach.
        let mut stack = stacked_panel();
        stack.lines[1].extremes = None;

        assert_eq!(
            chart_stack_total(&reaching_panel(false), REACHING_VIEW),
            None
        );
        assert_eq!(chart_stack_total(&stack, REACHING_VIEW), None);
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
