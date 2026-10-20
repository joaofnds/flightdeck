use num_traits::ToPrimitive;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    Bounds, Context, IntoElement, Render, SharedString, Window, canvas, div, fill, point,
    prelude::*, px, rgb, size,
};
use serde_json::Value;

use crate::charts::{self, GRID, Inspection, MUTED, PANEL};
use crate::data::Panel;
use crate::live_dashboard::LiveDashboard;
use crate::live_hover::Origin;
use crate::live_presentation::clock;
use crate::live_source::Snapshot;

/// The source of the readings an arrow step on a chart shows.
const CHART_STEP_SOURCE: &str = "chart-step";

#[cfg(test)]
#[path = "live_hover_cost_tests.rs"]
mod cost_tests;
#[path = "live_chart_inspector.rs"]
pub(crate) mod inspector;
#[path = "live_timeline_view.rs"]
pub(crate) mod timeline;

impl LiveDashboard {
    pub fn live_chart(&self, panel: &Rc<Panel>, cx: &mut Context<Self>) -> impl IntoElement {
        let index = self.chart_inspection_index(panel);
        let inspection = chart_inspection(panel, index);
        let hidden = self.chart_hidden(panel);
        let scale = charts::axis_scale(panel, inspection, &hidden, "left");
        let trailing = trailing_axes(panel, inspection, &hidden);
        let time = self.chart_time_window();
        let measured = Rc::new(Cell::new(Bounds::default()));
        let plot = charts::plot_without_cursor(
            Rc::clone(panel),
            inspection,
            hidden,
            Rc::clone(&measured),
            time,
        );
        let cursor = self.chart_cursor_fraction(panel);
        assert!(
            panel.kind != "timeline",
            "a timeline draws through live_timeline"
        );
        let height = 192.0;
        let chart = self
            .chart_events(
                div()
                    .relative()
                    .h(px(height))
                    .child(plot)
                    .child(chart_cursor(cursor, false))
                    .children(self.chart_tooltip(panel, index, cursor)),
                measured,
                panel,
                cx,
            )
            .into_any_element();
        let axes = PlotAxes {
            leading: charts::axis_labels(scale, charts::axis_unit(panel, "left")),
            trailing,
            height,
        };
        let time_row = chart_time_row(&axes, time);
        let reading = if is_process_disk(panel) {
            self.chart_disk_headline(panel, index)
        } else {
            self.chart_reading(panel, index)
        };

        div()
            .debug_selector(|| format!("chart-card:{}", panel.id))
            .flex_1()
            .min_w_0()
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(charts::LINE))
            .bg(rgb(PANEL))
            .px(px(12.0))
            .py(px(8.0))
            .child(Self::chart_heading(panel, cx))
            .child(chart_plot_row(&panel.id, reading, axes, chart))
            .child(time_row)
            .child(self.chart_legends(panel, index, cx))
            .children(panel.caption.clone().map(timeline::timeline_caption_text))
    }

    fn chart_time_window(&self) -> Option<charts::TimeWindow> {
        self.view_window()
    }

    fn chart_cursor_fraction(&self, panel: &Panel) -> Option<f32> {
        let hover = self.chart_hover.as_ref()?;
        let snapshot = self.snapshot.as_deref()?;
        if let Some(target) = self.reading_target() {
            if let Some(index) = target_index(panel, snapshot, target) {
                if let Some(position) = panel.positions.as_ref()?.get(index) {
                    return Some(*position);
                }
            }
        }

        cursor_fraction(panel, snapshot, self.view_window(), Some(hover.second))
    }

    fn chart_process_groups(
        &self,
        panel: &Panel,
    ) -> Option<&viewer::app_process_groups_history::AppProcessGroupsHistory> {
        if !panel.id.starts_with("live-process-groups-") {
            return None;
        }

        self.snapshot.as_ref()?.process_history.as_ref()?.all()
    }

    fn chart_inspection_index(&self, panel: &Panel) -> Option<usize> {
        self.snapshot
            .as_deref()
            .and_then(|snapshot| match self.reading_target() {
                Some(target) => target_index(panel, snapshot, target),
                None => inspection_index(panel, snapshot, None),
            })
    }

    fn chart_reading(&self, panel: &Panel, index: Option<usize>) -> String {
        if let Some(history) = self.chart_process_groups(panel) {
            return format!("{} captured named groups", history.named_group_count);
        }
        if let Some(history) = self.chart_attribution_history(panel) {
            return history.headline(index);
        }
        if panel.id == crate::live_resident_history::PANEL_ID {
            return self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.resident_history.as_ref())
                .map_or_else(|| "Unavailable".into(), |history| history.headline(index));
        }
        if is_process_disk(panel) {
            if panel.id == APP_DISK_PANEL {
                return self.chart_line_reading(panel, 0, index);
            }
            return format!(
                "Read {}; Write {}",
                self.chart_line_reading(panel, 0, index),
                self.chart_line_reading(panel, 1, index)
            );
        }

        if panel.id == "live-battery-charging" {
            let charging = self.reading_snapshot().and_then(|snapshot| {
                crate::live_views::matching_summary(&snapshot.selection)["battery"]["charging"]
                    .as_bool()
            });

            return match charging {
                Some(true) => "Charging",
                Some(false) => "Not charging",
                None => "Unavailable",
            }
            .into();
        }

        if panel.id == "live-memory-pressure" {
            let selection = self
                .reading_snapshot()
                .map_or(&Value::Null, |snapshot| &snapshot.selection);
            let summary = crate::live_views::matching_summary(selection);
            return crate::live_views::pressure(&summary["pressure"]).to_owned();
        }

        chart_reading(panel, index)
    }

    fn chart_line_reading(&self, panel: &Panel, line: usize, index: Option<usize>) -> String {
        if let Some(history) = self.chart_process_groups(panel) {
            return crate::live_process_history::groups::reading(history, &panel.id, line, index);
        }
        if let Some(reading) = self.snapshot.as_ref().and_then(|snapshot| {
            crate::live_process_history::captured_reading(
                snapshot.process_history.as_ref()?.payload()?,
                &panel.id,
                index,
            )
        }) {
            return reading;
        }

        if let Some(history) = self.chart_attribution_history(panel) {
            return history.reading(line, index);
        }
        if panel.id == crate::live_resident_history::PANEL_ID {
            return self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.resident_history.as_ref())
                .map_or_else(
                    || "Unavailable".into(),
                    |history| history.reading(line, index),
                );
        }
        let value = chart_line_value(panel, line, index);
        let unit = panel
            .lines
            .get(line)
            .and_then(|line| line.unit.as_deref())
            .unwrap_or(&panel.unit);
        let reading = line_reading(panel, value, unit);
        if !is_process_disk(panel) {
            return reading;
        }

        if value.is_none() {
            return reading;
        }

        let Some((measured, unavailable)) = self.chart_disk_counts(panel, line, index) else {
            return format!("{reading} · coverage unavailable");
        };

        if unavailable == 0 {
            return reading;
        }

        format!("{reading} · known subtotal · {measured} measured · {unavailable} unavailable")
    }

    fn chart_disk_headline(&self, panel: &Panel, index: Option<usize>) -> String {
        if panel.id == APP_DISK_PANEL {
            return self.chart_disk_headline_line(panel, 0, index);
        }
        format!(
            "Read {} · Write {}",
            self.chart_disk_headline_line(panel, 0, index),
            self.chart_disk_headline_line(panel, 1, index)
        )
    }

    fn chart_disk_headline_line(&self, panel: &Panel, line: usize, index: Option<usize>) -> String {
        let value = chart_line_value(panel, line, index);
        let reading = line_reading(panel, value, &panel.unit);
        if value.is_none() {
            return reading;
        }

        let Some((_, unavailable)) = self.chart_disk_counts(panel, line, index) else {
            return format!("{reading} · coverage unavailable");
        };

        if unavailable > 0 {
            return format!("{reading} *");
        }

        reading
    }

    fn chart_disk_counts(
        &self,
        panel: &Panel,
        line: usize,
        index: Option<usize>,
    ) -> Option<(u64, u64)> {
        assert!(line < 2);
        let field = if panel.id == APP_DISK_PANEL {
            assert_eq!(line, 0);
            "disk_io_bytes"
        } else {
            ["disk_read_bytes", "disk_written_bytes"][line]
        };
        // Panels retain the accepted history while exact preview details load.
        let snapshot = self.snapshot.as_ref()?;
        let measure = &process_history_payload(panel, snapshot)?["points"].get(index?)?[field];

        Some((
            measure["measured_members"].as_u64()?,
            measure["unavailable_members"].as_u64()?,
        ))
    }

    fn chart_events(
        &self,
        chart: gpui::Div,
        measured: Rc<Cell<Bounds<gpui::Pixels>>>,
        panel: &Rc<Panel>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = self.chart_hover_target(panel, measured, cx);
        let chart = self.chart_accessibility(chart, panel, cx);
        chart
            .focusable()
            .tab_stop(true)
            .focus(crate::charts::focus_ring)
            .on_key_down(cx.listener(Self::chart_keyboard))
            .child(hover)
    }

    fn chart_hover_target(
        &self,
        panel: &Rc<Panel>,
        measured: Rc<Cell<Bounds<gpui::Pixels>>>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let source = SharedString::from(panel.id.clone());
        let move_source = source.clone();
        let move_bounds = Rc::clone(&measured);
        let move_panel = Rc::clone(panel);
        let hover_panel = Rc::clone(panel);
        let drag = (source.clone(), Rc::clone(&measured));
        let target = div()
            .id("plot-hover")
            .absolute()
            .top(charts::PLOT_INSETS.top)
            .right(charts::PLOT_INSETS.right)
            .bottom(charts::PLOT_INSETS.bottom)
            .left(charts::PLOT_INSETS.left)
            .min_w(px(1.0))
            .min_h(px(1.0))
            .children(self.reading_membership_observer(move_source.clone(), cx))
            .children(self.drag_band(&source))
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    let hovered_at = Instant::now();
                    if !this.chart_anchor_matches(&move_panel) {
                        return;
                    }

                    this.chart_plot_width = move_bounds.get().size.width;
                    this.drag_move(&move_source, move_bounds.get(), event.position.x, cx);
                    let second =
                        chart_hover_second(this.view_window(), move_bounds.get(), event.position);
                    this.chart_hover_update(&move_source, second, hovered_at, cx);
                }),
            )
            .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                let hovered_at = Instant::now();
                if !this.chart_anchor_matches(&hover_panel) {
                    return;
                }

                if *hovered {
                    if this.pointer == crate::live_hover::controller::PointerPlace::OutsideWindow {
                        return;
                    }
                } else {
                    if this.reading_keyboard_hover_retained(
                        charts::plot_bounds(measured.get()),
                        window,
                    ) {
                        return;
                    }
                }

                let second = if *hovered {
                    this.chart_plot_width = measured.get().size.width;
                    chart_hover_second(this.view_window(), measured.get(), window.mouse_position())
                } else {
                    None
                };
                this.chart_hover_update(&source, second, hovered_at, cx);
            }));

        drag_input(target, drag, cx)
    }

    fn chart_hover_update(
        &mut self,
        source: &SharedString,
        second: Option<u64>,
        hovered_at: Instant,
        cx: &mut Context<Self>,
    ) {
        if let Some(second) = second {
            let Some(snapshot) = self.snapshot.as_deref() else {
                return;
            };

            let target = crate::live_hover::Target::regular(snapshot, second);
            self.hover_reading_target(source.clone(), target, hovered_at, cx);
        } else {
            self.clear_reading_target(source, cx);
        }
    }

    fn chart_anchor_matches(&self, panel: &Rc<Panel>) -> bool {
        self.panels
            .iter()
            .take(16)
            .any(|anchor| Rc::ptr_eq(anchor, panel))
    }

    fn chart_accessibility(
        &self,
        chart: gpui::Div,
        panel: &Panel,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (second, reading) = self.chart_accessibility_reading(panel);
        let first = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.catalog["first_second"].as_u64());
        let last = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.catalog["last_second"].as_u64());
        chart
            .id(SharedString::from(format!("{}-plot", panel.id)))
            .debug_selector(|| format!("{}-plot", panel.id))
            .accessibility_id(format!("flightdeck.chart.{}", panel.id))
            .role(gpui::Role::Slider)
            .aria_label(format!("{} history inspection", panel.title))
            .aria_description(
                "Left and right inspect adjacent seconds and Enter selects the one shown; \
                 unavailable readings are gaps",
            )
            .when_some(second.and_then(|value| value.to_f64()), |chart, value| {
                chart.aria_numeric_value(value).aria_numeric_value_step(1.0)
            })
            .when_some(first.and_then(|value| value.to_f64()), |chart, value| {
                chart.aria_min_numeric_value(value)
            })
            .when_some(last.and_then(|value| value.to_f64()), |chart, value| {
                chart.aria_max_numeric_value(value)
            })
            .on_a11y_action(gpui::AccessibleAction::Increment, {
                let entity = cx.entity().downgrade();
                move |_, window, cx| {
                    // Focus lets moving it away end the step, as it ends an arrow key's.
                    let stepped = entity.update(cx, |this, cx| {
                        this.chart_step(1, cx);
                        this.focus_inspection(window, cx);
                    });
                    if let Err(error) = stepped {
                        eprintln!("Chart accessibility action: {error}");
                    }
                }
            })
            .on_a11y_action(gpui::AccessibleAction::Decrement, {
                let entity = cx.entity().downgrade();
                move |_, window, cx| {
                    // Focus lets moving it away end the step, as it ends an arrow key's.
                    let stepped = entity.update(cx, |this, cx| {
                        this.chart_step(-1, cx);
                        this.focus_inspection(window, cx);
                    });
                    if let Err(error) = stepped {
                        eprintln!("Chart accessibility action: {error}");
                    }
                }
            })
            .aria_value(reading)
    }

    pub(crate) fn chart_accessibility_reading(&self, panel: &Panel) -> (Option<u64>, String) {
        let second = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.selection["second"].as_u64());
        let mut reading = format!(
            "{}; {}",
            second.map_or_else(
                || "Waiting".into(),
                |second| format!("{}; Unix second {second}", clock(second, true))
            ),
            self.chart_reading(panel, self.chart_inspection_index(panel))
        );
        if let Some(history) = self.chart_attribution_history(panel) {
            if let Some(reference) = history.reference_reading(self.chart_inspection_index(panel)) {
                reading.push_str("; ");
                reading.push_str(&reference);
            }
        }

        (second, reading)
    }

    fn chart_keyboard(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(delta) = crate::dashboard::keyboard_step(event) {
            self.chart_step(delta, cx);
        } else {
            if self.chart_stepped_second().is_none() {
                return;
            }
            match event.keystroke.key.as_str() {
                "enter" => self.chart_step_select(cx),
                "escape" => self.end_reading_preview(cx),
                _ => return,
            }
        }
        self.focus_inspection(window, cx);

        window.prevent_default();
        cx.stop_propagation();
    }

    pub(crate) fn focus_inspection(&self, window: &mut Window, cx: &mut Context<Self>) {
        assert!(self.inspection_focus.is_some());
        if let Some(focus) = &self.inspection_focus {
            window.focus(focus, cx);
        }
    }

    pub(crate) fn inspection_keyboard(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .inspection_focus
            .as_ref()
            .is_some_and(|focus| focus.is_focused(window))
        {
            self.chart_keyboard(event, window, cx);
        }
    }

    /// Shows the second `delta` seconds from the one shown, from the readings the page holds,
    /// as a hover does, so a step reads nothing from the recording until Enter selects it.
    pub(crate) fn chart_step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };

        let selection = KeyboardSelection {
            stepped: self.chart_stepped_second(),
            accepted: snapshot.selection["second"].as_u64(),
        };
        let Some(next) = keyboard_second(&selection, delta, &snapshot.catalog) else {
            return;
        };
        if Some(next) == selection.stepped.or(selection.accepted) {
            return;
        }

        let target = crate::live_hover::Target::regular(snapshot, next);
        self.set_reading_target(CHART_STEP_SOURCE.into(), target, Origin::Keyboard, cx);
    }

    /// Selects the second an arrow step shows, which then reads it with its context.
    fn chart_step_select(&mut self, cx: &mut Context<Self>) {
        let Some(second) = self.chart_stepped_second() else {
            return;
        };

        self.inspect(second, cx);
    }

    /// Watches the charts' focus, so a step ends when the charts lose it, as a keyboard
    /// inspection of the CPU history does.
    pub(crate) fn install_inspection_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inspection_focus.is_some() {
            return;
        }

        let focus = cx.focus_handle();
        cx.on_blur(&focus, window, |this, _, cx| {
            if this.chart_stepped_second().is_some() {
                this.end_reading_preview(cx);
            }
        })
        .detach();
        self.inspection_focus = Some(focus);
    }

    /// The second an arrow step on a chart shows, while it shows one.
    fn chart_stepped_second(&self) -> Option<u64> {
        let preview = self.preview.as_ref()?;
        if preview.source.as_ref() != CHART_STEP_SOURCE {
            return None;
        }

        Some(preview.target.second)
    }

    fn chart_heading(panel: &Rc<Panel>, cx: &mut Context<Self>) -> impl IntoElement {
        let help = panel.help.clone().unwrap_or_else(|| {
            "Unavailable readings remain gaps. Drag across a chart to zoom to that stretch.".into()
        });
        let inspector_panel = Rc::clone(panel);
        let keyboard_panel = Rc::clone(panel);
        let data = chart_head_button(format!("data-{}", panel.id), "Data")
            .debug_selector(|| format!("chart-data:{}", panel.id))
            .aria_label(format!("Data for {}", panel.title))
            .w(px(44.0))
            .border_1()
            .border_color(charts::alpha(charts::TEXT, 0.12))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_chart_inspector(&inspector_panel, window, cx);
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if ["enter", "space"].contains(&event.keystroke.key.as_str()) {
                        this.open_chart_inspector(&keyboard_panel, window, cx);
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }),
            );
        let about = chart_head_button(format!("about-{}", panel.id), "ⓘ")
            .px(px(8.0))
            .tooltip(move |_, cx| cx.new(|_| ChartHelp(help.clone())).into());

        div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .min_h(px(44.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(14.0))
                    .line_height(px(21.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(panel.title.clone()),
            )
            .child(data)
            .child(about)
    }

    fn chart_hidden(&self, panel: &Panel) -> Rc<Vec<String>> {
        Rc::new(
            panel
                .lines
                .iter()
                .take(16)
                .enumerate()
                .filter(|(index, _)| self.hidden.contains(&self.chart_legend_key(panel, *index)))
                .map(|(_, line)| line.name.clone())
                .collect(),
        )
    }

    pub(crate) fn chart_legend_key(&self, panel: &Panel, index: usize) -> String {
        let line = &panel.lines[index];
        if let Some(history) = self.chart_attribution_history(panel) {
            if history.is_reference(index) {
                return format!("{}\tmachine-reference", panel.id);
            }

            return format!("{}\tapp-series\t{}", panel.id, line.name);
        }

        format!("{}\t{}", panel.id, line.name)
    }

    fn chart_legends(
        &self,
        panel: &Panel,
        index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut legends = div()
            .flex()
            .flex_wrap()
            .gap_x(px(12.0))
            .mt(px(2.0))
            .text_size(px(12.0))
            .line_height(px(20.0));
        for (line_index, line) in panel.lines.iter().take(16).enumerate() {
            let key = self.chart_legend_key(panel, line_index);
            let reading = self.chart_line_reading(panel, line_index, index);
            let shown = (panel.lines.len() <= LEGEND_VALUE_LINES_MAX).then_some(reading.as_str());
            let label = Self::chart_legends_label(panel, line_index, shown);
            legends = legends.child(
                div()
                    .id(SharedString::from(key.clone()))
                    .debug_selector(|| format!("chart-legend:{}:{}:{reading}", panel.id, line.name))
                    .accessibility_id(format!("flightdeck.legend.{}.{}", panel.id, line.name))
                    .role(gpui::Role::Switch)
                    .aria_label(format!("{}: {}", panel.title, line.name))
                    .aria_description(reading.clone())
                    .aria_toggled(if self.hidden.contains(&key) {
                        gpui::Toggled::False
                    } else {
                        gpui::Toggled::True
                    })
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .focus(crate::charts::focus_ring)
                    .py(px(2.0))
                    .opacity(if self.hidden.contains(&key) {
                        0.35
                    } else {
                        1.0
                    })
                    .child(label)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_chart_legend(&key, cx)),
                    ),
            );
        }
        legends
    }

    fn toggle_chart_legend(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(index) = self.hidden.iter().position(|value| value == key) {
            self.hidden.remove(index);
        } else {
            self.hidden.push(key.into());
        }
        cx.notify();
    }

    fn chart_legends_label(panel: &Panel, index: usize, reading: Option<&str>) -> gpui::AnyElement {
        let line = &panel.lines[index];
        let tint = charts::color(&line.color);
        let swatch = if line.dashed == Some(true) {
            div().w(px(12.0)).h(px(3.0)).bg(rgb(tint))
        } else {
            div().w(px(12.0)).h(px(4.0)).rounded(px(2.0)).bg(rgb(tint))
        };

        div()
            .when(
                panel.id == crate::live_resident_history::PANEL_ID,
                |label| label.debug_selector(|| line.name.clone()),
            )
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(swatch.flex_shrink_0())
            .child(line.name.clone())
            .when_some(reading, |label, reading| {
                label.child(div().text_color(rgb(MUTED)).child(reading.to_owned()))
            })
            .into_any_element()
    }
}

