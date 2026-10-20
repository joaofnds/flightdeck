use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    Bounds, Context, IntoElement, MouseButton, Pixels, Window, canvas, div, fill, point,
    prelude::*, px, rgb, size,
};
use num_traits::ToPrimitive;

use super::{CPUHistory, CPUInspection, CPUStep};
use crate::charts::{GRID, MUTED, PANEL, TEXT};
use crate::live_dashboard::LiveDashboard;
use crate::live_hover::{Origin, Target};

/// The prototype's heatmap: a 244 pixel plot whose rows share 204 of them, below a 10 pixel
/// margin, with the row labels to the left of the cells and the time labels under them.
const PLOT_HEIGHT: f32 = 244.0;
const PLOT_TOP: f32 = 10.0;
const PLOT_BOTTOM: f32 = 30.0;
const PLOT_RIGHT: f32 = 8.0;
const LABEL_COLUMN: f32 = 104.0;
/// A row label ends this far left of the cells.
const LABEL_GAP: f32 = 9.0;
const HOVER_SOURCE: &str = "cpu-history-plot";

impl LiveDashboard {
    pub fn cpu_history_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = div()
            .debug_selector(|| "cpu-heatmap-panel".into())
            .flex_1()
            .min_w_0()
            .px(px(12.0))
            .pt(px(8.0))
            .pb(px(8.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(crate::charts::LINE))
            .bg(rgb(PANEL))
            .child(crate::live_chart_view::timeline::timeline_heading_text(
                "Cores over time",
                "Busy scheduler ticks of each logical CPU over 15 minutes, grouped by the \
                 core level macOS places it in. Blank intervals are unavailable."
                    .into(),
                "ⓘ",
            ));
        let Some(history) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cpu_history.as_ref())
        else {
            return panel.child(self.history_absent("Logical CPU history unavailable."));
        };
        if history.cpus().is_empty() {
            return panel.child("Logical CPU history unavailable.");
        }

