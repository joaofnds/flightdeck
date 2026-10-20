use std::cell::Cell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    Bounds, Context, IntoElement, Pixels, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use num_traits::ToPrimitive;

use crate::chart_axes::{AxisSide, TimeWindow, axis_column, elapsed_labels, labels};
use crate::charts::{self, BLUE, GRID, Inspection, LINE, MUTED, ORANGE, PANEL, RED, TEXT};

use crate::live_chart_view::ChartHelp;
use crate::live_dashboard::LiveDashboard;
use crate::live_projection::fixed_text;
use crate::live_sections::Section;
use crate::live_views::summary::Card;
use crate::self_monitoring::{self, Chart};

/// The parts of the Self page, each one row of the page's list, in the prototype's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelfPart {
    Overview,
    Charts,
    Sources,
    Seals,
    Signals,
    Reports,
    Minutes,
}

impl SelfPart {
    pub const ALL: [Self; 7] = [
        Self::Overview,
        Self::Charts,
        Self::Sources,
        Self::Seals,
        Self::Signals,
        Self::Reports,
        Self::Minutes,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Overview => "self-overview",
            Self::Charts => "self-charts",
            Self::Sources => "self-sources",
            Self::Seals => "self-seals",
            Self::Signals => "self-signals",
            Self::Reports => "self-reports",
            Self::Minutes => "self-minutes",
        }
    }
}

/// The prototype's Self-monitoring plot: 180 px of plot, 54 px of value labels to its left and
/// 12 px of margin right of it, over 30 px of elapsed-time labels.
const CHART_HEIGHT: f32 = 180.0;
const PLOT_LEFT: f32 = 54.0;
const PLOT_RIGHT: f32 = 12.0;
const TIME_LABELS_HEIGHT: f32 = 30.0;
const TIME_LABELS_DROP: f32 = 4.0;

/// The page background, which the pipeline's stages sit on.
const BACKGROUND: u32 = 0x0011_1217;

/// The shade of the prototype's table headings.
const RAISED: u32 = 0x002c_2f35;

const PIPELINE: [(&str, &str); 4] = [
    (
        "Collector",
        "macOS readings and process events. Scheduled once a second; measures source CPU cost.",
    ),
    (
        "Shared-memory ring",
        "120 tick slots plus a separate event log. One writer; readers can fall behind and lose \
         overwritten records.",
    ),
    (
        "Persister",
        "Separate process reads the ring, encodes bounded windows and commits them. Collection \
         continues if it exits.",
    ),
    (
        "Sealed recording",
        "Immutable Parquet windows of 60 elapsed seconds. Manifests retain availability, losses \
         and clock bounds.",
    ),
];

const SIGNALS: [(&str, &str); 5] = [
    (
        "Collector memory · Not captured",
        "Collector resident memory and physical footprint were not logged. The persister \
         footprint chart does not include the collector.",
    ),
    (
        "Ring reader lag · Not captured",
        "Latest/consumed publication and event cursors, oldest retained record, and overwrite \
         distance. Ring capacity is not queue occupancy.",
    ),
    (
        "Recording freshness · Not captured",
        "Last durable observation, time since last seal, unknown-extent losses, disk bytes/free \
         space and write failures.",
    ),
    (
        "Query performance · Not captured",
        "Selected-second query latency, result coverage, decoded bytes, failures and query \
         memory. The query API exists; this run did not exercise it.",
    ),
    (
        "Native viewer · Not captured",
        "Viewer CPU/footprint, time to display a selected second, rendering cost and live \
         ring-reader lag. The collector's reports do not carry them, so none is a measured zero.",
    ),
];

impl LiveDashboard {
    pub(crate) fn self_part(&self, part: SelfPart, cx: &mut Context<Self>) -> gpui::AnyElement {
        match part {
            SelfPart::Overview => self.self_overview().into_any_element(),
            SelfPart::Charts => self.self_charts(cx),
            SelfPart::Sources => self.self_sources().into_any_element(),
            SelfPart::Seals => self.self_seals().into_any_element(),
            SelfPart::Signals => self_signals().into_any_element(),
            SelfPart::Reports => self.self_reports_table(cx).into_any_element(),
            SelfPart::Minutes => self.self_minutes_table(cx).into_any_element(),
        }
    }