/// A button of a chart card's heading, as tall as the heading and set in the muted small type.
pub(crate) fn chart_head_button(id: String, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .role(gpui::Role::Button)
        .aria_label(label)
        .flex()
        .items_center()
        .justify_center()
        .h(px(44.0))
        .flex_shrink_0()
        .rounded(px(5.0))
        .text_size(px(11.0))
        .line_height(px(16.5))
        .text_color(rgb(MUTED))
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .focus(crate::charts::focus_ring)
        .hover(|style| style.bg(rgb(0x2c_2f_35)))
        .child(label)
}

/// The most lines a chart's legend shows each line's reading for. Past it the legend names
/// the lines alone, as the prototype's does, and the inspector holds the readings.
const LEGEND_VALUE_LINES_MAX: usize = 4;

pub(crate) struct ChartHelp(pub(crate) String);

struct KeyboardSelection {
    stepped: Option<u64>,
    accepted: Option<u64>,
}

fn keyboard_second(selection: &KeyboardSelection, delta: i64, catalog: &Value) -> Option<u64> {
    let second = selection.stepped.or(selection.accepted)?;
    let mut next = second.saturating_add_signed(delta);
    if let Some(first) = catalog["first_second"].as_u64() {
        next = next.max(first);
    }
    if let Some(last) = catalog["last_second"].as_u64() {
        next = next.min(last);
    }
    Some(next)
}