        panel.child(self.cpu_history_plot(history, cx)).child(
            div()
                .mt(px(7.0))
                .text_size(px(11.0))
                .line_height(px(17.0))
                .text_color(rgb(MUTED))
                .child("Green → yellow → red: 0–100% utilization."),
        )
    }

    fn cpu_history_plot(
        &self,
        history: &Arc<CPUHistory>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(focus) = self.cpu_history_focus.as_ref() else {
            unreachable!("Dashboard initializes the CPU history focus before rendering rows");
        };
        let time = crate::chart_axes::TimeWindow {
            first: history.wire.first_ns / super::NS,
            span: (history.wire.end_ns - history.wire.first_ns) / super::NS,
        };
        let plot = div()
            .id(HOVER_SOURCE)
            .debug_selector(|| HOVER_SOURCE.into())
            .track_focus(focus)
            .tab_stop(true)
            .focus(crate::charts::focus_ring)
            .relative()
            .h(px(PLOT_HEIGHT))
            .child(cpu_history_rows(
                history,
                self.cpu_history_inspection(history),
            ))
            .child(self.cpu_history_cells(history, cx))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(PLOT_HEIGHT - PLOT_BOTTOM))
                    .h(px(PLOT_BOTTOM))
                    .child(crate::chart_axes::time_labels_between(
                        Some(time),
                        px(LABEL_COLUMN),
                        px(PLOT_RIGHT),
                    )),
            )
            .on_key_down(cx.listener(Self::cpu_history_keyboard));

        self.cpu_history_accessibility(plot, history, cx)
    }

    fn cpu_history_cells(
        &self,
        history: &Arc<CPUHistory>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let measured = Rc::new(Cell::new(Bounds::default()));
        let inspection = self.cpu_history_inspection(history);
        let plot = cpu_history_cells_plot(history, inspection, Rc::clone(&measured));
        let move_history = Arc::clone(history);
        let move_bounds = Rc::clone(&measured);
        let click_history = Arc::clone(history);
        let click_bounds = Rc::clone(&measured);
        let hover_history = Arc::clone(history);
        div()
            .id("cpu-history-cells")
            .debug_selector(|| "cpu-history-cells".into())
            .absolute()
            .left(px(LABEL_COLUMN))
            .right(px(PLOT_RIGHT))
            .top(px(PLOT_TOP))
            .bottom(px(PLOT_BOTTOM))
            .child(plot)
            .children(self.reading_membership_observer(HOVER_SOURCE.into(), cx))
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    this.cpu_history_hover(
                        &move_history,
                        move_bounds.get(),
                        event.position,
                        Instant::now(),
                        cx,
                    );
                }),
            )
            .on_hover(cx.listener(move |this, hovered, window, cx| {
                if *hovered {
                    if this.pointer == crate::live_hover::controller::PointerPlace::OutsideWindow {
                        return;
                    }
                    this.cpu_history_hover(
                        &hover_history,
                        measured.get(),
                        window.mouse_position(),
                        Instant::now(),
                        cx,
                    );
                } else {
                    if this.reading_keyboard_hover_retained(measured.get(), window) {
                        return;
                    }
                    if this.cpu_history_matches_anchor(&hover_history) {
                        this.cpu_history_leave(cx);
                    }
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    if this.cpu_history_hover(
                        &click_history,
                        click_bounds.get(),
                        event.position,
                        Instant::now(),
                        cx,
                    ) {
                        this.cpu_history_select(window, cx);
                    }
                }),
            )
    }

    fn cpu_history_inspection(&self, history: &CPUHistory) -> Option<CPUInspection> {
        if let Some(target) = self.reading_target() {
            if self.cpu_history_owns_preview() {
                if self.cpu_inspection == Some(CPUInspection::Gap) {
                    return Some(CPUInspection::Gap);
                }
            }
            return Some(
                target
                    .identity
                    .as_ref()
                    .and_then(|identity| {
                        history.matching_inspection(identity, inspected_row(self.cpu_inspection))
                    })
                    .unwrap_or(CPUInspection::Gap),
            );
        }

        history.selected()
    }

    fn cpu_history_matches_anchor(&self, history: &Arc<CPUHistory>) -> bool {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cpu_history.as_ref())
            .is_some_and(|current| Arc::ptr_eq(current, history))
    }

    fn cpu_history_owns_preview(&self) -> bool {
        self.reading_target_source()
            .is_some_and(|source| source == &gpui::SharedString::from(HOVER_SOURCE))
    }

    fn cpu_history_leave(&mut self, cx: &mut Context<Self>) {
        let keyboard = self.reading_origin() == Some(Origin::Keyboard);
        if self.cpu_history_owns_preview() {
            if keyboard {
                return;
            }
        }

        self.clear_reading_target(&HOVER_SOURCE.into(), cx);
        if self.cpu_inspection.take().is_some() {
            cx.notify();
        }
    }

    fn cpu_history_accessibility(
        &self,
        plot: gpui::Stateful<gpui::Div>,
        history: &CPUHistory,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let inspection = self.cpu_history_inspection(history);
        plot.accessibility_id("flightdeck.cpu-history.plot")
            .role(gpui::Role::Slider)
            .aria_label("Logical CPU history")
            .aria_value(cpu_history_reading(history, inspection))
            .aria_description(
                "Left and Right inspect observations; Up and Down inspect CPUs; \
                Return selects the exact observation; Escape resets inspection; \
                blanks are unavailable",
            )
            .on_a11y_action(gpui::AccessibleAction::Increment, {
                let view = cx.entity().downgrade();
                move |_, window, cx| {
                    if let Err(error) = view.update(cx, |this, cx| {
                        this.cpu_history_accessible_step(1, window, cx);
                    }) {
                        eprintln!("CPU history accessibility action: {error}");
                    }
                }
            })
            .on_a11y_action(gpui::AccessibleAction::Decrement, {
                let view = cx.entity().downgrade();
                move |_, window, cx| {
                    if let Err(error) = view.update(cx, |this, cx| {
                        this.cpu_history_accessible_step(-1, window, cx);
                    }) {
                        eprintln!("CPU history accessibility action: {error}");
                    }
                }
            })
            .on_a11y_action(gpui::AccessibleAction::Click, {
                let view = cx.entity().downgrade();
                move |_, window, cx| {
                    if let Err(error) = view.update(cx, |this, cx| {
                        this.cpu_history_select(window, cx);
                    }) {
                        eprintln!("CPU history accessibility selection: {error}");
                    }
                }
            })
    }

    fn cpu_history_accessible_step(
        &mut self,
        observations: i8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(history) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cpu_history.as_ref())
        else {
            return;
        };
        self.cpu_inspection = history.step(
            self.cpu_history_inspection(history),
            CPUStep {
                observations,
                rows: 0,
            },
        );
        self.cpu_history_preview_keyboard(cx);
        if let Some(focus) = &self.cpu_history_focus {
            window.focus(focus, cx);
        }
        cx.notify();
    }

    pub(crate) fn cpu_history_hover(
        &mut self,
        history: &Arc<CPUHistory>,
        bounds: Bounds<Pixels>,
        position: gpui::Point<Pixels>,
        hovered_at: Instant,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };

        let Some(current) = snapshot.cpu_history.as_ref() else {
            return false;
        };

        if !Arc::ptr_eq(current, history) {
            return false;
        }

        if !bounds.contains(&position) {
            self.cpu_history_leave(cx);
            return false;
        }

        let fraction = f32::from(position.x - bounds.origin.x) / f32::from(bounds.size.width);
        let Some(rows) = history.cpus().len().to_f32() else {
            return false;
        };
        let row_height = f32::from(bounds.size.height) / rows;
        let row = (f32::from(position.y - bounds.origin.y) / row_height)
            .floor()
            .to_usize();
        let inspection = row
            .and_then(|row| history.inspect(row, f64::from(fraction)))
            .unwrap_or(CPUInspection::Gap);
        let target = if let Some((second, identity)) = history.target(inspection) {
            match Target::exact(second, &identity) {
                Ok(target) => target,
                Err(error) => {
                    eprintln!("CPU hover target unavailable: {error}");
                    self.cpu_history_leave(cx);
                    return false;
                }
            }
        } else {
            let Some(second) = crate::live_projection::sample_second(&snapshot.selection, fraction)
            else {
                return false;
            };

            Target::regular(snapshot, second)
        };
        let changed = self.cpu_inspection != Some(inspection);
        self.cpu_inspection = Some(inspection);
        self.hover_reading_target(HOVER_SOURCE.into(), target, hovered_at, cx);
        if changed {
            cx.notify();
        }
        true
    }

    fn cpu_history_preview_keyboard(&mut self, cx: &mut Context<Self>) {
        let target = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cpu_history.as_ref())
            .and_then(|history| {
                self.cpu_inspection
                    .and_then(|inspection| history.target(inspection))
            });
        let Some((second, identity)) = target else {
            self.end_reading_preview(cx);
            return;
        };

        match Target::exact(second, &identity) {
            Ok(target) => {
                self.set_reading_target(HOVER_SOURCE.into(), target, Origin::Keyboard, cx);
            }
            Err(error) => {
                eprintln!("CPU keyboard target unavailable: {error}");
                self.end_reading_preview(cx);
            }
        }
    }

    fn cpu_history_keyboard(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(history) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cpu_history.as_ref())
        else {
            return;
        };
        let inspection = self.cpu_history_inspection(history);
        self.cpu_inspection = match event.keystroke.key.as_str() {
            "left" => history.step(
                inspection,
                CPUStep {
                    observations: -1,
                    rows: 0,
                },
            ),
            "right" => history.step(
                inspection,
                CPUStep {
                    observations: 1,
                    rows: 0,
                },
            ),
            "up" => history.step(
                inspection,
                CPUStep {
                    observations: 0,
                    rows: -1,
                },
            ),
            "down" => history.step(
                inspection,
                CPUStep {
                    observations: 0,
                    rows: 1,
                },
            ),
            "home" => history.selected().map(|_| CPUInspection::Sample {
                row: inspected_row(inspection),
                observation_index: 0,
            }),
            "end" => history.selected().map(|_| CPUInspection::Sample {
                row: inspected_row(inspection),
                observation_index: history.observation_count() - 1,
            }),
            "escape" => None,
            "enter" => {
                self.cpu_history_select(window, cx);
                window.prevent_default();
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        self.cpu_history_preview_keyboard(cx);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    fn cpu_history_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(history) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cpu_history.as_ref())
        else {
            return;
        };
        let Some(inspection) = self.cpu_history_inspection(history) else {
            return;
        };
        let Some((second, identity)) = history.target(inspection) else {
            return;
        };
        self.request.second = Some(second);
        self.request.identity = Some(identity);
        self.request.offset = 0;
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.request.cpu_offset = 0;
        self.submit();
        self.focus_inspection(window, cx);
        window.prevent_default();
        cx.notify();
    }
}