    fn self_overview(&self) -> impl IntoElement {
        let status = self.self_reports.started.map_or_else(
            || "This app did not start the collector, so its reports are not captured.".into(),
            |started| {
                let elapsed = wall_second().map_or_else(
                    || "Unavailable".into(),
                    |now| self_monitoring::elapsed(started, now),
                );
                self_monitoring::run_status(&elapsed)
            },
        );
        let columns = if self.viewport_size.width < px(640.0) {
            2
        } else {
            4
        };

        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .debug_selector(|| format!("self-status:{status}"))
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .text_color(rgb(MUTED))
                    .child(status.clone()),
            )
            .child(self_notice())
            .child(
                div()
                    .grid()
                    .grid_cols(columns)
                    .gap(px(8.0))
                    .children(self.summary_cards().iter().map(self_card)),
            )
            .child(self_pipeline())
    }

    fn self_charts(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let heading = crate::live_views::heading("Collection and overhead");
        let Some(window) = self_window(self.self_reports.started) else {
            return div()
                .child(heading)
                .child(caption(
                    "The wall clock is unavailable, so no report can be placed.",
                ))
                .into_any_element();
        };
        let reports = &self.self_reports;
        let inspected = self_monitoring::inspected(reports, self.self_inspected);
        let second = inspected.map(|tick| tick.second);
        let started = reports.started.unwrap_or(window.first);
        let span = self_monitoring::elapsed(window.first, window.first + window.span);
        let (run, report) = match second {
            Some(second) => (
                format!("Run 0:00–{span} · inspecting report ending"),
                self_monitoring::elapsed(started, second),
            ),
            None => (
                format!("Run 0:00–{span} · no ten-second report yet"),
                String::new(),
            ),
        };
        let cursor = second.map(|second| fraction(window, second));
        let columns = if self.viewport_size.width < px(640.0) {
            1
        } else {
            2
        };
        let mut grid = div().grid().grid_cols(columns).gap(px(8.0));
        for chart in self_monitoring::charts(reports, window, second) {
            grid = grid.child(Self::self_chart(&chart, window, cursor, cx));
        }

        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(heading)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb(px(2.0))
                    .child(
                        div()
                            .debug_selector(|| format!("self-run:{run} {report}"))
                            .flex()
                            .gap(px(3.0))
                            .text_size(px(12.0))
                            .text_color(rgb(MUTED))
                            .child(run.clone())
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(report.clone()),
                            ),
                    )
                    .child(Self::latest_report_button(cx)),
            )
            .child(grid)
            .into_any_element()
    }

    /// Returns the inspection to the latest report, as tall as the prototype's buttons are.
    fn latest_report_button(cx: &mut Context<Self>) -> impl IntoElement {
        crate::dashboard::control("Latest report")
            .debug_selector(|| "self-latest-report".into())
            .flex()
            .items_center()
            .min_h(px(44.0))
            .p(px(8.0))
            .border_1()
            .border_color(rgb(LINE))
            .bg(gpui::transparent_black())
            .text_size(px(12.0))
            .on_click(cx.listener(|this, _, _, cx| {
                this.self_inspected = None;
                cx.notify();
            }))
    }

    fn self_chart(
        chart: &Chart,
        window: TimeWindow,
        cursor: Option<f32>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut axis = labels(self_monitoring::axis(chart));
        axis.width = px(PLOT_LEFT);
        let gutter = axis.width;
        let id = chart.panel.id.clone();
        let title = chart.panel.title.clone();
        let caption_text = chart.panel.caption.clone().unwrap_or_default();
        let help = chart.panel.help.clone().unwrap_or_default();
        let plot = Self::self_plot(chart, window, cursor, cx);

        div()
            .debug_selector(|| format!("chart-card:{id}"))
            .min_w_0()
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(PANEL))
            .px(px(12.0))
            .py(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .min_h(px(44.0))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        crate::live_chart_view::chart_head_button(format!("about-{id}"), "ⓘ")
                            .px(px(8.0))
                            .tooltip(move |_, cx| cx.new(|_| ChartHelp(help.clone())).into()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .child(axis_column(axis, px(CHART_HEIGHT), AxisSide::Leading))
                    .child(div().flex_1().min_w_0().child(plot)),
            )
            .child(
                div()
                    .pl(gutter)
                    .pt(px(TIME_LABELS_DROP))
                    .h(px(TIME_LABELS_HEIGHT))
                    .child(elapsed_labels(
                        window.span,
                        charts::PLOT_INSETS.left,
                        px(PLOT_RIGHT),
                    )),
            )
            .child(self_legend(chart))
            .child(caption(&caption_text))
    }

    fn self_plot(
        chart: &Chart,
        window: TimeWindow,
        cursor: Option<f32>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let measured = Rc::new(Cell::new(Bounds::default()));
        let points = chart.panel.positions.as_ref().map_or(0, Vec::len);
        let plot = (points > 0).then(|| {
            let upper = points - 1;
            charts::plot_without_cursor(
                Rc::clone(&chart.panel),
                Inspection {
                    lower: 0,
                    selected: upper,
                    upper,
                },
                Rc::new(Vec::new()),
                Rc::new(Cell::new(Bounds::default())),
                None,
            )
        });
        let budget = chart.budget;
        let hovered = Rc::clone(&measured);

        div()
            .id(gpui::SharedString::from(format!("{}-plot", chart.panel.id)))
            .debug_selector(|| format!("self-plot:{}", chart.panel.id))
            .relative()
            .h(px(CHART_HEIGHT))
            .mr(px(PLOT_RIGHT) - charts::PLOT_INSETS.right)
            .focusable()
            .tab_stop(true)
            .focus(crate::charts::focus_ring)
            .child(quarter_grid(measured))
            .children(plot)
            .child(overlay(budget, cursor))
            .children(budget.map(budget_label))
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    // Every plot hears every move, so each answers only for its own area.
                    if !hovered.get().contains(&event.position) {
                        return;
                    }
                    let rect = charts::plot_bounds(hovered.get());
                    let offset = f32::from(event.position.x - rect.origin.x);
                    let width = f32::from(rect.size.width).max(1.0);
                    this.self_hover(window, (offset / width).clamp(0.0, 1.0), cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let step = match event.keystroke.key.as_str() {
                    "left" => -1,
                    "right" => 1,
                    "home" => i64::MIN,
                    "end" => i64::MAX,
                    _ => return,
                };
                this.self_step(step, cx);
                window.prevent_default();
                cx.stop_propagation();
            }))
    }

    fn self_hover(&mut self, window: TimeWindow, fraction: f32, cx: &mut Context<Self>) {
        let span = window.span.to_f32().unwrap_or(0.0);
        let offset = (fraction * span).floor().to_u64().unwrap_or(0);
        let second = window.first.saturating_add(offset);
        let nearest = self_monitoring::nearest(&self.self_reports, second);
        if nearest.is_some() {
            if nearest != self.self_inspected {
                self.self_inspected = nearest;
                cx.notify();
            }
        }
    }

    /// Moves the inspection `step` reports along, stopping at the first and the last.
    fn self_step(&mut self, step: i64, cx: &mut Context<Self>) {
        let ticks = &self.self_reports.ticks;
        let Some(last) = ticks.len().checked_sub(1) else {
            return;
        };
        let current = self
            .self_inspected
            .and_then(|second| ticks.iter().position(|tick| tick.second == second))
            .unwrap_or(last);
        let current = i64::try_from(current).unwrap_or(i64::MAX);
        let target = current
            .saturating_add(step)
            .clamp(0, i64::try_from(last).unwrap_or(0));
        let Ok(index) = usize::try_from(target) else {
            return;
        };

        let second = ticks.get(index).map(|tick| tick.second);
        if second != self.self_inspected {
            self.self_inspected = second;
            cx.notify();
        }
    }

    fn self_sources(&self) -> impl IntoElement {
        let inspected = self_monitoring::inspected(&self.self_reports, self.self_inspected);
        let sources = inspected.map(self_monitoring::sources).unwrap_or_default();
        let largest = sources.first().map_or(0.0, |(_, value)| *value);
        let mut bars = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .py(px(8.0))
            .text_size(px(12.0))
            .line_height(px(18.0));
        for (name, value) in sources.iter().take(32) {
            bars = bars.child(source_bar(name, *value, largest));
        }
        if sources.is_empty() {
            bars = bars.child(
                div()
                    .text_color(rgb(MUTED))
                    .child("No ten-second report yet."),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(crate::live_views::heading("Where collector CPU goes"))
            .child(
                self_panel("CPU time by collection source")
                    .child(bars)
                    .child(caption(
                        "Milliseconds of CPU per elapsed second. Source values are rounded \
                     independently; unattributed covers remaining collector work. This is CPU \
                     cost, not sensor-read wall time.",
                    )),
            )
    }

    fn self_seals(&self) -> impl IntoElement {
        let rows = self_monitoring::seal_rows(&self.self_reports);
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(crate::live_views::heading("Recording integrity"))
            .child(
                self_panel("Sealed windows")
                    .child(caption(
                        "Completed seals, including the stop/drain window. Publication ranges \
                         are half-open. Seal time includes encoding and commit; staged bytes are \
                         uncompressed in-memory window data, not disk bytes.",
                    ))
                    .child(table(
                        "seals",
                        &self_monitoring::SEAL_HEADINGS,
                        &rows,
                        "No window sealed yet.",
                    )),
            )
    }

    fn self_reports_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let section = Section::SelfReports;
        let expanded = self.sections.expanded(section);
        let rows = if expanded {
            self_monitoring::report_rows(&self.self_reports)
        } else {
            Vec::new()
        };
        disclosure_panel()
            .child(
                self.disclosure(section, cx)
                    .text_size(px(12.0))
                    .text_color(rgb(MUTED)),
            )
            .when(expanded, |panel| {
                panel
                    .child(table(
                        "reports",
                        &self_monitoring::REPORT_HEADINGS,
                        &rows,
                        "No ten-second report yet.",
                    ))
                    .child(caption(
                        "Collector power is its reported process energy divided by the report \
                         interval, in mW. It excludes the persister, viewer and whole-machine \
                         power.",
                    ))
            })
    }

    fn self_minutes_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let section = Section::SelfMinutes;
        let expanded = self.sections.expanded(section);
        let rows = if expanded {
            self_monitoring::minute_rows(&self.self_reports)
        } else {
            Vec::new()
        };
        disclosure_panel()
            .child(
                self.disclosure(section, cx)
                    .text_size(px(12.0))
                    .text_color(rgb(MUTED)),
            )
            .when(expanded, |panel| {
                panel
                    .child(table(
                        "minutes",
                        &self_monitoring::MINUTE_HEADINGS,
                        &rows,
                        "No complete minute yet.",
                    ))
                    .child(caption(
                        "Read from the output of the collector this app started, as it runs. \
                         Report ends are the run's elapsed time at each report; minute \
                         CPU calculations use each reported elapsed_ns. No values are \
                         interpolated. Source report values are retained; displayed precision \
                         follows each metric.",
                    ))
            })
    }
}