fn chart_hover_second(
    window: Option<charts::TimeWindow>,
    bounds: Bounds<gpui::Pixels>,
    position: gpui::Point<gpui::Pixels>,
) -> Option<u64> {
    let rect = charts::plot_bounds(bounds);
    if !rect.contains(&position) {
        return None;
    }

    let fraction = f32::from(position.x - rect.origin.x) / f32::from(rect.size.width);
    window?.second_at(fraction)
}

/// The selected App's disk chart, one line of the bytes read and written together as the
/// prototype draws it. A process group's disk chart keeps read and written apart.
const APP_DISK_PANEL: &str = "live-app-disk";

fn is_process_disk(panel: &Panel) -> bool {
    matches!(
        panel.id.as_str(),
        APP_DISK_PANEL | crate::live_process_history::DISK_PANEL
    )
}

fn is_process_history(panel: &Panel) -> bool {
    if panel.id.starts_with("live-app-") {
        return true;
    }

    panel
        .id
        .starts_with(crate::live_process_history::PANEL_PREFIX)
}

fn process_history_payload<'a>(panel: &Panel, snapshot: &'a Snapshot) -> Option<&'a Value> {
    if panel
        .id
        .starts_with(crate::live_process_history::PANEL_PREFIX)
    {
        return snapshot.process_history.as_ref()?.payload();
    }

    snapshot.history.as_ref()
}