/// The row labels, each as tall as its row of cells.
fn cpu_history_rows(history: &CPUHistory, inspection: Option<CPUInspection>) -> impl IntoElement {
    let row_height = row_height(history);
    let mut labels = div()
        .absolute()
        .left_0()
        .top(px(PLOT_TOP))
        .w(px(LABEL_COLUMN - LABEL_GAP))
        .flex()
        .flex_col()
        .items_end()
        .text_size(px(11.0))
        .text_color(rgb(TEXT));
    for (row, cpu) in history.cpus().iter().enumerate() {
        let value = match inspection {
            Some(CPUInspection::Sample {
                observation_index, ..
            }) => history.value(CPUInspection::Sample {
                row,
                observation_index,
            }),
            Some(CPUInspection::Gap) | None => None,
        };
        let value = match value {
            Some(value) => format!("{value:.0}%"),
            None => "Unavailable".into(),
        };
        labels = labels.child(
            div()
                .id(gpui::SharedString::from(format!(
                    "cpu-history-row-{}",
                    cpu.cpu()
                )))
                .accessibility_id(format!("flightdeck.cpu-history.row.{}", cpu.cpu()))
                .role(gpui::Role::Label)
                .aria_label(format!("Logical CPU {}", cpu.cpu()))
                .aria_value(value)
                .h(px(row_height))
                .line_height(px(row_height))
                .child(crate::live_inventory::logical_cpu_name(cpu.cpu())),
        );
    }

    labels
}

