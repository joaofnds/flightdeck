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
use crate::live_dashboard::LiveDashboard;
use crate::live_presentation::clock;
use crate::live_source::Snapshot;

impl LiveDashboard {
    pub fn live_chart(&self, panel: &Rc<Panel>, cx: &mut Context<Self>) -> impl IntoElement {
        let upper = panel
            .lines
            .first()
            .map_or(0, |line| line.values.len().saturating_sub(1));
        let index = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| inspection_index(panel, snapshot, self.hover_second));
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
        let cursor = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| cursor_fraction(panel, snapshot, self.hover_second));
        let chart = Self::chart_events(
            div()
                .relative()
                .h(px(190.0))
                .child(plot)
                .child(chart_cursor(cursor)),
            measured,
            panel,
            cx,
        );
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
            .child(Self::chart_ticks(chart, domain))
            .child(self.chart_times())
            .child(self.chart_legends(panel, index, cx))
    }

    fn chart_headline(&self, panel: &Panel, index: Option<usize>) -> impl IntoElement {
        let mut headline = div()
            .flex()
            .items_baseline()
            .justify_between()
            .mt(px(12.0))
            .child(
                div()
                    .text_size(px(25.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(chart_reading(panel, index)),
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
        chart: gpui::Div,
        measured: Rc<Cell<Bounds<gpui::Pixels>>>,
        panel: &Panel,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let click_bounds = Rc::clone(&measured);
        chart
            .id(SharedString::from(format!("{}-plot", panel.id)))
            .focusable()
            .focus(|style| style.bg(rgb(0x20_30_41)))
            .on_key_down(cx.listener(Self::chart_keyboard))
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    let second = this.snapshot.as_ref().and_then(|snapshot| {
                        let rect = charts::plot_bounds(measured.get());
                        let fraction = f32::from(event.position.x - rect.origin.x)
                            / f32::from(rect.size.width);
                        crate::live_projection::sample_second(&snapshot.selection, fraction)
                    });
                    if this.hover_second != second {
                        this.hover_second = second;
                        cx.notify();
                    }
                }),
            )
            .on_hover(cx.listener(|this, hovered, _, cx| {
                if !hovered {
                    if this.hover_second.take().is_some() {
                        cx.notify();
                    }
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    let second = this.snapshot.as_ref().and_then(|snapshot| {
                        let rect = charts::plot_bounds(click_bounds.get());
                        let fraction = f32::from(event.position.x - rect.origin.x)
                            / f32::from(rect.size.width);
                        crate::live_projection::sample_second(&snapshot.selection, fraction)
                    });
                    if let Some(second) = second {
                        this.inspect(second, cx);
                    }
                }),
            )
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
        let Some(snapshot) = &self.snapshot else {
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
        window.prevent_default();
        cx.stop_propagation();
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
                .children(self.hover_second.map(|second| clock(second, true))),
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
        let mut legends = div().flex().flex_wrap().gap_x(px(12.0)).text_size(px(11.0));
        for line in panel.lines.iter().take(16) {
            let key = format!("{}\t{}", panel.id, line.name);
            let value = index.and_then(|index| line.values.get(index).copied().flatten());
            legends = legends.child(
                div()
                    .id(SharedString::from(key.clone()))
                    .cursor_pointer()
                    .focusable()
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
    .size_full()
}

fn cursor_fraction(panel: &Panel, snapshot: &Snapshot, hover: Option<u64>) -> Option<f32> {
    if let Some(index) = inspection_index(panel, snapshot, hover) {
        return panel.positions.as_ref()?.get(index).copied();
    }
    let second = hover?;
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
    let unit = if panel.id == "live-memory" {
        "GiB active"
    } else {
        &panel.unit
    };
    reading(value, unit)
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
    let points = if panel.id.starts_with("live-app-") {
        snapshot.history.as_ref()?["points"].as_array()?
    } else {
        snapshot.selection["trends"].as_array()?
    };
    points
        .iter()
        .take(crate::data::LIVE_SAMPLE_LIMIT)
        .position(|point| {
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