fn target_index(
    panel: &Panel,
    snapshot: &Snapshot,
    target: &crate::live_hover::Target,
) -> Option<usize> {
    target.identity.as_ref()?;
    if panel.id.starts_with("live-process-groups-") {
        return crate::live_process_history::groups::index(
            snapshot.process_history.as_ref()?.all()?,
            target,
        );
    }
    if let Some(metric) = crate::live_app_attribution_history::Metric::from_panel(&panel.id) {
        return snapshot.attribution_history(metric)?.index(Some(target));
    }
    if panel.id == crate::live_resident_history::PANEL_ID {
        return snapshot.resident_history.as_ref()?.index(Some(target));
    }
    if panel.kind == "timeline" {
        return inspection_index(panel, snapshot, Some(target.second));
    }
    if is_process_history(panel) {
        return process_history_payload(panel, snapshot)?["points"]
            .as_array()?
            .iter()
            .take(crate::data::LIVE_SAMPLE_LIMIT)
            .rposition(|point| target.matches(point.get("observation").unwrap_or(point)));
    }
    let recent = snapshot.selection["trends"]
        .as_array()?
        .iter()
        .take(crate::data::LIVE_SAMPLE_LIMIT)
        .rposition(|point| target.matches(point));
    if let Some(index) = recent {
        return Some(index + crate::live_projection::older_count(&snapshot.selection));
    }

    snapshot.selection["earlier_trends"]
        .as_array()?
        .iter()
        .take(crate::data::LIVE_SAMPLE_LIMIT)
        .rposition(|point| target.matches(point))
}