/// The rows share the plot's height between the margins.
fn row_height(history: &CPUHistory) -> f32 {
    let Some(rows) = history.cpus().len().to_f32() else {
        unreachable!("Validated CPU history has at most 32 rows");
    };
    assert!(rows > 0.0);

    (PLOT_HEIGHT - PLOT_TOP - PLOT_BOTTOM) / rows
}

fn inspected_row(inspection: Option<CPUInspection>) -> usize {
    match inspection {
        Some(CPUInspection::Sample { row, .. }) => row,
        Some(CPUInspection::Gap) | None => 0,
    }
}

fn cpu_history_reading(history: &CPUHistory, inspection: Option<CPUInspection>) -> String {
    let Some(CPUInspection::Sample {
        row,
        observation_index,
    }) = inspection
    else {
        return "No measurement interval here.".into();
    };

    let Some(cpu) = history.cpus().get(row) else {
        return "Logical CPU unavailable".into();
    };

    let Some(sample) = history.wire.observations.get(observation_index) else {
        return "CPU observation unavailable".into();
    };

    let reading = match history.value(CPUInspection::Sample {
        row,
        observation_index,
    }) {
        Some(value) => format!("{value:.0}%"),
        None => "Unavailable".into(),
    };
    format!(
        "Logical CPU {} · {} · {}.{:03} · publication {}",
        cpu.cpu(),
        reading,
        crate::live_presentation::clock(sample.observation.wall_ns / super::NS, true),
        sample.observation.wall_ns % super::NS / 1_000_000,
        sample.observation.publication
    )
}

