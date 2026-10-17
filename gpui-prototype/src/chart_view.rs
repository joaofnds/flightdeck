use std::cell::Cell;
use std::rc::Rc;

use gpui::{Bounds, Context, IntoElement, SharedString, div, prelude::*, px, rgb};

use crate::charts::{self, Inspection, MUTED};
use crate::dashboard::Dashboard;
use crate::data::{Panel, reading};

impl Dashboard {
    pub fn chart(&self, panel: &Rc<Panel>, cx: &mut Context<Self>) -> impl IntoElement {
        let inspection = self.chart_inspection(panel);
        let bounds = Rc::new(Cell::new(Bounds::default()));
        let hidden = self.chart_hidden(panel);
        let (minimum, maximum) = charts::domain(panel, inspection, &hidden);
        let height = if panel.kind == "timeline" {
            96.0
        } else {
            220.0
        };
        let mut chart = div().relative().w_full().h(px(height)).child(charts::plot(
            Rc::clone(panel),
            inspection,
            hidden,
            Rc::clone(&bounds),
        ));
        chart = Self::chart_ticks(chart, minimum, maximum);
        chart = self.chart_axes(chart, panel, inspection);
        chart = Self::chart_state(chart, panel, inspection);
        let is_diagnostic = panel.kind == "self-chart";
        let chart = chart.on_mouse_move(cx.listener(
            move |this, event: &gpui::MouseMoveEvent, _, cx| {
                let rect = charts::plot_bounds(bounds.get());
                let fraction =
                    f32::from(event.position.x - rect.origin.x) / f32::from(rect.size.width);
                let selected = inspection.select_fraction(fraction);
                if is_diagnostic {
                    if this.diagnostic_selected != selected {
                        this.diagnostic_selected = selected;
                        cx.notify();
                    }
                } else {
                    if this.inspection.selected != selected {
                        this.inspection.selected = selected;
                        cx.notify();
                    }
                }
            },
        ));
        let label = if is_diagnostic {
            "Separate diagnostic capture".into()
        } else {
            self.capture.time(inspection.lower)
        };
        let end = if is_diagnostic {
            "Elapsed seconds".into()
        } else {
            self.capture.time(inspection.upper)
        };
        div()
            .child(chart)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child(label)
                    .child(end),
            )
            .child(self.chart_legend(panel, inspection, cx))
    }

    fn chart_inspection(&self, panel: &Panel) -> Inspection {
        if panel.kind == "self-chart" {
            let upper = panel
                .lines
                .first()
                .map_or(1, |line| line.values.len())
                .saturating_sub(1);
            Inspection {
                lower: 0,
                selected: self.diagnostic_selected.min(upper),
                upper,
            }
        } else {
            self.inspection
        }
    }

    fn chart_hidden(&self, panel: &Panel) -> Rc<Vec<String>> {
        let mut hidden = Rc::new(
            panel
                .lines
                .iter()
                .filter(|line| {
                    self.hidden
                        .contains(&format!("{}\t{}", panel.id, line.name))
                })
                .map(|line| line.name.clone())
                .collect::<Vec<_>>(),
        );
        if panel.id.starts_with("process") {
            if let Some(process) = &self.process {
                for line in &panel.lines {
                    let suffix = line.name.strip_prefix(process);
                    if !matches!(
                        suffix,
                        Some("" | " received" | " sent" | " read" | " written")
                    ) {
                        Rc::make_mut(&mut hidden).push(line.name.clone());
                    }
                }
            }
        }
        hidden
    }

    fn chart_ticks(mut chart: gpui::Div, minimum: f32, maximum: f32) -> gpui::Div {
        for tick in 0..=4 {
            chart = chart.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(2.0 + charts::number(tick) * 48.0))
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "{:.0}",
                        maximum - (maximum - minimum) * charts::number(tick) / 4.0
                    )),
            );
        }
        chart
    }

    fn chart_axes(&self, mut chart: gpui::Div, panel: &Panel, inspection: Inspection) -> gpui::Div {
        let hidden = self.chart_hidden(panel);
        let mut axes = Vec::with_capacity(4);
        for line in &panel.lines {
            if let Some(axis) = &line.axis {
                if axis == "hidden" {
                    continue;
                }
                if axes.contains(&axis.as_str()) {
                    continue;
                }
                let (minimum, maximum) = charts::axis_domain(panel, inspection, &hidden, axis);
                chart = chart.child(
                    div()
                        .absolute()
                        .right_0()
                        .top(px(2.0 + charts::number(axes.len()) * 20.0))
                        .text_size(px(9.0))
                        .text_color(rgb(charts::color(&line.color)))
                        .child(format!(
                            "{minimum:.0}–{maximum:.0} {}",
                            line.unit.as_deref().unwrap_or(&panel.unit)
                        )),
                );
                axes.push(axis.as_str());
            }
        }
        chart
    }

    fn chart_state(mut chart: gpui::Div, panel: &Panel, inspection: Inspection) -> gpui::Div {
        if let Some(states) = &panel.states {
            let value = panel
                .lines
                .first()
                .and_then(|line| line.values.get(inspection.selected))
                .copied()
                .flatten();
            let label = states
                .iter()
                .enumerate()
                .find(|(index, _)| value == Some(charts::number(*index)))
                .map_or("Unavailable", |(_, label)| label.as_str());
            chart = chart.child(
                div()
                    .absolute()
                    .left(px(42.0))
                    .top(px(50.0))
                    .text_size(px(18.0))
                    .child(label.to_owned()),
            );
        }
        chart
    }

    fn chart_legend(
        &self,
        panel: &Panel,
        inspection: Inspection,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut legend = div().flex().flex_wrap().gap_x(px(10.0)).text_size(px(11.0));
        for line in &panel.lines {
            let name = line.name.clone();
            let key = format!("{}\t{name}", panel.id);
            let value = line.values.get(inspection.selected).copied().flatten();
            let opacity = if self.hidden.contains(&key) {
                0.35
            } else {
                1.0
            };
            legend = legend.child(
                div()
                    .id(SharedString::from(format!("{}-{name}", panel.id)))
                    .cursor_pointer()
                    .py(px(5.0))
                    .opacity(opacity)
                    .text_color(rgb(charts::color(&line.color)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let hidden = Rc::make_mut(&mut this.hidden);
                        if hidden.contains(&key) {
                            hidden.retain(|value| value != &key);
                        } else {
                            hidden.push(key.clone());
                        }
                        cx.notify();
                    }))
                    .child(format!(
                        "━ {}  {}",
                        line.name,
                        reading(value, line.unit.as_deref().unwrap_or(&panel.unit))
                    )),
            );
        }
        legend
    }
}