/// The cursor over a chart. A state timeline draws it dashed and always, as the prototype does,
/// and a line chart draws it solid while the pointer is over it.
fn chart_cursor(fraction: Option<f32>, dashed: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let Some(fraction) = fraction else {
                return;
            };
            let rect = charts::plot_bounds(bounds);
            let x = rect.origin.x + rect.size.width * fraction;
            if !dashed {
                window.paint_quad(fill(
                    Bounds::new(point(x, rect.origin.y), size(px(1.0), rect.size.height)),
                    charts::cursor_color(),
                ));
                return;
            }
            let dash = px(3.0);
            let mut top = rect.origin.y;
            let bottom = rect.origin.y + rect.size.height;
            while top < bottom {
                let length = dash.min(bottom - top);
                window.paint_quad(fill(
                    Bounds::new(point(x, top), size(px(1.0), length)),
                    charts::alpha(charts::TEXT, 0.7),
                ));
                top += dash * 2.0;
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Lets a press on the plot start a drag, and its release anywhere end it.
fn drag_input(
    plot: gpui::Stateful<gpui::Div>,
    (source, measured): (SharedString, Rc<Cell<Bounds<gpui::Pixels>>>),
    cx: &mut Context<LiveDashboard>,
) -> gpui::Stateful<gpui::Div> {
    let (down_source, down_bounds) = (source.clone(), Rc::clone(&measured));
    let (up_source, up_bounds) = (source.clone(), Rc::clone(&measured));

    plot.on_mouse_down(
        gpui::MouseButton::Left,
        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
            window.prevent_default();
            this.drag_begin(&down_source, (down_bounds.get(), event), cx);
        }),
    )
    .on_mouse_up(
        gpui::MouseButton::Left,
        cx.listener(move |this, event: &gpui::MouseUpEvent, _, cx| {
            this.drag_end(&up_source, (up_bounds.get(), event), cx);
        }),
    )
    .on_mouse_up_out(
        gpui::MouseButton::Left,
        cx.listener(move |this, event: &gpui::MouseUpEvent, _, cx| {
            this.drag_end(&source, (measured.get(), event), cx);
        }),
    )
}