fn cpu_history_paint(
    history: &CPUHistory,
    inspection: Option<CPUInspection>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    let row_height = row_height(history);
    for (row, cpu) in history.cpus().iter().enumerate() {
        let Some(row) = row.to_f32() else {
            unreachable!("Validated CPU history has at most 32 rows");
        };
        // The prototype insets each cell half a pixel from its row's top and leaves 0.7 below.
        let top = bounds.origin.y + px(row * row_height + 0.5);
        let height = px((row_height - 0.7).max(1.0));
        window.paint_quad(fill(
            Bounds::new(point(bounds.origin.x, top), size(bounds.size.width, height)),
            rgb(GRID),
        ));
        for interval in &history.wire.intervals {
            if let Some(value) = cpu.busy_percent()[interval.observation_index] {
                let first = history.fraction(interval.first_ns).to_f32();
                let end = history.fraction(interval.end_ns).to_f32();
                let (Some(first), Some(end)) = (first, end) else {
                    unreachable!("Validated CPU history fractions fit f32");
                };
                let rectangle = Bounds::new(
                    point(bounds.origin.x + bounds.size.width * first, top),
                    size(bounds.size.width * (end - first), height),
                );
                window.paint_quad(fill(rectangle, crate::charts::utilization_color(value)));
            }
        }
    }
    let inspected = match inspection {
        Some(CPUInspection::Sample {
            observation_index, ..
        }) => history
            .wire
            .observations
            .get(observation_index)
            .map(|sample| sample.observation.wall_ns),
        Some(CPUInspection::Gap) | None => None,
    };
    cpu_history_cursor(
        history,
        inspected.unwrap_or(history.wire.observation.wall_ns),
        bounds,
        window,
    );
}

/// The cursor's dark edge and light core. Cells run from light green to red and the plot
/// behind a gap is dark, so neither colour alone holds 3:1 against both.
pub(crate) const CURSOR_EDGE: u32 = 0x0011_1217;
pub(crate) const CURSOR_EDGE_OPACITY: f32 = 0.9;
pub(crate) const CURSOR_CORE: u32 = 0x00ff_ffff;
const CURSOR_EDGE_WIDTH: f32 = 3.0;
const CURSOR_CORE_WIDTH: f32 = 1.0;

/// A solid white line with a dark edge on each side, so it reads over every cell colour.
fn cpu_history_cursor(
    history: &CPUHistory,
    wall_ns: u64,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    let Some(fraction) = history.fraction(wall_ns).to_f32() else {
        unreachable!("Validated CPU history fractions fit f32");
    };
    let center = bounds.origin.x + bounds.size.width * fraction;
    for (color, width) in [
        (
            crate::charts::alpha(CURSOR_EDGE, CURSOR_EDGE_OPACITY),
            CURSOR_EDGE_WIDTH,
        ),
        (rgb(CURSOR_CORE), CURSOR_CORE_WIDTH),
    ] {
        window.paint_quad(fill(
            Bounds::new(
                point(center - px(width / 2.0), bounds.origin.y),
                size(px(width), bounds.size.height),
            ),
            color,
        ));
    }
}

/// The history's cells, painted at the measured bounds, which it records for the pointer.
fn cpu_history_cells_plot(
    history: &Arc<CPUHistory>,
    inspection: Option<CPUInspection>,
    measured: Rc<Cell<Bounds<Pixels>>>,
) -> impl IntoElement + use<> {
    let paint_history = Arc::clone(history);
    canvas(
        move |bounds, _, _| measured.set(bounds),
        move |bounds, (), window, _| {
            cpu_history_paint(&paint_history, inspection, bounds, window);
        },
    )
    .size_full()
}
