use num_traits::ToPrimitive;
use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, Context, IntoElement, MouseButton, Render, SharedString, Window, canvas, div, fill,
    point, prelude::*, px, rgb, size,
};
use serde_json::Value;

use crate::charts::{self, GRID, Inspection, MUTED, PANEL};
use crate::dashboard::control;
use crate::data::{Panel, reading};
use crate::live_dashboard::{ChartHover, LiveDashboard};
use crate::live_presentation::clock;
use crate::live_source::Snapshot;

impl LiveDashboard {
    pub fn live_chart(&self, panel: &Rc<Panel>, cx: &mut Context<Self>) -> impl IntoElement {
        let upper = panel
            .lines
            .first()
            .map_or(0, |line| line.values.len().saturating_sub(1));
        let index = self.snapshot.as_deref().and_then(|snapshot| {
            inspection_index(
                panel,
                snapshot,
                self.chart_hover.as_ref().map(|hover| hover.second),
            )
        });
        let inspection = Inspection {
            lower: 0,
            selected: index.or(panel.selected_index).unwrap_or(upper),
            upper,
        };
        let hidden = self.chart_hidden(panel);
        let domain = charts::domain(panel, inspection, &hidden);
        let measured = Rc::new(Cell::new(Bounds::default()));
        let plot =
            charts::plot_without_cursor(Rc::clone(panel), inspection, hidden, Rc::clone(&measured));
        let cursor = self.snapshot.as_deref().and_then(|snapshot| {
            cursor_fraction(
                panel,
                snapshot,
                self.chart_hover.as_ref().map(|hover| hover.second),
            )
        });
        let chart = self.chart_events(
            div()
                .relative()
                .h(px(if panel.kind == "timeline" {
                    100.0
                } else {
                    190.0
                }))
                .child(plot)
                .child(chart_cursor(cursor)),
            measured,
            panel,
            cx,
        );
        let chart = if panel.kind == "timeline" {
            chart.into_any_element()
        } else {
            Self::chart_ticks(chart, domain).into_any_element()
        };

        div()
            .flex_1()
            .min_w_0()
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .p(px(18.0))
            .child(Self::chart_heading(panel))
            .child(self.chart_headline(panel, index))
            .child(chart)
            .child(self.chart_times())
            .child(self.chart_legends(panel, index, cx))
            .child(self.chart_source_state(panel))
    }

    fn chart_source_state(&self, panel: &Panel) -> gpui::Div {
        let mut notice = div().text_size(px(11.0)).text_color(rgb(MUTED));
        if panel.id != "live-fans" {
            return notice;
        }

        let summary = self
            .snapshot
            .as_ref()
            .map(|snapshot| &snapshot.selection["summary"]);
        let text = match summary.map(|summary| &summary["fan_rpm"]) {
            Some(Value::Array(channels)) if channels.is_empty() => {
                "No fan channels reported by SMC at this observation."
            }
            Some(Value::Array(_)) => "SMC channel indices; physical positions are unreported.",
            _ => "Fan source unavailable at this observation.",
        };
        notice = notice.child(text);

        notice
    }