fn cursor_fraction(
    panel: &Panel,
    snapshot: &Snapshot,
    view: Option<charts::TimeWindow>,
    hover: Option<u64>,
) -> Option<f32> {
    let second = hover?;
    if let Some(index) = inspection_index(panel, snapshot, Some(second)) {
        return panel.positions.as_ref()?.get(index).copied();
    }

    view?.fraction_of(second)
}

impl Render for ChartHelp {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let text = self.0.clone();
        div()
            .debug_selector(move || text.clone())
            .w(px(320.0))
            .p(px(12.0))
            .rounded(px(6.0))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(GRID))
            .text_size(px(12.0))
            .text_color(rgb(crate::charts::TEXT))
            .font_family("Helvetica Neue")
            .child(self.0.clone())
    }
}

fn chart_reading(panel: &Panel, index: Option<usize>) -> String {
    let value = if panel.id == "live-power-breakdown" {
        chart_line_value(panel, 0, index)
            .zip(chart_line_value(panel, 1, index))
            .map(|(remainder, gpu)| remainder + gpu)
    } else {
        chart_line_value(panel, 0, index)
    };
    if let Some(states) = &panel.states {
        return states
            .iter()
            .enumerate()
            .find(|(index, _)| value == Some(charts::number(*index)))
            .map_or_else(|| "Unavailable".to_owned(), |(_, state)| state.clone());
    }

    let unit = if panel.id == "live-memory" {
        "GiB app memory"
    } else {
        &panel.unit
    };
    line_reading(panel, value, unit)
}

fn chart_line_value(panel: &Panel, line: usize, index: Option<usize>) -> Option<f32> {
    panel
        .lines
        .get(line)
        .and_then(|line| index.and_then(|index| line.values.get(index).copied().flatten()))
}

/// The value axes beside a plot `height` tall: the left axis, then any right-hand axes in
/// order outward.
struct PlotAxes {
    leading: crate::chart_axes::AxisLabels,
    trailing: Vec<crate::chart_axes::AxisLabels>,
    height: f32,
}

/// The labels of each right-hand axis that a visible line reads on.
fn trailing_axes(
    panel: &Panel,
    inspection: charts::Inspection,
    hidden: &[String],
) -> Vec<crate::chart_axes::AxisLabels> {
    let mut axes = Vec::new();
    for axis in ["right1", "right2"] {
        let visible = panel.lines.iter().any(|line| {
            if hidden.contains(&line.name) {
                return false;
            }
            line.axis.as_deref() == Some(axis)
        });
        if visible {
            let scale = charts::axis_scale(panel, inspection, hidden, axis);
            axes.push(charts::axis_labels(scale, charts::axis_unit(panel, axis)));
        }
    }
    axes
}

/// The value axes beside the plot. The headline reading left the card for the legend, as in
/// the prototype, and the row's selectors keep naming the reading the plot shows.
fn chart_plot_row(
    id: &str,
    reading: String,
    axes: PlotAxes,
    chart: gpui::AnyElement,
) -> impl IntoElement {
    let selector = format!("chart-reading:{id}:{reading}");
    let height = px(axes.height);
    div().debug_selector(|| selector).child(
        div()
            .debug_selector(|| reading)
            .flex()
            .child(charts::axis_column(
                axes.leading,
                height,
                charts::AxisSide::Leading,
            ))
            .child(div().flex_1().min_w_0().child(chart))
            .children(
                axes.trailing
                    .into_iter()
                    .map(|labels| charts::axis_column(labels, height, charts::AxisSide::Trailing)),
            ),
    )
}

/// The time labels under the plot, inset by the value axes on either side.
fn chart_time_row(
    axes: &PlotAxes,
    time: Option<crate::chart_axes::TimeWindow>,
) -> impl IntoElement + use<> {
    let trailing_width = axes
        .trailing
        .iter()
        .fold(px(0.0), |width, labels| width + labels.width);
    div()
        .pl(axes.leading.width)
        .pr(trailing_width)
        .h(px(crate::chart_axes::TIME_ROW_HEIGHT))
        .child(charts::time_labels(time))
}