fn self_card(card: &Card) -> impl IntoElement {
    let selector = format!("summary-card:{}:{}", card.label, card.reading);
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .min_w_0()
        .px(px(16.0))
        .py(px(14.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(rgb(GRID))
        .bg(rgb(PANEL))
        .child(
            div()
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(rgb(MUTED))
                .child(card.label),
        )
        .child(
            div()
                .text_size(px(23.0))
                .line_height(px(36.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgb(card.color))
                .child(card.reading.clone()),
        )
        .child(
            div()
                .text_size(px(11.0))
                .line_height(px(16.5))
                .text_color(rgb(MUTED))
                .child(card.note.clone()),
        )
}

/// The run-end estimate of processes the collector missed. The collector reports none, so the
/// notice says it is not captured where the prototype gives the count.
fn self_notice() -> impl IntoElement {
    div()
        .debug_selector(|| "self-notice".into())
        .flex()
        .overflow_hidden()
        .rounded(px(6.0))
        .border_t_1()
        .border_r_1()
        .border_b_1()
        .border_color(rgb(LINE))
        .child(div().w(px(3.0)).flex_shrink_0().bg(rgb(ORANGE)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .px(px(16.0))
                .py(px(12.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .line_height(px(19.5))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("Processes missed during this run · Not captured"),
                )
                .child(
                    paragraph(
                        "The collector reports no run-end PID-sequence estimate. Query and \
                         viewer health were not captured.",
                    )
                    .mt(px(4.0)),
                ),
        )
}

fn self_pipeline() -> impl IntoElement {
    let mut stages = div().flex().gap(px(24.0)).my(px(12.0));
    for (index, (title, text)) in PIPELINE.iter().enumerate() {
        stages = stages.child(
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .p(px(12.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(BACKGROUND))
                .child(
                    div()
                        .mb(px(5.0))
                        .text_size(px(14.0))
                        .line_height(px(21.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(*title),
                )
                .child(paragraph(text))
                .when(index + 1 < PIPELINE.len(), |stage| {
                    stage.child(
                        div()
                            .absolute()
                            .right(px(-20.0))
                            .top(gpui::relative(0.5))
                            .text_color(rgb(MUTED))
                            .child("→"),
                    )
                }),
        );
    }

    self_panel("Flightdeck recording pipeline")
        .child(stages)
        .child(
            paragraph(
                "Sealed recording → disk query API (implemented, not sampled here) → native \
                 viewer. Live viewing also reads the ring. Neither path uses a metrics server or \
                 a dashboard service.",
            )
            .py(px(12.0))
            .border_t_1()
            .border_color(rgb(LINE)),
        )
}

fn self_signals() -> impl IntoElement {
    let mut signals = div().grid().grid_cols(2).gap(px(12.0)).my(px(12.0));
    for (title, text) in SIGNALS {
        signals = signals.child(
            div()
                .debug_selector(move || format!("self-signal:{title}"))
                .pb(px(12.0))
                .border_b_1()
                .border_color(rgb(LINE))
                .child(
                    div()
                        .text_size(px(13.0))
                        .line_height(px(19.5))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(title),
                )
                .child(paragraph(text).mt(px(4.0))),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(crate::live_views::heading("Signals still to connect"))
        .child(self_panel_plain().child(signals))
}

fn self_panel(title: &'static str) -> gpui::Div {
    self_panel_plain().child(
        div().flex().items_center().min_h(px(40.0)).child(
            div()
                .debug_selector(move || title.into())
                .text_size(px(14.0))
                .line_height(px(21.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        ),
    )
}

fn self_panel_plain() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(PANEL))
}

fn disclosure_panel() -> gpui::Div {
    self_panel_plain()
}

/// A caption under a chart or table, as the prototype sets it.
fn caption(text: &str) -> gpui::Div {
    div()
        .mt(px(7.0))
        .text_size(px(11.0))
        .line_height(px(17.0))
        .text_color(rgb(MUTED))
        .child(text.to_owned())
}

/// A paragraph of a panel's body text.
fn paragraph(text: &str) -> gpui::Div {
    div()
        .text_size(px(12.0))
        .line_height(px(18.0))
        .text_color(rgb(MUTED))
        .child(text.to_owned())
}

fn source_bar(name: &str, value: f32, largest: f32) -> impl IntoElement {
    let share = if largest > 0.0 {
        (value / largest).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let reading = fixed_text(value, 2);
    let selector = format!("self-source:{name}:{reading}");

    div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(div().w(px(112.0)).flex_shrink_0().child(name.to_owned()))
        .child(
            div().flex_1().min_w_0().h(px(6.0)).child(
                div()
                    .h_full()
                    .w(gpui::relative(share))
                    .rounded(px(2.0))
                    .bg(rgb(BLUE)),
            ),
        )
        .child(
            div()
                .w(px(60.0))
                .flex_shrink_0()
                .text_right()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(reading),
        )
}

/// The weight of each column: the width of its longest text plus the cell padding, so the
/// columns share the table's width as the prototype's auto-sized columns do.
fn column_weights(headings: &[&str], rows: &[Vec<String>]) -> Vec<f32> {
    headings
        .iter()
        .enumerate()
        .map(|(index, heading)| {
            let longest = rows
                .iter()
                .filter_map(|row| row.get(index))
                .map(|text| text.chars().count())
                .max()
                .unwrap_or(0);
            let heading = heading.chars().count().to_f32().unwrap_or(0.0) * 6.0;
            let cells = longest.to_f32().unwrap_or(0.0) * 6.6;
            heading.max(cells) + 28.0
        })
        .collect()
}

fn table(id: &str, headings: &[&str], rows: &[Vec<String>], empty: &str) -> gpui::Div {
    let weights = column_weights(headings, rows);
    let mut table = div().flex().flex_col().text_size(px(12.0)).child(
        div()
            .flex()
            .py(px(10.0))
            .border_b_1()
            .border_color(rgb(GRID))
            .bg(rgb(RAISED))
            .text_size(px(11.0))
            .line_height(px(16.5))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(MUTED))
            .children(
                headings
                    .iter()
                    .enumerate()
                    .map(|(index, heading)| cell(index, heading, weights[index])),
            ),
    );
    let shown = rows.len().min(128);
    for (index, row) in rows.iter().take(shown).enumerate() {
        let selector = format!("self-{id}:{}", row.join("|"));
        table = table.child(
            div()
                .debug_selector(move || selector.clone())
                .flex()
                .py(px(10.0))
                .line_height(px(18.0))
                .when(index + 1 < shown, |row| {
                    row.border_b_1().border_color(rgb(GRID))
                })
                .children(
                    row.iter()
                        .enumerate()
                        .map(|(index, value)| cell(index, value, weights[index])),
                ),
        );
    }
    if rows.is_empty() {
        table = table.child(
            div()
                .py(px(6.0))
                .px(px(14.0))
                .text_color(rgb(MUTED))
                .child(empty.to_owned()),
        );
    }
    div()
        .overflow_hidden()
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(PANEL))
        .rounded(px(7.0))
        .child(table)
}

fn cell(index: usize, text: &str, weight: f32) -> gpui::Div {
    let cell = div()
        .flex_basis(px(weight))
        .flex_grow(weight)
        .min_w_0()
        .px(px(14.0))
        .whitespace_nowrap()
        .overflow_hidden()
        .child(text.to_owned());
    if index == 0 { cell } else { cell.text_right() }
}

fn self_legend(chart: &Chart) -> impl IntoElement {
    let id = chart.panel.id.clone();
    div()
        .flex()
        .flex_wrap()
        .gap_x(px(16.0))
        .gap_y(px(8.0))
        .my(px(8.0))
        .min_h(px(24.0))
        .text_size(px(12.0))
        .line_height(px(18.0))
        .children(chart.legend.iter().map(|legend| {
            let selector = format!("chart-legend:{id}:{}:{}", legend.name, legend.reading);
            div()
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(12.0))
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(rgb(legend.color)),
                )
                .child(legend.name)
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(legend.reading.clone()),
                )
        }))
}

/// The four gridlines that split the value axis in quarters, as the prototype draws them.
fn quarter_grid(measured: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(
        move |bounds, _, _| measured.set(bounds),
        |bounds, (), window, _| {
            let plot = charts::plot_bounds(bounds);
            for quarter in 0..=4_u8 {
                let y = plot.bottom() - plot.size.height * (f32::from(quarter) / 4.0);
                let color = if quarter == 0 {
                    charts::alpha(TEXT, 0.12)
                } else {
                    rgb(GRID)
                };
                window.paint_quad(fill(
                    Bounds::new(point(plot.origin.x, y), size(plot.size.width, px(1.0))),
                    color,
                ));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The dashed budget line and the cursor at the inspected report.
fn overlay(budget: Option<f32>, cursor: Option<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let plot = charts::plot_bounds(bounds);
            if let Some(budget) = budget {
                let y = plot.bottom() - plot.size.height * budget;
                for dash in 0..256_u16 {
                    let x = plot.origin.x + px(f32::from(dash) * 8.0);
                    if x >= plot.right() {
                        break;
                    }
                    let width = (plot.right() - x).min(px(5.0));
                    window.paint_quad(fill(
                        Bounds::new(point(x, y), size(width, px(1.0))),
                        rgb(RED),
                    ));
                }
            }
            if let Some(cursor) = cursor {
                let x = plot.origin.x + plot.size.width * cursor;
                window.paint_quad(fill(
                    Bounds::new(point(x, plot.origin.y), size(px(1.0), plot.size.height)),
                    charts::cursor_color(),
                ));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn budget_label(budget: f32) -> impl IntoElement {
    let plot =
        CHART_HEIGHT - f32::from(charts::PLOT_INSETS.top) - f32::from(charts::PLOT_INSETS.bottom);
    let top = f32::from(charts::PLOT_INSETS.top) + plot * (1.0 - budget) - 17.0;
    div()
        .debug_selector(|| "self-budget-label".into())
        .absolute()
        .top(px(top.max(0.0)))
        .right(px(8.0))
        .text_size(px(12.0))
        .line_height(px(15.0))
        .text_color(rgb(TEXT))
        .child("5% budget")
}

/// The run so far, from the second the collector started to the current wall second, at least
/// ten seconds so the first report has a place on the axis.
fn self_window(started: Option<u64>) -> Option<TimeWindow> {
    let now = wall_second()?;
    let first = started.map_or(now, |started| started.min(now));
    Some(TimeWindow {
        first,
        span: (now - first).max(10),
    })
}

fn fraction(window: TimeWindow, second: u64) -> f32 {
    let offset = second.saturating_sub(window.first).to_f32().unwrap_or(0.0);
    let span = window.span.to_f32().unwrap_or(1.0).max(1.0);
    offset / span
}

fn wall_second() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}