    fn chart_headline(&self, panel: &Panel, index: Option<usize>) -> impl IntoElement {
        let reading = chart_reading(panel, index);
        let mut headline = div()
            .flex()
            .items_baseline()
            .justify_between()
            .mt(px(12.0))
            .child(
                div()
                    .debug_selector(|| reading.clone())
                    .text_size(px(25.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(reading),
            );
        if panel.stack == Some(true) {
            let summary = self
                .snapshot
                .as_ref()
                .map_or(&Value::Null, |snapshot| &snapshot.selection["summary"]);
            headline = headline.child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(
                format!(
                    "{} installed",
                    crate::live_projection::quantity(
                        summary,
                        "memory_total_bytes",
                        "GiB",
                        1_073_741_824.0,
                    )
                ),
            ));
        }
        headline
    }

    fn chart_events(
        &self,
        chart: gpui::Div,
        measured: Rc<Cell<Bounds<gpui::Pixels>>>,
        panel: &Panel,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hover = Self::chart_hover_target(panel, Rc::clone(&measured), cx);
        let chart = self.chart_accessibility(chart, panel, cx);
        chart
            .focusable()
            .tab_stop(true)
            .focus(|style| style.bg(rgb(0x20_30_41)))
            .on_key_down(cx.listener(Self::chart_keyboard))
            .child(hover)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    let second = this.snapshot.as_ref().and_then(|snapshot| {
                        let rect = charts::plot_bounds(measured.get());
                        let fraction = f32::from(event.position.x - rect.origin.x)
                            / f32::from(rect.size.width);
                        crate::live_projection::sample_second(&snapshot.selection, fraction)
                    });
                    if let Some(second) = second {
                        this.inspect(second, cx);
                        this.focus_inspection(window, cx);
                        window.prevent_default();
                    }
                }),
            )
    }

    fn chart_hover_target(
        panel: &Panel,
        measured: Rc<Cell<Bounds<gpui::Pixels>>>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let source = SharedString::from(panel.id.clone());
        let move_source = source.clone();
        let move_bounds = Rc::clone(&measured);
        div()
            .id("plot-hover")
            .absolute()
            .top(charts::PLOT_INSETS.top)
            .right(charts::PLOT_INSETS.right)
            .bottom(charts::PLOT_INSETS.bottom)
            .left(charts::PLOT_INSETS.left)
            .min_w(px(1.0))
            .min_h(px(1.0))
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    let second = chart_hover_second(
                        this.snapshot.as_deref(),
                        move_bounds.get(),
                        event.position,
                    );
                    this.chart_hover_update(&move_source, second, cx);
                }),
            )
            .on_hover(cx.listener(move |this, hovered, window, cx| {
                let second = if *hovered {
                    chart_hover_second(
                        this.snapshot.as_deref(),
                        measured.get(),
                        window.mouse_position(),
                    )
                } else {
                    None
                };
                this.chart_hover_update(&source, second, cx);
            }))
    }

    fn chart_hover_update(
        &mut self,
        source: &SharedString,
        second: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if let Some(second) = second {
            let changed = self.chart_hover.as_ref().map(|hover| hover.second) != Some(second);
            self.chart_hover = Some(ChartHover {
                source: source.clone(),
                second,
            });
            if changed {
                cx.notify();
            }
        } else {
            if self
                .chart_hover
                .as_ref()
                .is_some_and(|hover| &hover.source == source)
            {
                self.chart_hover = None;
                cx.notify();
            }
        }
    }

    fn chart_accessibility(
        &self,
        chart: gpui::Div,
        panel: &Panel,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let second = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.selection["second"].as_u64());
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
                "Left and right inspect adjacent seconds; unavailable readings are gaps",
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
                move |_, _, cx| {
                    if let Err(error) = entity.update(cx, |this, cx| this.chart_step(1, cx)) {
                        eprintln!("Chart accessibility action: {error}");
                    }
                }
            })
            .on_a11y_action(gpui::AccessibleAction::Decrement, {
                let entity = cx.entity().downgrade();
                move |_, _, cx| {
                    if let Err(error) = entity.update(cx, |this, cx| this.chart_step(-1, cx)) {
                        eprintln!("Chart accessibility action: {error}");
                    }
                }
            })
            .aria_value(format!(
                "{}; {}",
                second.map_or_else(
                    || "Waiting".into(),
                    |second| format!("{}; Unix second {second}", clock(second, true))
                ),
                chart_reading(panel, panel.selected_index)
            ))
    }

    fn chart_keyboard(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(delta) = crate::dashboard::keyboard_step(event) else {
            return;
        };

        self.chart_step(delta, cx);
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

    pub(crate) fn chart_step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(snapshot) = self.context_snapshot() else {
            return;
        };

        let selection = KeyboardSelection {
            pending: self.request.second,
            accepted: snapshot.selection["second"].as_u64(),
        };
        let Some(next) = keyboard_second(&selection, delta, &snapshot.catalog) else {
            return;
        };

        if Some(next) != selection.pending.or(selection.accepted) {
            self.inspect(next, cx);
        }
    }

    fn chart_heading(panel: &Panel) -> impl IntoElement {
        let help = panel.help.clone().unwrap_or_else(|| {
            "Unavailable readings remain gaps. Click a chart to pause at that second.".into()
        });
        div()
            .flex()
            .justify_between()
            .items_center()
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(panel.title.clone()),
            )
            .child(control("ⓘ").tooltip(move |_, cx| cx.new(|_| ChartHelp(help.clone())).into()))
    }

    fn chart_ticks(chart: impl IntoElement, domain: (f32, f32)) -> impl IntoElement {
        let mut container = div().relative().child(chart);
        for tick in 0..=4 {
            let fraction = charts::number(tick) / 4.0;
            let value = domain.1 - (domain.1 - domain.0) * fraction;
            container = container.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(2.0 + fraction * 162.0))
                    .w(px(35.0))
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child(axis_reading(value)),
            );
        }
        container
    }

    fn chart_times(&self) -> impl IntoElement {
        let second = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.selection["second"].as_u64());
        let mut labels = div()
            .flex()
            .justify_between()
            .pl(px(40.0))
            .pr(px(10.0))
            .text_size(px(10.0))
            .text_color(rgb(MUTED));
        if let Some(second) = second {
            let first = second.saturating_sub(899);
            for stamp in [first, first + (second - first) / 2, second] {
                labels = labels.child(clock(stamp, false));
            }
        }
        div().child(labels).child(
            div()
                .h(px(18.0))
                .text_size(px(10.0))
                .text_color(rgb(MUTED))
                .children(
                    self.chart_hover
                        .as_ref()
                        .map(|hover| clock(hover.second, true)),
                ),
        )
    }

    fn chart_hidden(&self, panel: &Panel) -> Rc<Vec<String>> {
        Rc::new(
            panel
                .lines
                .iter()
                .take(16)
                .filter(|line| {
                    self.hidden
                        .contains(&format!("{}\t{}", panel.id, line.name))
                })
                .map(|line| line.name.clone())
                .collect(),
        )
    }

    fn chart_legends(
        &self,
        panel: &Panel,
        index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if panel.id == "live-memory-pressure" {
            return pressure_legend();
        }

        let mut legends = div().flex().flex_wrap().gap_x(px(12.0)).text_size(px(11.0));
        for line in panel.lines.iter().take(16) {
            let key = format!("{}\t{}", panel.id, line.name);
            let value = index.and_then(|index| line.values.get(index).copied().flatten());
            legends = legends.child(
                div()
                    .id(SharedString::from(key.clone()))
                    .accessibility_id(format!("flightdeck.legend.{}.{}", panel.id, line.name))
                    .role(gpui::Role::Switch)
                    .aria_label(format!("{}: {}", panel.title, line.name))
                    .aria_toggled(if self.hidden.contains(&key) {
                        gpui::Toggled::False
                    } else {
                        gpui::Toggled::True
                    })
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .focus(|style| style.bg(rgb(0x32_46_61)))
                    .py(px(4.0))
                    .opacity(if self.hidden.contains(&key) {
                        0.35
                    } else {
                        1.0
                    })
                    .text_color(rgb(charts::color(&line.color)))
                    .child(format!("━ {}  {}", line.name, reading(value, &panel.unit)))
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
}

struct ChartHelp(String);

struct KeyboardSelection {
    pending: Option<u64>,
    accepted: Option<u64>,
}

fn keyboard_second(selection: &KeyboardSelection, delta: i64, catalog: &Value) -> Option<u64> {
    let second = selection.pending.or(selection.accepted)?;
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
    snapshot: Option<&Snapshot>,
    bounds: Bounds<gpui::Pixels>,
    position: gpui::Point<gpui::Pixels>,
) -> Option<u64> {
    let rect = charts::plot_bounds(bounds);
    if !rect.contains(&position) {
        return None;
    }

    let fraction = f32::from(position.x - rect.origin.x) / f32::from(rect.size.width);
    crate::live_projection::sample_second(&snapshot?.selection, fraction)
}

fn chart_cursor(fraction: Option<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            if let Some(fraction) = fraction {
                let rect = charts::plot_bounds(bounds);
                let x = rect.origin.x + rect.size.width * fraction;
                window.paint_quad(fill(
                    Bounds::new(point(x, rect.origin.y), size(px(1.0), rect.size.height)),
                    charts::alpha(crate::charts::TEXT, 0.6),
                ));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn cursor_fraction(panel: &Panel, snapshot: &Snapshot, hover: Option<u64>) -> Option<f32> {
    let second = hover?;
    if let Some(index) = inspection_index(panel, snapshot, Some(second)) {
        return panel.positions.as_ref()?.get(index).copied();
    }
    let selected = snapshot.selection["second"].as_u64()?;
    let first = selected.saturating_sub(899);
    let end = selected.checked_add(1)?;
    if !(first..end).contains(&second) {
        return None;
    }
    let Ok(offset) = u16::try_from(second - first) else {
        return None;
    };
    let Ok(span) = u16::try_from(end - first) else {
        return None;
    };
    Some((f32::from(offset) + 0.5) / f32::from(span))
}

impl Render for ChartHelp {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
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
    let value = panel
        .lines
        .first()
        .and_then(|line| index.and_then(|index| line.values.get(index).copied().flatten()));
    if let Some(states) = &panel.states {
        return states
            .iter()
            .enumerate()
            .find(|(index, _)| value == Some(charts::number(*index)))
            .map_or_else(|| "Unavailable".to_owned(), |(_, state)| state.clone());
    }

    let unit = if panel.id == "live-memory" {
        "GiB active"
    } else {
        &panel.unit
    };
    reading(value, unit)
}

fn pressure_legend() -> gpui::Div {
    let mut legend = div().flex().flex_wrap().gap_x(px(12.0)).text_size(px(11.0));
    for (label, color) in [
        ("Normal", charts::GREEN),
        ("Warning", charts::ORANGE),
        ("Critical", charts::RED),
    ] {
        legend = legend.child(div().text_color(rgb(color)).child(format!("■ {label}")));
    }

    legend.child(div().text_color(rgb(MUTED)).child("Blank: unavailable"))
}

fn axis_reading(value: f32) -> String {
    if value.abs() < 1.0 {
        if value != 0.0 {
            return format!("{value:.2}");
        }
    }
    if value.abs() < 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    }
}

fn inspection_index(panel: &Panel, snapshot: &Snapshot, second: Option<u64>) -> Option<usize> {
    let Some(second) = second else {
        return panel.selected_index;
    };
    if snapshot.selection["second"].as_u64() == Some(second) {
        return panel.selected_index;
    }

    if panel.id == "live-memory-pressure" {
        let first = snapshot.selection["second"].as_u64()?.saturating_sub(899);
        let Ok(offset) = usize::try_from(second.checked_sub(first)?) else {
            return None;
        };
        return (offset < panel.lines.first()?.values.len()).then_some(offset);
    }

    let points = if panel.id.starts_with("live-app-") {
        snapshot.history.as_ref()?["points"].as_array()?
    } else {
        snapshot.selection["trends"].as_array()?
    };
    points
        .iter()
        .take(crate::data::LIVE_SAMPLE_LIMIT)
        .rposition(|point| {
            let stamp = point.get("observation").unwrap_or(point);
            wall_second(&stamp["wall_ns"]) == Some(second)
        })
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
    fn queued_keyboard_steps_advance_and_reverse_before_a_new_snapshot_arrives() {
        // Pending inspection remains the input until the worker publishes its accepted snapshot.
        let catalog = json!({"first_second":900,"last_second":1100});
        let mut selection = KeyboardSelection {
            pending: None,
            accepted: Some(1000),
        };
        selection.pending = keyboard_second(&selection, 1, &catalog);
        assert_eq!(keyboard_second(&selection, 1, &catalog), Some(1002));
        assert_eq!(keyboard_second(&selection, -1, &catalog), Some(1000));
        selection.pending = Some(1100);
        assert_eq!(keyboard_second(&selection, 1, &catalog), Some(1100));
        selection.pending = Some(900);
        assert_eq!(keyboard_second(&selection, -1, &catalog), Some(900));
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
    fn fractional_axis_labels_do_not_round_small_activity_to_zero() {
        // Preserve useful scale information when activity or power stays below one unit.
        assert_eq!(axis_reading(0.01), "0.01");
        assert_eq!(axis_reading(-0.05), "-0.05");
        assert_eq!(axis_reading(100.0), "100");
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
        let fraction = cursor_fraction(&panel, &snapshot, Some(999)).unwrap();
        assert!((fraction - 898.5 / 900.0).abs() < f32::EPSILON);
    }
}