/// Inspects the hovered reading, else the selected second, else the newest reading.
fn chart_inspection(panel: &Panel, index: Option<usize>) -> Inspection {
    let newest = panel
        .lines
        .first()
        .map_or(0, |line| line.values.len().saturating_sub(1));
    let (lower, upper) = match &panel.positions {
        Some(positions) if panel.window.is_some() => {
            let (lower, upper) = crate::time_range::visible(positions);
            (lower, upper.min(newest))
        }
        _ => (0, newest),
    };
    assert!(lower <= upper);
    Inspection {
        lower,
        selected: index.or(panel.selected_index).unwrap_or(upper),
        upper,
    }
}

fn line_reading(panel: &Panel, value: Option<f32>, unit: &str) -> String {
    if let Some(states) = &panel.states {
        if panel.kind == "timeline" {
            // A timeline stores each state as its index among the panel's states.
            let state = value
                .and_then(|value| value.to_usize())
                .and_then(|index| states.get(index));
            return state.map_or_else(|| "Unavailable".into(), Clone::clone);
        }
    }
    value.map_or_else(
        || "Unavailable".into(),
        |value| crate::live_projection::quantity_text(value, unit),
    )
}

fn inspection_index(panel: &Panel, snapshot: &Snapshot, second: Option<u64>) -> Option<usize> {
    let Some(second) = second else {
        return panel.selected_index;
    };
    if snapshot.selection["second"].as_u64() == Some(second) {
        return panel.selected_index;
    }
    if panel.id.starts_with("live-process-groups-") {
        let target = crate::live_hover::Target::regular(snapshot, second);
        return crate::live_process_history::groups::index(
            snapshot.process_history.as_ref()?.all()?,
            &target,
        );
    }
    if let Some(metric) = crate::live_app_attribution_history::Metric::from_panel(&panel.id) {
        let target = crate::live_hover::Target::regular(snapshot, second);
        return snapshot.attribution_history(metric)?.index(Some(&target));
    }
    if panel.id == crate::live_resident_history::PANEL_ID {
        let target = crate::live_hover::Target::regular(snapshot, second);
        return snapshot.resident_history.as_ref()?.index(Some(&target));
    }

    if panel.kind == "timeline" {
        let first = match panel.window {
            Some(window) => window.first,
            None => snapshot.selection["second"].as_u64()?.saturating_sub(899),
        };
        let Ok(offset) = usize::try_from(second.checked_sub(first)?) else {
            return None;
        };
        return (offset < panel.lines.first()?.values.len()).then_some(offset);
    }

    let at_second = |point: &Value| {
        let stamp = point.get("observation").unwrap_or(point);
        wall_second(&stamp["wall_ns"]) == Some(second)
    };
    if is_process_history(panel) {
        return process_history_payload(panel, snapshot)?["points"]
            .as_array()?
            .iter()
            .take(crate::data::LIVE_SAMPLE_LIMIT)
            .rposition(at_second);
    }
    let recent = snapshot.selection["trends"]
        .as_array()?
        .iter()
        .take(crate::data::LIVE_SAMPLE_LIMIT)
        .rposition(at_second);
    if let Some(index) = recent {
        return Some(index + crate::live_projection::older_count(&snapshot.selection));
    }

    crate::live_projection::older_index(&snapshot.selection, second)
}

fn wall_second(value: &Value) -> Option<u64> {
    let wall = if let Some(value) = value.as_u64() {
        value
    } else {
        match value.as_str()?.parse::<u64>() {
            Ok(value) => value,
            Err(_) => return None,
        }
    };
    Some(wall / 1_000_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn keyboard_steps_walk_from_the_stepped_second_within_the_recording() {
        // The second a step shows is the next step's input, held to the recording's edges.
        let catalog = json!({"first_second":900,"last_second":1100});
        let mut selection = KeyboardSelection {
            stepped: None,
            accepted: Some(1000),
        };
        selection.stepped = keyboard_second(&selection, 1, &catalog);
        assert_eq!(keyboard_second(&selection, 1, &catalog), Some(1002));
        assert_eq!(keyboard_second(&selection, -1, &catalog), Some(1000));
        selection.stepped = Some(1100);
        assert_eq!(keyboard_second(&selection, 1, &catalog), Some(1100));
        selection.stepped = Some(900);
        assert_eq!(keyboard_second(&selection, -1, &catalog), Some(900));
    }

    #[test]
    fn a_zoomed_chart_inspects_only_the_readings_inside_its_view() {
        // Goal: a chart zoomed to part of the history paints and scales to the readings it shows.
        // Method: place readings on both sides of the plot and read the inspected index range.
        let panel = json!({
            "selectedIndex": 1, "positions": [-0.5, 0.2, 0.6, 1.2], "segments": [0, 0, 0, 0],
            "id": "p", "title": "P", "unit": "%", "kind": "line",
            "lines": [{"name": "a", "color": "#fff", "values": [1.0, 2.0, 3.0, 4.0]}],
        });
        let mut panel: Panel = serde_json::from_value(panel).unwrap();
        panel.window = Some(crate::chart_axes::TimeWindow { first: 0, span: 60 });

        let inspection = chart_inspection(&panel, None);

        assert_eq!(
            (inspection.lower, inspection.selected, inspection.upper),
            (1, 1, 2)
        );
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            generation: 1,
            catalog: Value::Null,
            selection: json!({"second":1000,"trends":[
                {"wall_ns":998_100_000_000_u64},
                {"wall_ns":1_000_100_000_000_u64},
                {"wall_ns":1_000_200_000_000_u64}]}),
            pages: BTreeMap::new(),
            apps: None,
            detail: None,
            history: None,
            cpu_detail: None,
            cpu_history: None,
            resident_history: None,
            app_attribution_history: None,
            overview_app_histories: None,
            histories_pending: false,
            process_history: None,
            rankings: None,
            app_ranks: None,
            app_parts: None,
            read_ms: 0.0,
        }
    }

    fn panel() -> Panel {
        serde_json::from_value(json!({"id":"live-cpu","title":"CPU","kind":"line",
            "unit":"%","lines":[],"positions":[0.998,0.999,0.9991],
            "selectedIndex":2}))
        .unwrap()
    }

    #[test]
    fn every_chart_reads_small_rates_in_the_unit_that_keeps_their_digits() {
        // Goal: a slow network rate reads as the prototype prints it rather than as 0.0 MB/s.
        // Method: read sub-unit values through a network panel and a CPU panel.
        let network: Panel = serde_json::from_value(json!({"id":"live-network",
            "title":"Network","kind":"line","unit":"MB/s","lines":[]}))
        .unwrap();
        assert_eq!(line_reading(&network, Some(0.0143), "MB/s"), "14.3 kB/s");
        assert_eq!(line_reading(&network, Some(2.5), "MB/s"), "2.5 MB/s");
        assert_eq!(line_reading(&network, None, "MB/s"), "Unavailable");
        assert_eq!(line_reading(&panel(), Some(42.0), "%"), "42%");
    }

    #[test]
    fn timestamp_parsing_keeps_exact_seconds() {
        // Both backend encodings select seconds using integer arithmetic rather than f32 time.
        assert_eq!(
            wall_second(&serde_json::json!("1700000000999999999")),
            Some(1_700_000_000)
        );
        assert_eq!(wall_second(&Value::Null), None);
    }

    #[test]
    fn hover_keeps_selected_alternative_and_does_not_fill_empty_seconds() {
        // Two observations share a second: retain its exact selection, while a gap stays empty.
        let snapshot = snapshot();
        let panel = panel();
        assert_eq!(inspection_index(&panel, &snapshot, None), Some(2));
        assert_eq!(inspection_index(&panel, &snapshot, Some(1000)), Some(2));
        assert_eq!(inspection_index(&panel, &snapshot, Some(998)), Some(0));
        assert_eq!(inspection_index(&panel, &snapshot, Some(999)), None);
        let full = charts::TimeWindow::live(1000);
        let zoomed = charts::TimeWindow {
            first: 900,
            span: 101,
        };
        let fraction = cursor_fraction(&panel, &snapshot, Some(full), Some(999)).unwrap();
        assert!((fraction - 898.5 / 900.0).abs() < f32::EPSILON);
        let fraction = cursor_fraction(&panel, &snapshot, Some(zoomed), Some(999)).unwrap();
        assert!((fraction - 99.5 / 101.0).abs() < f32::EPSILON);
    }

    fn long_snapshot() -> Snapshot {
        let point = |second: u64, publication: u64| {
            json!({"run": "00000000000000010000000000000002",
                "boot": "00000000000000030000000000000004", "segment": 1,
                "wall_ns": second * 1_000_000_000, "publication": publication})
        };
        let mut snapshot = snapshot();
        snapshot.selection = json!({"second": 5000, "earlier_stride_seconds": 10,
            "earlier_trends": [point(2000, 1), point(2010, 2)],
            "trends": [point(4101, 3), point(5000, 4)]});
        snapshot
    }

    fn long_panel(kind: &str, readings: usize) -> Panel {
        let mut panel: Panel = serde_json::from_value(json!({"id":"live-cpu","title":"CPU",
            "kind":kind,"unit":"%","lines":[{"name":"a","color":"#fff",
            "values":vec![1.0; readings]}], "selectedIndex": readings - 1}))
        .unwrap();
        panel.window = Some(charts::TimeWindow {
            first: 1401,
            span: 3600,
        });
        panel
    }

    #[test]
    fn hover_finds_older_readings_after_the_tail_offset_of_every_index() {
        // Goal: a chart over the whole hour hovers the reading its pointer is over, whether it is
        // an older thinned reading or a recent one. Method: ask for seconds in the older tail,
        // between its readings, and in the recent context of a chart that holds both.
        let snapshot = long_snapshot();
        let panel = long_panel("line", 4);

        assert_eq!(inspection_index(&panel, &snapshot, Some(2013)), Some(1));
        assert_eq!(inspection_index(&panel, &snapshot, Some(2000)), Some(0));
        assert_eq!(inspection_index(&panel, &snapshot, Some(2020)), None);
        assert_eq!(inspection_index(&panel, &snapshot, Some(4101)), Some(2));
        assert_eq!(inspection_index(&panel, &snapshot, Some(5000)), Some(3));
        let target = crate::live_hover::Target::regular(&snapshot, 2013);
        assert_eq!(target.second, 2010);
        assert_eq!(target_index(&panel, &snapshot, &target), Some(1));
        let target = crate::live_hover::Target::regular(&snapshot, 4101);
        assert_eq!(target_index(&panel, &snapshot, &target), Some(2));
    }

    #[test]
    fn a_timeline_over_the_whole_hour_hovers_the_cell_of_the_second() {
        // Goal: a state timeline holds a cell per second of the hour, so hover names the cell
        // from the chart's own first second. Method: ask for an older and a recent second.
        let snapshot = long_snapshot();
        let panel = long_panel("timeline", 3600);

        assert_eq!(inspection_index(&panel, &snapshot, Some(2013)), Some(612));
        assert_eq!(inspection_index(&panel, &snapshot, Some(4101)), Some(2700));
    }
}
